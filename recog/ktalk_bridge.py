#!/usr/bin/env python3
"""
KTalk Bridge - WebSocket server для приёма данных о спикерах из Chrome extension.

Запускается параллельно с live транскрипцией и предоставляет real-time
информацию о том, кто сейчас говорит в KTalk конференции.

Использование:
    # Standalone (для тестирования)
    python -m recog.ktalk_bridge --port 8766

    # Как модуль из recog/cli.py
    from recog.ktalk_bridge import KTalkBridge
    bridge = KTalkBridge()
    bridge.start()  # Запускает в фоновом потоке

    # Получение текущего спикера
    speaker = bridge.get_current_speaker()
"""

import asyncio
import base64
import binascii
import collections
import json
import threading
import time
from dataclasses import dataclass, field
from datetime import datetime
from typing import Optional, Dict, List, Callable, TYPE_CHECKING
import sys
from pathlib import Path
from recog.config import load_dotenv, MIC_SPEAKER_NAME, get_data_dir
from recog.ai_settings import AiSettingsStore
from recog.log_utils import log_rate_limited
from recog.ws_origin import origin_allowed
from recog.meeting_storage import is_conference_source
from recog.user_profile import resolve_identity

if TYPE_CHECKING:
    from recog.meeting_storage import MeetingStorage

load_dotenv()

# Proactive transcript-tick thresholds for the Copilot channel. Either
# trigger fires a `transcript_tick` frame to channel clients (if any) so
# Claude can decide whether to surface a proactive note. Numbers here are
# bridge-side ratelimits — the extension still gates the resulting
# `copilot_reply` on its own 60 s UI cooldown and the user mute toggle.
COPILOT_TICK_NEW_SEGMENTS = 5      # at least N new buffered segments
COPILOT_TICK_INTERVAL_SECONDS = 15.0  # OR at least T seconds since the last tick
COPILOT_TICK_RECENT_SEGMENTS = 10  # how many trailing segments to include as context

# Copilot auto-session (recog/copilot_supervisor.py) agent-state merge (D4).
# The supervisor reports the TUI screen state and the copilot channel daemon
# reports its live MCP-session count; the bridge merges them into one
# effective chip status broadcast to extensions.
# The supervisor repeats its status as an idempotent heartbeat every ~5 s;
# two missed heartbeats (>10 s of silence) mean it crashed → offline.
AGENT_SILENCE_THRESHOLD_S = 10.0
# `claude_sessions == 0` during an active meeting means "TUI up but copilot
# never connected". Debounced so a supervisor leaving `spawning` before the
# daemon's first `claude_sessions` poll (5 s) cannot flap the chip to offline.
AGENT_SESSIONS_ZERO_DEBOUNCE_S = 10.0
AGENT_CHANNEL_RECOVERY_S = 60.0

# Synthetic lifecycle id for a manually spawned copilot session (webapp
# "Recog Copilot" button). A constant — not a per-click uuid — so a flap or a
# supervisor reconnect maps to the SAME id and the supervisor's GRACE revival
# / adopt paths recognise the session instead of killing and respawning it.
MANUAL_MEETING_ID = "manual"

# Upper bound for a single fan-out `send()` (DW-6). WebSocket flow-control
# backpressure blocks `send` without raising, so one stalled recipient would
# otherwise freeze every broadcast loop for the recipients queued behind it.
# Read at call time so tests can monkeypatch it.
BROADCAST_SEND_TIMEOUT_S = 5.0

@dataclass
class SpeakerEvent:
    """Событие смены спикера."""
    speaker_name: Optional[str]  # None если никто не говорит
    speaker_id: str
    timestamp: float
    is_local: bool = False


@dataclass
class SpeakerSegment:
    """Завершённый сегмент речи спикера."""
    speaker_name: str
    speaker_id: str
    start_time: float  # Unix timestamp в секундах
    end_time: float    # Unix timestamp в секундах
    duration_ms: int   # Длительность в миллисекундах
    is_local: bool = False


@dataclass
class Participant:
    """Участник конференции."""
    id: str
    name: str
    is_local: bool = False
    mic_muted: bool = True


@dataclass
class PcmChunk:
    """Чанк Int16 PCM от одного участника KTalk-конференции.

    Поля соответствуют WS-протоколу `type: "pcm_chunk"` (см. design.md).
    `pcm` — decoded Int16LE bytes (sample_count = len(pcm) // 2).
    Timestamp'ы уже скорректированы на clock_offset (см. `_process_message`).
    """
    pid: str
    speaker_name: str
    is_local: bool
    seq: int
    chunk_start_ts: float  # Unix timestamp (секунды) — начало окна, с учётом clock_offset
    chunk_end_ts: float    # Unix timestamp (секунды) — конец окна, с учётом clock_offset
    sample_rate: int
    pcm: bytes


class KTalkBridge:
    """
    WebSocket сервер для приёма данных о спикерах из KTalk Chrome extension.
    """

    # Время (сек) в течение которого помним последнего спикера после того как он замолчал
    # Должно быть >= chunk_duration чтобы компенсировать задержку аудио capture
    SPEAKER_MEMORY_SECONDS = 0.8

    # Максимальная длительность сегмента (сек) - если спикер говорит дольше, разбиваем на части
    MAX_SEGMENT_DURATION = 20.0

    def __init__(
        self, host: str = "localhost", port: int = 8766, *, ai_settings_store=None, ktalk_only=False
    ):
        self.host = host
        self.port = port
        self._current_speaker: Optional[str] = None
        self._current_speaker_id: Optional[str] = None
        self._current_is_local: bool = False  # Текущий спикер - локальный?
        self._is_speaking: bool = False
        self._last_speaker: Optional[str] = None  # Последний говоривший
        self._last_speaker_time: float = 0  # Когда последний раз кто-то говорил
        self._last_is_local: bool = False  # Последний спикер был локальным?
        self._participants: Dict[str, Participant] = {}
        self._speaker_history: List[SpeakerEvent] = []
        self._lock = threading.Lock()
        self._user_identity = resolve_identity(
            {"name": "", "aliases": [], "revision": "unconfigured"},
            legacy_name=MIC_SPEAKER_NAME,
        )
        self._read_user_identity = None
        self._apply_mic_identity = None
        self._user_profile_refresh_lock = asyncio.Lock()
        self._ai_settings_refresh_lock = asyncio.Lock()
        self._ai_settings_store = ai_settings_store or AiSettingsStore(get_data_dir())
        self._copilot_policy = self._read_copilot_policy()
        self._server = None
        self._thread: Optional[threading.Thread] = None
        self._running = False
        self._loop = None
        # Strong references to fire-and-forget `close()` tasks scheduled by
        # `_send_bounded`: the event loop holds tasks only weakly, so a
        # pending close could be garbage-collected before it runs.
        self._bg_close_tasks: set = set()
        # Clients whose bounded send already timed out. Keep their role-set
        # membership until `_handle_client` reaps the closed socket so its
        # role-specific disconnect cleanup still runs, while later broadcasts
        # fail fast instead of paying the timeout again.
        self._timed_out_clients: set = set()
        self._connected_clients = set()
        # Subscriber clients (e.g. frontend `BridgeSubscriber`) connect as
        # read-only listeners. They do NOT participate in the KTalk meeting
        # lifecycle: their presence must not block the
        # "last extension disconnect -> end meeting" path, and they do not
        # send clock_sync/speaker_update so we do not reset _clock_offset
        # on their connect. Populated when a client sends `subscriber_hello`.
        self._subscriber_clients = set()
        # Copilot channel clients (recog/copilot_channel.py) connect as the
        # bidirectional bridge between extension chat frames and a Claude
        # Code MCP session. Like subscribers, they are isolated from the
        # KTalk meeting lifecycle: their presence must not block
        # "last extension disconnect -> end meeting". Populated when a
        # client sends `copilot_channel_hello`.
        self._copilot_channel_clients: set = set()

        # Copilot supervisor client (recog/copilot_supervisor.py) connects as
        # the auto-session lifecycle driver. Like channel/subscriber clients it
        # is isolated from the KTalk meeting lifecycle (its presence must not
        # block "last extension disconnect -> end meeting"). Populated when a
        # client sends `supervisor_hello`. Meeting `meeting_started` /
        # `meeting_ended` frames fan out to it via `_emit_lifecycle`.
        self._supervisor_clients: set = set()

        # Manual copilot session mirror (D5). True while a manual (non-
        # conference) session should be alive: armed when a
        # `copilot_spawn_request` is actually relayed to a supervisor, cleared
        # on `copilot_stop_request` and on conference preemption, re-armed from
        # `supervisor_hello.current_meeting_id == MANUAL_MEETING_ID`.
        #
        # Single-owner semantics: the SUPERVISOR owns manual-session truth (it
        # physically holds the tmux session); this flag is only the bridge's
        # mirror, so `_compute_meeting_state` can answer "adopt" instead of
        # "kill" on a reconnect. Deliberately NOT persisted — recovery after a
        # bridge restart is `supervisor_hello`-driven only, so the bridge can
        # never claim a session that no longer exists.
        #
        # Unlike the client sets around it, reads need no threading lock:
        # every access is confined to the asyncio loop thread. Manual
        # spawn/stop operations still need an asyncio lock because handlers
        # for different subscriber sockets can interleave while awaiting a
        # supervisor send.
        self._manual_copilot_active: bool = False
        self._manual_copilot_request_lock = asyncio.Lock()

        # Agent-state merge (D4). Last status reported by the supervisor
        # (screen classification) and by the copilot channel daemon (live MCP
        # session count), reduced to one effective chip status. `None` status
        # = no supervisor report yet; `None` session count = unknown (SDK
        # private attr missing) — merge must not treat it as a confident 0.
        self._supervisor_agent_status: Optional[str] = None
        self._supervisor_status_ts: float = 0.0  # time.monotonic of last report
        self._channel_recovery_sent_at: Optional[float] = None
        self._claude_sessions_count: Optional[int] = None
        self._sessions_zero_since: Optional[float] = None  # when count first hit 0
        # Last effective status broadcast to extensions. Seeded to "offline"
        # so a boot-time recompute with no supervisor is a no-op (the chip
        # already defaults to offline) — only real transitions broadcast.
        self._last_effective_agent_state: str = "offline"

        # Tracks `copilot_chat_message` request_id -> meeting_id at the
        # moment the request was forwarded to channel clients. On
        # `meeting_leave` the value is prefixed with `"stale:"` so any
        # late `copilot_channel_reply` / `copilot_channel_status` arriving
        # after the meeting has ended is dropped instead of being relayed
        # to extension clients (which now belong to the next meeting).
        self._copilot_pending_requests: Dict[str, str] = {}

        # Recent transcript buffer keyed by segment_id, used by the
        # `get_recent_transcript` MCP tool exposed by `recog/copilot_channel.py`.
        # The deque preserves insertion order with a hard cap; on overflow the
        # oldest segment_id is also evicted from the dict so memory stays
        # bounded across long meetings.
        self._copilot_transcript_buffer: Dict[int, dict] = {}
        self._copilot_transcript_order: collections.deque = collections.deque(maxlen=300)

        # Proactive transcript-tick state. Only populated while a meeting is
        # active so the trigger in `send_transcript` is a cheap None-check
        # outside meetings. Reset on `meeting_join` and cleared on
        # `meeting_leave` so a tick cannot reference a stale meeting_id.
        self._copilot_tick_state: Optional[dict] = None

        # Отслеживание активного сегмента речи
        self._segment_start_time: Optional[float] = None
        self._segment_speaker_name: Optional[str] = None
        self._segment_speaker_id: Optional[str] = None
        self._segment_is_local: bool = False

        # Очередь завершённых сегментов для обработки
        self._completed_segments: List[SpeakerSegment] = []

        # Флаг для логирования первого спикера
        self._first_speaker_logged = False

        # Sequence number для обнаружения out-of-order сообщений
        self._last_seq: int = 0

        # Clock offset для синхронизации JS и Python часов
        self._clock_offset: float = 0.0

        # Callbacks для уведомления о событиях
        self._on_speaker_change: Optional[Callable[[Optional[str], bool], None]] = None
        self._on_segment_complete: Optional[Callable[[SpeakerSegment], None]] = None
        self.ktalk_only = ktalk_only
        self._on_meeting_capture: Optional[Callable[[bool], None]] = None
        self._recording_active_getter: Optional[Callable[[], bool]] = None
        self._on_meeting_change: Optional[Callable[[str, Path], None]] = None
        self._on_pre_meeting_switch: Optional[Callable[[], None]] = None
        self._on_pcm_chunk: Optional[Callable[[PcmChunk], None]] = None
        # Optional callback returning a snapshot of PerSpeakerIngest stats.
        # LiveTranscriber wires it; bridge surfaces the result through the
        # debug file so frontend /api/pcm-diagnostics can read it.
        self._pcm_stats_getter: Optional[Callable[[], dict]] = None
        # Last browser-side PCM/WebRTC transport snapshot sent by the
        # extension (`pcm_transport_stats`). Kept separate from PerSpeakerIngest
        # stats because it describes browser graph / RTP health, not Python ASR.
        self._pcm_transport_stats: dict = {}
        # Per-pid throttle for cross-meeting pcm_chunk drop logs. A sibling
        # KTalk tab fires ~one chunk per speaker per 3 s, so logging every
        # drop floods stderr — keyed by pid, value is the last log timestamp.
        self._pcm_cross_meeting_drop_log_ts: Dict[str, float] = {}
        # Extension WS that owns the currently active meeting. Used to
        # reject `meeting_join` from a *second* simultaneously-open KTalk
        # tab: there is exactly ONE audio capture pipeline + storage, so
        # allowing a sibling tab to silently flip `current_meeting_id`
        # leaves the original tab stranded — its `state.currentMeetingId`
        # is now stale and `handleTranscript`'s meeting_id filter drops
        # every new segment. Cleared when the owner WS disconnects or the
        # owner sends `meeting_leave`.
        self._meeting_owner_ws = None

        # Meeting storage для сохранения транскриптов
        self._meeting_storage: Optional['MeetingStorage'] = None

        # IncrementGenerator для автогенерации инкрементов при выходе из встречи
        self._increment_generator = None
        self._background_tasks: set = set()

        # Debug state file
        self._debug_file = Path("/tmp/ktalk_debug.json")
        self._debug_update_interval = 1.0  # seconds
        self._last_debug_update = 0.0

    def _clear_speaker_state(self):
        """Clear all speaker/participant state under self._lock.

        Call when a meeting ends, a new meeting starts, or the last client
        disconnects -- anywhere stale speaker data must not leak.
        """
        with self._lock:
            self._participants = {}
            self._is_speaking = False
            self._current_speaker = None
            self._current_speaker_id = None
            self._last_speaker = None
            self._last_speaker_time = 0
            self._last_is_local = False
            self._speaker_history.clear()
            self._segment_start_time = None
            self._segment_speaker_name = None
            self._segment_speaker_id = None
            self._pcm_transport_stats = {}

    def set_speaker_callback(self, callback: Callable[[Optional[str], bool], None]):
        """
        Устанавливает callback для уведомления о смене спикера.

        Args:
            callback: функция (speaker_name: Optional[str], is_speaking: bool)
        """
        self._on_speaker_change = callback

    def set_segment_callback(self, callback: Callable[['SpeakerSegment'], None]):
        """
        Устанавливает callback для уведомления о завершении сегмента речи.

        Args:
            callback: функция (segment: SpeakerSegment)
        """
        self._on_segment_complete = callback

    def set_meeting_change_callback(self, callback: Callable[[str, Path], None]):
        """
        Устанавливает callback для уведомления о смене встречи.

        Args:
            callback: функция (meeting_id: str, meeting_path: Path)
        """
        self._on_meeting_change = callback

    def set_meeting_capture_callback(
        self, callback: Callable[[bool], None], *, active_getter: Callable[[], bool]
    ):
        """Notify live capture only after an accepted join, and before any leave."""
        self._on_meeting_capture = callback
        self._recording_active_getter = active_getter

    def is_recording_active(self) -> bool:
        """Read the capture owner's state without maintaining a bridge mirror."""
        if self._recording_active_getter is not None:
            return self._recording_active_getter()
        return not self.ktalk_only

    def _set_meeting_capture(self, active: bool):
        if self._on_meeting_capture is not None:
            self._on_meeting_capture(active)
        elif active and self.ktalk_only:
            raise RuntimeError("No capture controller registered")

    def set_pre_meeting_switch_callback(self, callback: Callable[[], None]):
        """
        Устанавливает callback, вызываемый ДО переключения встречи в MeetingStorage.
        Используется для инвалидации in-flight чанков перед сменой meeting path.
        """
        self._on_pre_meeting_switch = callback

    def set_pcm_stats_getter(self, getter: Optional[Callable[[], dict]]) -> None:
        """Wire a getter returning PerSpeakerIngest.get_stats() for debug."""
        self._pcm_stats_getter = getter

    def set_pcm_chunk_callback(self, callback: Optional[Callable[['PcmChunk'], None]]):
        """
        Устанавливает callback для приёма PCM-чанков от extension'а.

        Args:
            callback: функция (chunk: PcmChunk) — вызывается синхронно из
                asyncio-потока bridge'а для каждого валидного `pcm_chunk`
                сообщения. None — отключить callback.
        """
        self._on_pcm_chunk = callback

    def set_meeting_storage(self, storage: 'MeetingStorage'):
        """
        Устанавливает MeetingStorage для сохранения транскриптов.

        Args:
            storage: экземпляр MeetingStorage
        """
        self._meeting_storage = storage

    def set_increment_generator(self, generator):
        """Set IncrementGenerator for auto-generating increments on meeting leave."""
        self._increment_generator = generator

    def _has_active_ktalk_meeting(self) -> bool:
        """Return True only while current storage points at a conference meeting."""
        if not self._meeting_storage or not self._meeting_storage.current_meeting_path:
            return False
        try:
            meta = self._meeting_storage._load_meta()
        except Exception as e:
            print(f"[KTalk Bridge] pcm_chunk meeting guard failed: {e}", file=sys.stderr)
            return False
        # Widened from ktalk-only to every conference source: a no-op for
        # Tier-1 matrix (no PCM without a worklet) but keeps the PCM gate
        # consistent and forward-compatible with Tier-2 per-participant PCM.
        return bool(meta and is_conference_source(meta.get("source")))

    async def _safe_generate_increment(self, meeting_id: str, meeting_path):
        """Wrapper for increment generation that logs unexpected errors."""
        try:
            await self._increment_generator.generate_all(meeting_id, meeting_path)
        except Exception as e:
            print(f"[KTalk Bridge] increment generation error: {e}", file=sys.stderr)

    def get_completed_segment(self) -> Optional['SpeakerSegment']:
        """Возвращает и удаляет первый завершённый сегмент из очереди."""
        with self._lock:
            if self._completed_segments:
                return self._completed_segments.pop(0)
            return None

    def get_all_completed_segments(self) -> List['SpeakerSegment']:
        """Возвращает и очищает все завершённые сегменты."""
        with self._lock:
            segments = self._completed_segments.copy()
            self._completed_segments.clear()
            return segments

    def _get_current_speaker_unlocked(self) -> Optional[str]:
        """
        Возвращает имя текущего говорящего (без захвата lock).

        Должна вызываться только когда self._lock уже захвачен.
        """
        if self._is_speaking and self._current_speaker:
            return self._current_speaker

        # Если никто не говорит, проверяем "память" о последнем спикере
        if self._last_speaker:
            elapsed = time.time() - self._last_speaker_time
            if elapsed < self.SPEAKER_MEMORY_SECONDS:
                return self._last_speaker

        return None

    def get_current_speaker(self) -> Optional[str]:
        """
        Возвращает имя текущего говорящего.

        Если никто не говорит, но кто-то говорил недавно (в пределах SPEAKER_MEMORY_SECONDS),
        возвращает имя последнего говорившего. Это нужно для компенсации задержки
        между аудио capture и WebSocket событиями.
        """
        with self._lock:
            return self._get_current_speaker_unlocked()

    def get_dominant_speaker_for_period(self, start_time: float, end_time: float) -> Optional[str]:
        """
        Возвращает спикера, который говорил больше всего в заданном временном окне.

        Args:
            start_time: начало окна (Unix timestamp в секундах)
            end_time: конец окна (Unix timestamp в секундах)

        Returns:
            Имя спикера с наибольшим временем речи в окне, или None
        """
        with self._lock:
            # Находим события, которые могли влиять на окно
            # (включая последнее событие до start_time, т.к. оно задаёт начального спикера)
            relevant_events = [e for e in self._speaker_history
                               if e.timestamp <= end_time]

            if not relevant_events:
                return self._get_current_speaker_unlocked()

            # Считаем длительность речи каждого спикера в окне
            speaker_durations: Dict[str, float] = {}

            for i, event in enumerate(relevant_events):
                if event.speaker_name is None:
                    continue

                event_start = max(event.timestamp, start_time)
                # Событие заканчивается при следующем событии или в конце окна
                if i + 1 < len(relevant_events):
                    event_end = min(relevant_events[i + 1].timestamp, end_time)
                else:
                    event_end = end_time

                if event_end > event_start:
                    duration = event_end - event_start
                    speaker_durations[event.speaker_name] = (
                        speaker_durations.get(event.speaker_name, 0) + duration
                    )

            if not speaker_durations:
                return self._get_current_speaker_unlocked()

            # Возвращаем спикера с наибольшим временем речи
            return max(speaker_durations, key=speaker_durations.get)

    def is_someone_speaking(self) -> bool:
        """Возвращает True если кто-то говорит."""
        with self._lock:
            return self._is_speaking

    def is_current_speaker_local(self) -> bool:
        """Возвращает True если текущий спикер - локальный (я)."""
        with self._lock:
            if self._is_speaking:
                return self._current_is_local

            # Если никто не говорит, проверяем "память"
            if self._last_speaker:
                elapsed = time.time() - self._last_speaker_time
                if elapsed < self.SPEAKER_MEMORY_SECONDS:
                    return self._last_is_local

            return False

    def get_participants(self) -> Dict[str, Participant]:
        """Возвращает словарь участников."""
        with self._lock:
            return self._participants.copy()

    def are_all_mics_muted(self) -> bool:
        """Возвращает True если у всех участников выключены микрофоны."""
        with self._lock:
            if not self._participants:
                return False
            return all(p.mic_muted for p in self._participants.values())

    def is_local_mic_muted(self) -> bool:
        """Возвращает True если микрофон локального пользователя замьючен в KTalk."""
        with self._lock:
            for p in self._participants.values():
                if p.is_local:
                    return p.mic_muted
            # Нет локального участника - считаем что не замьючен
            return False

    def is_speaker_muted(self, speaker_name: str) -> bool:
        """Возвращает True если микрофон спикера с указанным именем замьючен."""
        with self._lock:
            for p in self._participants.values():
                if p.name == speaker_name:
                    return p.mic_muted
            # Спикер не найден среди участников — не можем определить
            return False

    def get_sole_unmuted_remote_participant(self) -> Optional[str]:
        """
        Возвращает имя единственного удалённого участника с включённым микрофоном.

        Используется как fallback когда dominant speaker detection отстаёт:
        если ровно один remote участник unmuted, скорее всего говорит именно он.
        Возвращает None если 0 или 2+ remote участников unmuted (чтобы не угадывать).
        """
        with self._lock:
            unmuted_remote = [
                p for p in self._participants.values()
                if not p.is_local and not p.mic_muted
            ]
            if len(unmuted_remote) == 1:
                return unmuted_remote[0].name
            return None

    def get_speaker_history(self) -> List[SpeakerEvent]:
        """Возвращает историю спикеров."""
        with self._lock:
            return self._speaker_history.copy()

    def _write_debug_state(self):
        """Записывает текущее состояние в debug файл."""
        now = time.time()
        if now - self._last_debug_update < self._debug_update_interval:
            return
        self._last_debug_update = now

        try:
            with self._lock:
                participants = [
                    {
                        "id": p.id,
                        "name": p.name,
                        "is_local": p.is_local,
                        "mic_muted": p.mic_muted
                    }
                    for p in self._participants.values()
                ]

                state = {
                    "timestamp": now,
                    "timestamp_iso": datetime.fromtimestamp(now).isoformat(),
                    "bridge": {
                        "running": self._running,
                        "connected_clients": len(self._connected_clients),
                        "port": self.port
                    },
                    "conference": {
                        "participants": participants,
                        "participant_count": len(participants),
                        "all_mics_muted": all(p.mic_muted for p in self._participants.values()) if self._participants else False
                    },
                    "speaker": {
                        "current": self._current_speaker,
                        "current_id": self._current_speaker_id,
                        "is_speaking": self._is_speaking,
                        "is_local": self._current_is_local,
                        "last_speaker": self._last_speaker,
                        "last_speaker_time": self._last_speaker_time
                    },
                    "segments": {
                        "active_start": self._segment_start_time,
                        "active_speaker": self._segment_speaker_name,
                        "completed_queue": len(self._completed_segments),
                        "history_count": len(self._speaker_history)
                    },
                    "meeting": {
                        "id": self._meeting_storage.get_current_meeting_id() if self._meeting_storage else None
                    },
                    "recording": {
                        "policy": "ktalk_only" if self.ktalk_only else "continuous",
                        "active": self.is_recording_active(),
                    },
                    "pcm": {},
                    "pcm_transport": dict(self._pcm_transport_stats),
                }
            if self._pcm_stats_getter is not None:
                try:
                    state["pcm"].update(self._pcm_stats_getter())
                except Exception as e:
                    state["pcm"]["error"] = str(e)

            with open(self._debug_file, 'w') as f:
                json.dump(state, f, indent=2, ensure_ascii=False)

        except Exception as e:
            print(f"[KTalk Bridge] Debug write error: {e}", file=sys.stderr)

    def _check_segment_timeout(self):
        """Проверяет и завершает сегмент если он превысил MAX_SEGMENT_DURATION."""
        completed_segment = None
        now = time.time()

        with self._lock:
            if (self._segment_start_time is not None
                    and self._segment_speaker_name
                    and (now - self._segment_start_time) >= self.MAX_SEGMENT_DURATION):
                duration_ms = int((now - self._segment_start_time) * 1000)
                if duration_ms > 100:
                    completed_segment = SpeakerSegment(
                        speaker_name=self._segment_speaker_name,
                        speaker_id=self._segment_speaker_id or "",
                        start_time=self._segment_start_time,
                        end_time=now,
                        duration_ms=duration_ms,
                        is_local=self._segment_is_local
                    )
                    self._completed_segments.append(completed_segment)

                # Начинаем новый сегмент с того же спикера
                self._segment_start_time = now

        if completed_segment and self._on_segment_complete:
            self._on_segment_complete(completed_segment)

    def send_transcript(self, speaker: str, text: str, timestamp: str, is_continuation: bool = False, is_local: bool = False, uncertain: bool = False, uncertain_reason: str = "", correction: bool = False, segment_id: int = 0, source_id: str = ""):
        """Отправляет транскрипт всем подключённым клиентам.

        `segment_id` (>0) пробрасывается в DOM как `data-segment-id`, чтобы
        `transcript_refine` мог найти соответствующий bubble при tier-2 апдейте.

        `source_id` (non-empty) — стабильный идентификатор источника (например,
        per-pid PCM stream); расширение использует его в continuation-merge
        предикате, чтобы chunks от разных источников не склеивались в один
        bubble (см. `2026-04-24-per-speaker-refine-bugs.md`, Bug 1).
        """
        # Tag every live transcript with the storage's current meeting_id so
        # a second extension tab (joined to a different meeting) can drop
        # segments that belong to a sibling tab's meeting in `handleTranscript`.
        # Mirrors the meeting_id field on `transcript_refine` / `transcript_history`.
        current_meeting_id = (
            self._meeting_storage.get_current_meeting_id()
            if self._meeting_storage is not None
            else None
        )
        payload = {
            "type": "transcript",
            "speaker": speaker,
            "text": text,
            "timestamp": timestamp,
            "is_continuation": is_continuation,
            "is_local": is_local,  # True = микрофон (я), False = системный звук (другие)
            "uncertain": uncertain,  # True = непонятно кто говорит
            "uncertain_reason": uncertain_reason,  # "local" = системный звук + локальный спикер, "mics_off" = все микрофоны выключены
            "correction": correction,  # True = retroactive correction of previously Unknown segment
            "meeting_id": current_meeting_id,
        }
        if segment_id > 0:
            payload["segment_id"] = segment_id
        if source_id:
            payload["source_id"] = source_id
        message = json.dumps(payload)

        # Отправляем асинхронно через event loop. Snapshot under the
        # lock: `_handle_client` mutates the set on the asyncio thread
        # while this method iterates from the live/audio thread — an
        # unguarded iteration can raise `RuntimeError: Set changed size
        # during iteration`.
        if self._loop:
            with self._lock:
                recipients = list(self._connected_clients)
            for client in recipients:
                try:
                    asyncio.run_coroutine_threadsafe(
                        client.send(message),
                        self._loop
                    )
                except Exception:
                    pass  # Клиент может отключиться

        if segment_id > 0:
            meeting_id = (
                self._meeting_storage.get_current_meeting_id()
                if self._meeting_storage is not None
                else None
            )
            self._record_copilot_transcript(
                segment_id=segment_id,
                speaker=speaker,
                text=text,
                meeting_id=meeting_id,
            )
            self._maybe_emit_transcript_tick()

    def configure_user_profile(self, identity: dict, read_identity, apply_mic) -> None:
        """Use the live process's resolver and microphone label as one identity."""
        with self._lock:
            self._user_identity = {**identity, "local_user_aliases": list(identity["local_user_aliases"])}
            self._read_user_identity = read_identity
            self._apply_mic_identity = apply_mic

    def user_identity_snapshot(self) -> dict:
        with self._lock:
            return {**self._user_identity, "local_user_aliases": list(self._user_identity["local_user_aliases"])}

    def _read_copilot_policy(self) -> dict:
        task = self._ai_settings_store.resolve_task("copilot")
        return {
            "copilot_instructions": task["prompt"],
            "copilot_policy_revision": task["revision"],
            "copilot_enabled": task["enabled"],
        }

    def copilot_policy_snapshot(self) -> dict:
        with self._lock:
            return dict(self._copilot_policy)

    async def _refresh_ai_settings(self, websocket) -> None:
        with self._lock:
            allowed = websocket is not None and websocket in self._subscriber_clients
        if not allowed:
            return
        async with self._ai_settings_refresh_lock:
            try:
                # The refresh frame is a hint, never a source of instructions.
                policy = await asyncio.to_thread(self._read_copilot_policy)
                with self._lock:
                    changed = policy != self._copilot_policy
                    self._copilot_policy = policy
            except Exception:
                await self._send_bounded(websocket, json.dumps({
                    "type": "ai_settings_error",
                    "message": "Could not apply the saved AI settings",
                }))
                return
            if changed:
                if not policy["copilot_enabled"]:
                    self._set_manual_copilot_active(False)
                    for request_id in list(self._copilot_pending_requests):
                        await self._broadcast_copilot_frame({
                            "type": "copilot_status", "status": "error",
                            "request_id": request_id, "message": "Copilot выключен в настройках.",
                        })
                    self._copilot_pending_requests.clear()
                await self._relay_to_supervisors({"type": "copilot_settings_changed"})
                await self._broadcast_to_channel_clients({
                    "type": "copilot_policy_updated",
                    **self.user_identity_snapshot(), **policy,
                })
            await self._send_bounded(websocket, json.dumps({
                "type": "ai_settings_applied",
                "revision": policy["copilot_policy_revision"],
                "model_application": "next_session",
            }, ensure_ascii=False))

    async def _refresh_user_profile(self, websocket) -> None:
        with self._lock:
            allowed = websocket is not None and websocket in self._subscriber_clients
        if not allowed:
            return
        async with self._user_profile_refresh_lock:
            try:
                if self._read_user_identity is None or self._apply_mic_identity is None:
                    raise RuntimeError("Live identity is not connected")
                # The frame revision is only a refresh hint. The file is authoritative.
                identity = self._read_user_identity()
                identity = {**identity, "local_user_aliases": list(identity["local_user_aliases"])}
                with self._lock:
                    changed = identity != self._user_identity
                    self._apply_mic_identity(identity)
                    self._user_identity = identity
            except Exception:
                await self._send_bounded(websocket, json.dumps({
                    "type": "user_profile_error",
                    "message": "Could not apply the saved user profile",
                }))
                return
            if changed:
                await self._broadcast_to_channel_clients({
                    "type": "user_profile_updated", **identity,
                })
            await self._send_bounded(websocket, json.dumps({
                "type": "user_profile_applied",
                "revision": identity["local_user_profile_revision"],
                "identity": identity,
            }, ensure_ascii=False))

    async def _send_identity_tick(self, client, frame: dict) -> None:
        # The audio thread may have queued this before a refresh. Snapshot on
        # the WS loop so an old tick cannot restore aliases after the update.
        frame = {**frame, **self.user_identity_snapshot(), **self.copilot_policy_snapshot()}
        await self._send_bounded(client, json.dumps(frame, ensure_ascii=False))

    def _maybe_emit_transcript_tick(self) -> None:
        """Broadcast a `transcript_tick` to channel clients when triggers fire.

        Two independent triggers, evaluated under `_lock` to keep tick state
        consistent across threads (`send_transcript` runs on the live/audio
        thread; the asyncio loop owns the WS sockets):
          * at least `COPILOT_TICK_NEW_SEGMENTS` new segments since the last
            emit (uses a monotonic counter that survives deque eviction), OR
          * at least `COPILOT_TICK_INTERVAL_SECONDS` of wall-clock elapsed
            since the previous emit.
        Suppressed when no meeting is active (`_copilot_tick_state is None`)
        or no channel client is registered — the second guard avoids
        formatting a snapshot nobody will read.
        """
        with self._lock:
            tick_state = self._copilot_tick_state
            if not self._copilot_policy["copilot_enabled"]:
                return
            if tick_state is None:
                return
            if not self._copilot_channel_clients:
                return
            tick_state["seg_count"] += 1
            now_ts = time.time()
            new_segs = tick_state["seg_count"] - tick_state["last_seg_count"]
            elapsed = now_ts - tick_state["last_emit_ts"]
            time_trigger = elapsed >= COPILOT_TICK_INTERVAL_SECONDS
            count_trigger = new_segs >= COPILOT_TICK_NEW_SEGMENTS
            if not (count_trigger or time_trigger):
                return
            meeting_id = tick_state["meeting_id"]
            meeting_title = tick_state.get("meeting_title") or ""
            ordered_ids = list(self._copilot_transcript_order)
            # Copy buffer entries under the lock — `send_refine` (refine
            # worker thread) and `_record_copilot_transcript` (asyncio
            # thread) mutate `entry["text"]` / `entry["refined"]` in place.
            # Holding references and serialising outside the lock at line
            # 850 would let `json.dumps` observe a torn state (e.g.,
            # refined text with `refined=False`).
            new_segments_payload = [
                dict(self._copilot_transcript_buffer[sid])
                for sid in ordered_ids[-new_segs:]
                if sid in self._copilot_transcript_buffer
                and self._copilot_transcript_buffer[sid].get("meeting_id") == meeting_id
            ]
            recent_payload = [
                dict(self._copilot_transcript_buffer[sid])
                for sid in ordered_ids[-COPILOT_TICK_RECENT_SEGMENTS:]
                if sid in self._copilot_transcript_buffer
                and self._copilot_transcript_buffer[sid].get("meeting_id") == meeting_id
            ]
            recent_context = "\n".join(
                f"{entry.get('speaker') or '?'}: {entry.get('text') or ''}"
                for entry in recent_payload
            )
            tick_state["last_seg_count"] = tick_state["seg_count"]
            tick_state["last_emit_ts"] = now_ts
            recipients = list(self._copilot_channel_clients)

        if self._loop is None:
            return
        frame = {
            "type": "transcript_tick",
            "meeting_id": meeting_id,
            "meeting_title": meeting_title,
            "new_segments": new_segments_payload,
            "recent_context": recent_context,
            "recent_segments": recent_payload,
        }
        for client in recipients:
            try:
                asyncio.run_coroutine_threadsafe(self._send_identity_tick(client, frame), self._loop)
            except Exception:
                pass

    def _record_copilot_transcript(
        self,
        *,
        segment_id: int,
        speaker: str,
        text: str,
        meeting_id: Optional[str],
    ) -> None:
        """Append/replace a recent-transcript entry keyed by segment_id.

        Backs the `get_recent_transcript` MCP tool in
        `recog/copilot_channel.py`. The deque preserves insertion order so
        an overflow can evict the oldest segment_id; for an existing entry
        we overwrite text/speaker (continuations) and bump `end_ms` while
        keeping the original `start_ms` and `refined` flag.
        """
        if segment_id <= 0:
            return
        now_ms = time.time() * 1000
        with self._lock:
            existing = self._copilot_transcript_buffer.get(segment_id)
            if existing is not None:
                existing["speaker"] = speaker
                existing["text"] = text
                existing["end_ms"] = now_ms
                existing["meeting_id"] = meeting_id
                return
            if len(self._copilot_transcript_order) == self._copilot_transcript_order.maxlen:
                evicted = self._copilot_transcript_order.popleft()
                self._copilot_transcript_buffer.pop(evicted, None)
            self._copilot_transcript_order.append(segment_id)
            self._copilot_transcript_buffer[segment_id] = {
                "segment_id": segment_id,
                "speaker": speaker,
                "text": text,
                "start_ms": now_ms,
                "end_ms": now_ms,
                "refined": False,
                "meeting_id": meeting_id,
            }

    def send_refine(self, updates: list[dict], meeting_id: Optional[str] = None):
        """Broadcast tier-2 refined segment text to all connected clients.

        Invoked from `LiveTranscriber._apply_refine` after
        `MeetingStorage.update_segments` rewrites the JSONL. Each entry in
        `updates` carries `segment_id` and `text`; downstream the extension
        overlay swaps bubble text and the webapp promotes tier-1 rows to
        tier-2 (see plan `2026-04-24-per-speaker-refine-transport-ui.md`).

        `meeting_id` identifies the meeting whose segment_ids these updates
        reference. Consumers filter by it so a refine in-flight across a
        meeting switch cannot rewrite rows in the new meeting that happen
        to share a segment_id (ids restart at 1 per meeting).
        """
        if not updates:
            return

        refined_at = datetime.now().isoformat()
        payload_updates = [
            {"segment_id": u["segment_id"], "text": u["text"]}
            for u in updates
        ]
        envelope = {
            "type": "transcript_refine",
            "updates": payload_updates,
            "refined_at": refined_at,
        }
        if meeting_id:
            envelope["meeting_id"] = meeting_id
        message = json.dumps(envelope, ensure_ascii=False)

        # Broadcast to both extension and subscriber clients — subscribers
        # (e.g. frontend `BridgeSubscriber`) relay the refine onto webapp
        # WS clients, see plan `2026-04-24-per-speaker-refine-transport-ui.md`.
        # Snapshot under the lock: `_handle_client` mutates these sets on
        # the asyncio thread while this method runs on the refine worker
        # thread.
        if self._loop:
            with self._lock:
                recipients = list(self._connected_clients) + list(self._subscriber_clients)
            for client in recipients:
                try:
                    asyncio.run_coroutine_threadsafe(
                        client.send(message),
                        self._loop
                    )
                except Exception:
                    pass  # Клиент может отключиться

        # Tier-2 refine rewrites the recent-transcript buffer so the
        # `get_recent_transcript` MCP tool returns refined text once it is
        # available. Segments not yet in the buffer (e.g. evicted past the
        # deque cap) are ignored silently. When `meeting_id` is provided
        # we additionally guard against cross-meeting writes: a refine
        # whose meeting_id does not match the buffer entry's meeting_id
        # belongs to a different meeting and would silently corrupt the
        # current meeting's text (segment_ids restart at 1 per meeting).
        with self._lock:
            for u in updates:
                sid = u.get("segment_id")
                text = u.get("text")
                if not isinstance(sid, int) or sid <= 0:
                    continue
                if not isinstance(text, str):
                    continue
                entry = self._copilot_transcript_buffer.get(sid)
                if entry is None:
                    continue
                if meeting_id is not None and entry.get("meeting_id") != meeting_id:
                    continue
                entry["text"] = text
                entry["refined"] = True

    async def _send_transcript_history(self, websocket=None):
        """Отправляет историю транскрипта текущей встречи.

        Если передан `websocket` — отправляем только инициатору запроса
        (например, табу, который только что присоединился через `meeting_join`).
        Иначе — broadcast всем extension-клиентам.

        Per-WS адресация принципиальна для случая двух параллельных KTalk-табов:
        `handleTranscriptHistory` в extension'е перетирает `state.currentMeetingId`
        полученным `meeting_id`, и broadcast при `meeting_join` из второго таба
        утаскивает первый таб в чужую встречу, ломая клиентскую фильтрацию
        `handleTranscript` / `handleTranscriptRefine`.
        """
        if not self._meeting_storage:
            return

        segments = self._meeting_storage.get_current_transcript()
        meeting_id = self._meeting_storage.get_current_meeting_id()

        frame = {
            "type": "transcript_history",
            "meeting_id": meeting_id,
            "segments": segments,
        }
        if websocket is not None:
            try:
                await websocket.send(json.dumps(frame, ensure_ascii=False))
            except Exception:
                pass
            return
        await self._broadcast_message(frame)

    async def _send_bounded(self, client, message: str) -> bool:
        """Send `message` to one client, bounded by `BROADCAST_SEND_TIMEOUT_S`.

        WebSocket flow-control backpressure makes `send()` block without
        raising, so an unbounded send lets one stalled socket freeze the
        whole fan-out loop. On timeout: log one stderr line, mark the client
        so later broadcasts fail fast, schedule the client's `close()` via
        `create_task` (never awaited inline — `close()` on a stalled socket
        blocks until its own close_timeout), and return False. Role-set
        membership is preserved until `_handle_client` reaps the closed
        socket so its channel/supervisor/meeting cleanup still runs. Clients
        without a `close` attribute (test fakes) are tolerated. Non-timeout
        exceptions propagate so each fan-out loop keeps its existing
        per-client error visibility.
        """
        with self._lock:
            if client in self._timed_out_clients:
                return False
        try:
            await asyncio.wait_for(
                client.send(message), timeout=BROADCAST_SEND_TIMEOUT_S
            )
            return True
        except asyncio.TimeoutError:
            with self._lock:
                still_registered = (
                    client in self._connected_clients
                    or client in self._subscriber_clients
                    or client in self._copilot_channel_clients
                    or client in self._supervisor_clients
                )
                first_timeout = (
                    still_registered and client not in self._timed_out_clients
                )
                if first_timeout:
                    self._timed_out_clients.add(client)
            if not first_timeout:
                return False
            print(
                f"[KTalk Bridge] Send timed out after {BROADCAST_SEND_TIMEOUT_S}s — "
                f"closing stalled client",
                file=sys.stderr,
            )
            close = getattr(client, "close", None)
            if close is not None:
                try:
                    result = close()
                    if asyncio.iscoroutine(result):
                        async def close_safely():
                            try:
                                await result
                            except Exception as e:
                                print(
                                    f"[KTalk Bridge] Failed to close stalled client: {e}",
                                    file=sys.stderr,
                                )

                        task = asyncio.get_running_loop().create_task(close_safely())
                        self._bg_close_tasks.add(task)
                        task.add_done_callback(self._bg_close_tasks.discard)
                except Exception:
                    pass
            return False

    async def _broadcast_message(self, data: dict):
        """Отправляет сообщение всем подключённым extension-клиентам.

        Extension-only by design: subscribers (frontend `BridgeSubscriber`)
        must not receive ordinary transcript/lifecycle/meeting frames.
        `transcript_refine` uses its dedicated `send_refine` path; copilot
        frames use `_broadcast_copilot_frame` instead.
        """
        if not self._connected_clients:
            return

        message = json.dumps(data, ensure_ascii=False)

        async def dispatch(client):
            try:
                await self._send_bounded(client, message)
            except Exception:
                pass  # Клиент может отключиться

        await asyncio.gather(
            *(dispatch(client) for client in list(self._connected_clients)),
            return_exceptions=True,
        )

    async def _broadcast_copilot_frame(self, data: dict):
        """Fan a copilot frame out to extension AND subscriber clients.

        `_broadcast_message` targets only `_connected_clients` (the Chrome
        extension), so subscribers — the frontend `BridgeSubscriber`, which
        relays onto the webapp `#/live` view — would never see copilot
        replies or status. Every copilot broadcast site routes through here
        instead; `_broadcast_message` stays extension-only so transcript,
        lifecycle and meeting frames do not gain the wider fan-out.

        Unlike `_broadcast_message` this must NOT early-return on an empty
        `_connected_clients`: the primary scenario is a webapp open with no
        conference tab at all, where there are zero extension clients.

        Snapshot under the lock: `_handle_client` mutates both sets on the
        asyncio thread (same reason as `send_refine`).
        """
        with self._lock:
            recipients = list(self._connected_clients) + list(self._subscriber_clients)
        if not recipients:
            return

        message = json.dumps(data, ensure_ascii=False)

        async def dispatch(client):
            try:
                await self._send_bounded(client, message)
            except Exception:
                pass  # Клиент может отключиться

        await asyncio.gather(
            *(dispatch(client) for client in recipients), return_exceptions=True
        )

    async def _broadcast_to_channel_clients(self, data: dict):
        """Send a frame to every registered Copilot channel client.

        Channel clients live outside `_connected_clients`, so the regular
        `_broadcast_message` does not reach them. Used for meeting
        lifecycle (`meeting_started`/`meeting_ended`) and any future
        bridge → channel notifications.
        """
        with self._lock:
            channel_clients = list(self._copilot_channel_clients)
        if not channel_clients:
            return
        message = json.dumps(data, ensure_ascii=False)

        async def dispatch(client):
            try:
                await self._send_bounded(client, message)
            except Exception as e:
                print(
                    f"[KTalk Bridge] Failed to send {data.get('type')} to channel client: {e}",
                    file=sys.stderr,
                )

        await asyncio.gather(
            *(dispatch(client) for client in channel_clients), return_exceptions=True
        )

    async def _emit_lifecycle(self, frame: dict):
        """Fan a meeting-lifecycle frame out to every consumer.

        Single fan-out point for `meeting_started` / `meeting_ended`: channel
        clients (context for the next Claude turn), extension clients (system
        entry in the Copilot tab), AND the copilot supervisor (auto-session
        spawn/kill trigger). Routing all three sites through one helper means
        a future emit site cannot silently skip the supervisor — which would
        never wake up. Supervisor send errors are swallowed with a log so a
        dead supervisor socket cannot crash the lifecycle handlers on the
        transcription path.
        """
        if frame.get("type") == "meeting_started":
            frame = {**frame, **self.user_identity_snapshot(), **self.copilot_policy_snapshot()}
        if self.copilot_policy_snapshot()["copilot_enabled"]:
            await self._broadcast_to_channel_clients(frame)
        await self._broadcast_message(frame)
        with self._lock:
            supervisors = list(self._supervisor_clients)
        if not supervisors:
            return
        message = json.dumps(frame, ensure_ascii=False)

        async def dispatch(client):
            try:
                await self._send_bounded(client, message)
            except Exception as e:
                print(
                    f"[KTalk Bridge] Failed to send {frame.get('type')} to supervisor: {e}",
                    file=sys.stderr,
                )

        await asyncio.gather(
            *(dispatch(client) for client in supervisors), return_exceptions=True
        )

    async def _relay_to_supervisors(self, frame: dict) -> bool:
        """Forward a frame verbatim to every registered supervisor client.

        Returns True only when the frame actually reached at least one
        supervisor — registration alone is not delivery. A socket already in
        its closing handshake stays in `_supervisor_clients` until
        `_handle_client`'s finally-block reaps it, so "non-empty set" would
        report success for a frame nobody received; the spawn caller would then
        arm the manual mirror with no session behind it and tell the user
        nothing. False makes the caller reply `supervisor_disconnected`
        instead. Per-client send errors are swallowed with a log exactly as in
        `_emit_lifecycle`: a dead supervisor socket must not crash the message
        handler, nor abort delivery to the sockets queued behind it.

        Timed-out sends are delivery-unknown: cancelling `send()` after the
        frame reached the transport buffer means the peer may still receive
        it, so a timeout is counted as not delivered (at-most-once from the
        caller's perspective) — the originator may see
        `supervisor_disconnected` while the supervisor still acts on the
        frame.
        """
        with self._lock:
            supervisors = list(self._supervisor_clients)
        if not supervisors:
            return False
        message = json.dumps(frame, ensure_ascii=False)
        delivered = False
        for client in supervisors:
            if (
                frame.get("type") == "copilot_spawn_request"
                and self._is_meeting_active()
            ):
                break
            try:
                if await self._send_bounded(client, message):
                    delivered = True
            except Exception as e:
                print(
                    f"[KTalk Bridge] Failed to send {frame.get('type')} to supervisor: {e}",
                    file=sys.stderr,
                )
        return delivered

    async def _reply_copilot_status(
        self, websocket, request_id: str, status: str, message: str = ""
    ) -> None:
        """Answer the ORIGINATOR socket with a per-request `copilot_status`.

        Deliberately not a broadcast: the frame binds an error to one click via
        `request_id`, so fanning it out would make every open webapp render a
        failure for a button it never pressed.
        """
        if websocket is None:
            print(
                f"[KTalk Bridge] {status} status for request_id={request_id} "
                f"dropped — no originator socket",
                file=sys.stderr,
            )
            return
        frame = json.dumps(
            {
                "type": "copilot_status",
                "request_id": request_id,
                "status": status,
                "message": message,
            },
            ensure_ascii=False,
        )
        try:
            await websocket.send(frame)
        except Exception as e:
            print(
                f"[KTalk Bridge] Failed to send {status} status: {e}",
                file=sys.stderr,
            )

    def _set_manual_copilot_active(self, active: bool) -> None:
        """Update the manual mirror and restart sessions-zero debounce."""
        if self._manual_copilot_active == active:
            return
        self._manual_copilot_active = active
        self._sessions_zero_since = None

    async def _preempt_manual_copilot(self, *, notify: bool = True) -> bool:
        """Clear the manual mirror when a conference takes over the copilot.

        Called from the conference `meeting_started` emit path: the supervisor
        kills the `"manual"` session and respawns bound to the meeting id, so
        the bridge's mirror must follow or a later reconnect would resurrect a
        session that no longer exists.

        With `notify=False`, only the synchronous state mutation runs. This
        lets `meeting_join` invalidate late manual replies before its first
        lifecycle-send await without putting subscriber I/O on the lifecycle
        critical path.
        """
        if not self._manual_copilot_active:
            return False
        self._set_manual_copilot_active(False)
        # Invalidate pending manual-chat requests (recorded with meeting id
        # "") — the supervisor kills the manual session, so a late reply for
        # one of these must not be broadcast into the new conference chat.
        # The `stale:` marker follows the existing convention; the reply
        # handler's stale-prefix branch drops (and terminally echoes) it.
        stale_count = 0
        for req_id, mid in list(self._copilot_pending_requests.items()):
            if mid == "":
                self._copilot_pending_requests[req_id] = f"stale:{mid}"
                stale_count += 1
        if stale_count:
            print(
                f"[KTalk Bridge] Marked {stale_count} manual copilot request(s) stale on preemption",
                file=sys.stderr,
            )
        if notify:
            await self._notify_manual_copilot_preempted()
        return True

    async def _notify_manual_copilot_preempted(self) -> None:
        """Notify webapp subscribers after conference lifecycle delivery.

        The extension has no renderer for this status, so its frame stream
        stays byte-identical.
        """
        with self._lock:
            subscribers = list(self._subscriber_clients)
        if not subscribers:
            return
        message = json.dumps(
            {
                "type": "copilot_status",
                "request_id": "",
                "status": "manual_preempted",
                "message": "Manual copilot session preempted by a conference meeting",
            },
            ensure_ascii=False,
        )
        async def dispatch(client):
            try:
                await self._send_bounded(client, message)
            except Exception as e:
                print(
                    f"[KTalk Bridge] Failed to send manual_preempted to subscriber: {e}",
                    file=sys.stderr,
                )

        await asyncio.gather(
            *(dispatch(client) for client in subscribers), return_exceptions=True
        )

    def _is_meeting_active(self) -> bool:
        """True when a conference meeting is genuinely in progress.

        Deliberately NOT `current_meeting_path is not None` — that field is
        never None (`on_ktalk_leave` immediately creates an orphan). A meeting
        is active only when the current meeting was opened by a conference
        platform (`is_conference_source(meta["source"])` — ktalk / vas3k /
        jitsi / matrix) AND at least one extension client is connected to
        drive it.
        """
        storage = self._meeting_storage
        if storage is None or not storage.current_meeting_path:
            return False
        try:
            meta = storage._load_meta()
        except Exception as e:
            print(
                f"[KTalk Bridge] _load_meta failed computing meeting-active: {e}",
                file=sys.stderr,
            )
            return False
        if not (meta and is_conference_source(meta.get("source"))):
            return False
        with self._lock:
            return bool(self._connected_clients)

    def _copilot_should_be_alive(self) -> bool:
        """A copilot session should exist: conference meeting OR manual session."""
        return self._is_meeting_active() or self._manual_copilot_active

    def _compute_meeting_state(self) -> dict:
        """Build the `meeting_state` reconciliation reply for `supervisor_hello`.

        `active` is True when a copilot session should exist at all — a
        conference meeting (`_is_meeting_active`) OR an armed manual mirror —
        with the conference taking precedence for meeting_id / meeting_title.
        Neither is populated while inactive, so a stale orphan meeting never
        looks live to the supervisor.

        A manual session (no conference, `_manual_copilot_active` armed) is
        reported as the synthetic `MANUAL_MEETING_ID` so the supervisor's
        reconcile adopts its live tmux session instead of GRACE-killing it.
        Conference first: `"manual"` must never mask a real meeting id.
        """
        active = self._is_meeting_active()
        meeting_id = None
        meeting_title = None
        if active:
            storage = self._meeting_storage
            meeting_id = storage.current_meeting_id
            try:
                meeting_title = storage._load_meta().get("title")
            except Exception:
                meeting_title = None
        elif self._manual_copilot_active:
            active = True
            meeting_id = MANUAL_MEETING_ID
        return {
            "type": "meeting_state",
            "active": active,
            "meeting_id": meeting_id,
            "meeting_title": meeting_title,
        }

    def _set_claude_sessions(self, count: Optional[int]):
        """Record the daemon's live MCP-session count for the merge.

        The `sessions==0 -> offline` debounce anchor lives in
        `_update_sessions_zero_anchor`, NOT here: the always-on daemon reports
        `count=0` at its own startup — long before any meeting — so anchoring
        on the raw count would leave the debounce clock stale (already elapsed)
        by the time a copilot comes up, defeating the anti-flap cushion.
        """
        self._claude_sessions_count = count

    def _update_sessions_zero_anchor(self, now: float) -> None:
        """Track when the sessions-zero *offline* condition began holding.

        The offline verdict fires only when the daemon reports a confident 0
        live sessions AND the supervisor is past `spawning` AND a copilot
        session should be alive (conference meeting OR manual session —
        `_copilot_should_be_alive`, so a manual copilot whose Claude session
        never registered cannot keep a false green chip). Anchoring
        `_sessions_zero_since` to when that combined
        condition *begins* (rather than to the raw count) means the debounce
        window starts when the copilot comes online with no MCP session yet —
        giving the daemon's 5 s poll time to report the fresh session before
        the chip could flip to offline. Reset to None whenever any component
        drops (copilot connects, meeting ends, supervisor re-enters spawning)
        so the debounce restarts cleanly on the next occurrence.
        """
        status = self._supervisor_agent_status
        eligible = (
            self._claude_sessions_count == 0
            and status is not None
            and status not in ("offline", "spawning")
            and self._copilot_should_be_alive()
        )
        if not eligible:
            self._sessions_zero_since = None
        elif self._sessions_zero_since is None:
            self._sessions_zero_since = now

    def _effective_agent_state(self, now: float) -> str:
        """Merge supervisor status + daemon session count into a chip status (D4)."""
        status = self._supervisor_agent_status
        # Supervisor absent, explicitly offline, or silent past two missed
        # heartbeats → offline regardless of the last screen state.
        if (
            status is None
            or status == "offline"
            or (now - self._supervisor_status_ts) > AGENT_SILENCE_THRESHOLD_S
        ):
            return "offline"
        # Daemon reports a confident zero live MCP sessions while a copilot
        # session should be alive (conference meeting OR manual session) and
        # the supervisor is past `spawning`: the TUI is up but the copilot
        # never connected. Debounced so leaving `spawning` before the daemon's
        # first poll does not flap the chip. `None` count is unknown (not a
        # confident 0) and never forces offline here.
        if (
            self._claude_sessions_count == 0
            and status != "spawning"
            and self._sessions_zero_since is not None
            and (now - self._sessions_zero_since) >= AGENT_SESSIONS_ZERO_DEBOUNCE_S
            and self._copilot_should_be_alive()
        ):
            return "offline"
        return status

    async def _recover_missing_channel_session(self, now: float):
        """Ask the supervisor to replace a TUI that has lost its MCP transport."""
        if (
            self._sessions_zero_since is None
            or now - self._sessions_zero_since < AGENT_CHANNEL_RECOVERY_S
            or self._supervisor_agent_status not in ("online", "working")
            or now - self._supervisor_status_ts > AGENT_SILENCE_THRESHOLD_S
        ):
            return
        if (
            self._channel_recovery_sent_at is not None
            and now - self._channel_recovery_sent_at < AGENT_CHANNEL_RECOVERY_S
        ):
            return
        # Throttle attempts even when delivery fails; the next heartbeat retries.
        self._channel_recovery_sent_at = now
        await self._relay_to_supervisors(
            {
                "type": "copilot_channel_unavailable",
                "meeting_id": self._compute_meeting_state()["meeting_id"],
            }
        )

    async def _recompute_agent_state(self, now: Optional[float] = None):
        """Recompute the effective chip status and broadcast on change only.

        Called on every `copilot_agent_status` / `claude_sessions` frame and
        periodically from the server loop (so supervisor silence flips the
        chip to offline even with no incoming frame). Heartbeats that do not
        change the effective status are not broadcast to extensions.
        """
        if now is None:
            now = time.monotonic()
        self._update_sessions_zero_anchor(now)
        effective = self._effective_agent_state(now)
        await self._recover_missing_channel_session(now)
        if effective == self._last_effective_agent_state:
            return
        self._last_effective_agent_state = effective
        await self._broadcast_copilot_frame({
            "type": "copilot_status",
            "request_id": "",
            "status": "agent_state",
            "agent_state": effective,
        })

    async def _handle_client(self, websocket):
        """Обрабатывает подключение клиента (Chrome extension или subscriber)."""
        # Origin allowlist (recog/ws_origin.py): WebSockets bypass CORS, so
        # reject browser pages that are not the extension's conference hosts
        # or the webapp BEFORE the socket is registered anywhere and before
        # any inbound frame is read. Python clients (supervisor, channel
        # daemon, BridgeSubscriber) send no Origin header and pass; test
        # fakes without a `request` attribute are treated the same way.
        request = getattr(websocket, "request", None)
        if request is not None:
            # websockets >= 14 asyncio implementation: handshake request
            # object with a Headers mapping.
            headers = request.headers
        else:
            # websockets 12/13 legacy protocol exposes the handshake headers
            # as `request_headers` instead. Test fakes with neither
            # attribute fall through as no-Origin (allowed).
            headers = getattr(websocket, "request_headers", None)
        origin = None
        rejected_reason = None
        if headers is not None:
            try:
                origin = headers.get("Origin")
            except LookupError:
                # websockets' Headers.get raises MultipleValuesError (a
                # LookupError) when the handshake carries duplicate Origin
                # headers — never a legitimate client; reject cleanly.
                rejected_reason = "duplicate Origin headers"
        if rejected_reason is None and not origin_allowed(origin):
            rejected_reason = f"disallowed origin {origin!r}"
        if rejected_reason is not None:
            # Rate-limited: this path is attacker-reachable (a hostile page
            # can loop connection attempts) — do not let it flood stderr.
            log_rate_limited(
                "bridge-ws-origin-reject",
                f"[KTalk Bridge] Rejected WS connection: {rejected_reason} "
                f"({websocket.remote_address})",
            )
            await websocket.close(code=1008, reason="origin not allowed")
            return
        # Guard set mutation with `self._lock` — `send_transcript` /
        # `send_refine` snapshot these sets from non-asyncio threads and
        # an unlocked mutation races their iteration.
        with self._lock:
            self._connected_clients.add(websocket)
            # Reset sequence counter on new client connect — extension resets its counter
            # on page reload, so we must accept seq=1 again.
            # _clock_offset is NOT reset here: a subscriber client (frontend
            # `BridgeSubscriber`) reconnect would otherwise wipe the
            # extension's synced offset for up to ~30 s (next clock_sync).
            # The extension sends `clock_sync` immediately after its own
            # connect, overwriting any stale offset within a few ms.
            self._last_seq = 0
            channel_clients_present = bool(self._copilot_channel_clients)
        print(f"[KTalk Bridge] Client connected from {websocket.remote_address}", file=sys.stderr)
        # Late-joiner channel state: the new socket is currently in
        # `_connected_clients`; we can't tell yet whether it's an extension,
        # subscriber, or channel client (those send hellos later). Sending
        # `channel_connected` to channel/subscriber clients is harmless —
        # `recog/copilot_channel.py::_handle_inbound` only acts on a fixed
        # whitelist and `recog/web/bridge_subscriber.py` only forwards
        # `transcript_refine`. Without this primer, an extension that
        # reconnects to a bridge whose channel was already up would never
        # learn the channel is healthy and would keep showing "unknown".
        if channel_clients_present:
            try:
                await websocket.send(json.dumps({
                    "type": "copilot_status",
                    "request_id": "",
                    "status": "channel_connected",
                    "message": "Copilot channel server connected",
                }, ensure_ascii=False))
            except Exception as e:
                print(
                    f"[KTalk Bridge] Failed to send channel_connected primer to new client: {e}",
                    file=sys.stderr,
                )
        # Agent-state primer: the chip broadcast is change-only, so an
        # extension that reloads mid-meeting reconnects with its default
        # ⚫ offline chip and — if the effective status has not changed since —
        # never receives an `agent_state` frame, showing a wrong offline for
        # the rest of the meeting. Send the current effective status now so a
        # reloaded tab reflects the live chip immediately. Harmless to
        # non-extension clients (subscriber/channel/supervisor all ignore it).
        try:
            await websocket.send(json.dumps({
                "type": "copilot_status",
                "request_id": "",
                "status": "agent_state",
                "agent_state": self._last_effective_agent_state,
            }, ensure_ascii=False))
        except Exception as e:
            print(
                f"[KTalk Bridge] Failed to send agent_state primer to new client: {e}",
                file=sys.stderr,
            )

        try:
            async for message in websocket:
                try:
                    data = json.loads(message)
                    if data.get("type") == "subscriber_hello":
                        # Subscriber announces itself — move the connection
                        # from the extension set to the subscribers set so
                        # it does not block meeting lifecycle or receive
                        # clock_sync resets. Subscribers may later send manual
                        # copilot control frames; the same loop handles those
                        # while continuing to relay server→client broadcasts.
                        with self._lock:
                            self._connected_clients.discard(websocket)
                            self._subscriber_clients.add(websocket)
                            no_extension_clients = not self._connected_clients
                        print(
                            f"[KTalk Bridge] Subscriber registered from {websocket.remote_address}",
                            file=sys.stderr,
                        )
                        # Race recovery: between this socket's WS-open and
                        # hello-processing, it sat in `_connected_clients`.
                        # If an extension client disconnected in that window,
                        # its finally-block saw `no_extension_clients=False`
                        # and skipped the meeting-end lifecycle. Re-run the
                        # check now that the subscriber has been unmasked.
                        if no_extension_clients:
                            await self._end_meeting_on_extension_gone()
                        continue
                    await self._process_message(data, websocket=websocket)
                except json.JSONDecodeError:
                    print(f"[KTalk Bridge] Invalid JSON: {message[:100]}", file=sys.stderr)
                except Exception as e:
                    print(f"[KTalk Bridge] Error processing message: {e}", file=sys.stderr)
        except Exception as e:
            print(f"[KTalk Bridge] Connection error: {e}", file=sys.stderr)
        finally:
            with self._lock:
                was_subscriber = websocket in self._subscriber_clients
                was_copilot_channel = websocket in self._copilot_channel_clients
                was_supervisor = websocket in self._supervisor_clients
                self._connected_clients.discard(websocket)
                self._subscriber_clients.discard(websocket)
                self._copilot_channel_clients.discard(websocket)
                self._supervisor_clients.discard(websocket)
                self._timed_out_clients.discard(websocket)
                # Release meeting ownership if the owner WS just dropped —
                # otherwise the rejection gate would permanently block every
                # subsequent meeting_join from a fresh tab (the dead WS
                # reference would still match nothing in _connected_clients
                # but the comparison check happens before that read).
                was_meeting_owner = websocket is self._meeting_owner_ws
                if was_meeting_owner:
                    self._meeting_owner_ws = None
                no_extension_clients = not self._connected_clients
                no_channel_clients_left = not self._copilot_channel_clients
                no_supervisor_clients_left = not self._supervisor_clients
            if was_supervisor:
                role = "Copilot supervisor"
            elif was_copilot_channel:
                role = "Copilot channel"
            elif was_subscriber:
                role = "Subscriber"
            else:
                role = "Client"
            print(
                f"[KTalk Bridge] {role} disconnected",
                file=sys.stderr,
            )
            # Channel-client departure leaves any in-flight `copilot_chat_message`
            # request without a path to Claude — the reply will never arrive,
            # so the extension would hang forever on "Жду ответа Claude…" with
            # the textarea disabled. When the LAST channel client leaves, drop
            # pending bookkeeping (so a future reconnect cannot reuse a stale
            # id) and broadcast a channel-level `channel_disconnected` to all
            # extensions. The extension flips every pending placeholder via
            # the existing handler. Other channel clients still present can
            # still answer, so we only fire on the 1→0 transition.
            if was_copilot_channel and no_channel_clients_left:
                self._set_claude_sessions(None)
                stranded = 0
                for req_id, mid in list(self._copilot_pending_requests.items()):
                    if isinstance(mid, str) and mid.startswith("stale:"):
                        continue
                    self._copilot_pending_requests.pop(req_id, None)
                    stranded += 1
                if stranded:
                    print(
                        f"[KTalk Bridge] Channel disconnected with "
                        f"{stranded} in-flight request(s); dropped bookkeeping",
                        file=sys.stderr,
                    )
                await self._broadcast_copilot_frame({
                    "type": "copilot_status",
                    "request_id": "",
                    "status": "channel_disconnected",
                    "message": "Copilot channel server disconnected",
                })
            # Supervisor departure: drop its last reported status so the merge
            # flips the chip to offline promptly instead of waiting out the
            # silence window. Only on the last supervisor leaving (1→0).
            if was_supervisor and no_supervisor_clients_left:
                self._supervisor_agent_status = None
                await self._recompute_agent_state()
            # Only extension clients count for lifecycle — subscribers are
            # passive listeners and must not block "last extension gone".
            if no_extension_clients or was_meeting_owner:
                await self._end_meeting_on_extension_gone()

    async def _end_meeting_on_extension_gone(self) -> None:
        """Run the end-of-meeting cleanup when the meeting lost its extension.

        Two conditions end a meeting here, whichever comes first:

        - **The owner is gone.** `_meeting_owner_ws` — the socket that sent
          `meeting_join` — closed. Other extension tabs may still be
          connected, and usually are: the content script runs on every
          conference host in the manifest, so an idle `example.ktalk.ru` tab
          holds a WS without being in any call. Waiting for those would pin
          the meeting LIVE indefinitely.
        - **Nobody is left.** `_connected_clients` is empty. Covers a meeting
          whose owner the bridge never recorded (bridge restarted mid-call),
          and is re-checked by the `*_hello` handlers, which recover from the
          race where a Python client was still transiently counted as an
          extension when the real one disconnected.

        Idempotent: safe to call when no meeting is active (the inner
        checks skip the cleanup).

        Mirrors the explicit `meeting_leave` Copilot cleanup so a hard
        extension disconnect (page reload, crash, network drop) cannot
        leak tick state, transcript-buffer entries, or in-flight
        `copilot_chat_message` request bookkeeping into the next meeting.
        """
        self._set_meeting_capture(False)
        # Clear stale participant state BEFORE bumping the generation
        # (same order as meeting_leave) so that the live thread's next
        # chunk cannot read old-meeting speaker data through
        # get_current_speaker / get_sole_unmuted_remote_participant.
        self._clear_speaker_state()
        if not (self._meeting_storage and self._meeting_storage.current_meeting_path):
            return
        try:
            meta = self._meeting_storage._load_meta()
        except Exception as e:
            print(
                f"[KTalk Bridge] _load_meta failed during disconnect cleanup: {e}",
                file=sys.stderr,
            )
            return
        if not (meta and is_conference_source(meta.get("source"))):
            return
        # Capture prev meeting context BEFORE on_ktalk_leave switches it
        prev_meeting_id = self._meeting_storage.current_meeting_id
        prev_meeting_path = self._meeting_storage.current_meeting_path
        # Use the same pre/post switch flow as meeting_join/leave
        # so that _meeting_generation is bumped and WAV writers are swapped.
        if self._on_pre_meeting_switch:
            self._on_pre_meeting_switch()
        new_meeting_id = self._meeting_storage.on_ktalk_leave("disconnected")
        if self._on_meeting_change:
            meeting_path = self._meeting_storage.current_meeting_path
            self._on_meeting_change(new_meeting_id, meeting_path)
        # Trigger async increment generation for the meeting we just left
        if self._increment_generator and prev_meeting_id and prev_meeting_path:
            task = asyncio.create_task(self._safe_generate_increment(prev_meeting_id, prev_meeting_path))
            self._background_tasks.add(task)
            task.add_done_callback(self._background_tasks.discard)
        # Copilot cleanup (mirror of `meeting_leave`): mark in-flight
        # requests stale, halt proactive ticks, free the transcript buffer,
        # and notify any still-connected channel client that the meeting
        # ended. Without this a hard disconnect would let a late
        # `copilot_channel_reply` from the previous meeting leak into the
        # next extension session.
        stale_count = 0
        for req_id, mid in list(self._copilot_pending_requests.items()):
            if mid == prev_meeting_id and not mid.startswith("stale:"):
                self._copilot_pending_requests[req_id] = f"stale:{mid}"
                stale_count += 1
        if stale_count:
            print(
                f"[KTalk Bridge] Marked {stale_count} copilot request(s) stale on extension disconnect",
                file=sys.stderr,
            )
        self._copilot_tick_state = None
        with self._lock:
            self._copilot_transcript_buffer.clear()
            self._copilot_transcript_order.clear()
        if prev_meeting_id:
            # The extension that owned this meeting is gone; any extension
            # still connected sits on another tab and filters the frame by
            # its own `state.currentMeetingId`. `_emit_lifecycle` also
            # reaches channel clients and the supervisor, which is the point
            # of the broadcast here.
            await self._emit_lifecycle({
                "type": "meeting_ended",
                "meeting_id": prev_meeting_id,
                "created_at": time.time() * 1000,
            })
        print("[KTalk Bridge] Ended KTalk meeting due to client disconnect", file=sys.stderr)

    async def _process_message(self, data: dict, websocket=None):
        """Обрабатывает сообщение от extension.

        `websocket` is required for branches that move the originating
        socket between client sets (e.g. `copilot_channel_hello`) or that
        need to address the originator directly (e.g. `copilot_chat_message`
        replying with a `channel_disconnected` status). Other branches
        broadcast or operate on shared state and ignore it.
        """
        msg_type = data.get("type")

        if (msg_type in ("copilot_spawn_request", "copilot_chat_message")
                and not self.copilot_policy_snapshot()["copilot_enabled"]):
            request_id = data.get("request_id")
            if isinstance(request_id, str) and request_id:
                await self._reply_copilot_status(websocket, request_id, "error", "Copilot выключен в настройках.")
            return
        if (msg_type in ("copilot_channel_reply", "copilot_channel_status")
                and not self.copilot_policy_snapshot()["copilot_enabled"]):
            return

        if msg_type == "ai_settings_refresh":
            await self._refresh_ai_settings(websocket)

        elif msg_type == "user_profile_refresh":
            await self._refresh_user_profile(websocket)

        elif msg_type == "speaker_update":
            # Проверяем sequence number для обнаружения out-of-order сообщений
            seq = data.get("seq")
            if seq is not None:
                if seq <= self._last_seq:
                    print(f"[KTalk Bridge] Out-of-order speaker_update: seq={seq}, last={self._last_seq}", file=sys.stderr)
                self._last_seq = max(self._last_seq, seq)

            # Основное событие: обновление текущего спикера
            speaker = data.get("speaker")  # {name, id, isLocal} или null
            raw_ts = data.get("timestamp", time.time() * 1000) / 1000  # JS timestamp в мс -> секунды
            event_time = raw_ts + self._clock_offset if self._clock_offset else raw_ts

            completed_segments = []

            with self._lock:
                if speaker:
                    new_speaker_id = speaker.get("id")
                    new_speaker_name = speaker.get("name")
                    new_is_local = speaker.get("isLocal", False)

                    # Логируем первого спикера
                    if not self._first_speaker_logged:
                        print(f"[KTalk Bridge] First speaker received: {new_speaker_name}", file=sys.stderr)
                        self._first_speaker_logged = True

                    # Проверяем нужно ли завершить текущий сегмент
                    should_complete = False
                    if self._segment_start_time is not None:
                        # Сменился спикер
                        if self._segment_speaker_id != new_speaker_id:
                            should_complete = True
                        # Или превышен лимит времени
                        elif (event_time - self._segment_start_time) >= self.MAX_SEGMENT_DURATION:
                            should_complete = True

                    if should_complete:
                        # Завершаем предыдущий сегмент
                        duration_ms = int((event_time - self._segment_start_time) * 1000)
                        if duration_ms > 100:  # Минимум 100мс
                            segment = SpeakerSegment(
                                speaker_name=self._segment_speaker_name or "",
                                speaker_id=self._segment_speaker_id or "",
                                start_time=self._segment_start_time,
                                end_time=event_time,
                                duration_ms=duration_ms,
                                is_local=self._segment_is_local
                            )
                            self._completed_segments.append(segment)
                            completed_segments.append(segment)
                        # Сбрасываем для начала нового сегмента
                        self._segment_start_time = None

                    # Начинаем новый сегмент если его не было
                    if self._segment_start_time is None:
                        self._segment_start_time = event_time
                        self._segment_speaker_name = new_speaker_name
                        self._segment_speaker_id = new_speaker_id
                        self._segment_is_local = new_is_local

                    self._current_speaker = new_speaker_name
                    self._current_speaker_id = new_speaker_id
                    self._current_is_local = new_is_local
                    self._is_speaking = True

                    # Запоминаем последнего спикера
                    self._last_speaker = self._current_speaker
                    self._last_speaker_time = time.time()
                    self._last_is_local = new_is_local

                    # Добавляем в историю (wall-clock time для consistency с live.py)
                    now = time.time()
                    event = SpeakerEvent(
                        speaker_name=self._current_speaker,
                        speaker_id=self._current_speaker_id or "",
                        timestamp=now,
                        is_local=new_is_local
                    )
                    self._speaker_history.append(event)

                    # Trim old history entries (keep last 5 minutes max)
                    cutoff = now - 300  # 5 minutes
                    while self._speaker_history and self._speaker_history[0].timestamp < cutoff:
                        self._speaker_history.pop(0)
                else:
                    # Записываем silence event в историю
                    silence_event = SpeakerEvent(
                        speaker_name=None,
                        speaker_id="",
                        timestamp=time.time(),
                        is_local=False
                    )
                    self._speaker_history.append(silence_event)

                    # Trim old history entries (keep last 5 minutes max)
                    cutoff = time.time() - 300
                    while self._speaker_history and self._speaker_history[0].timestamp < cutoff:
                        self._speaker_history.pop(0)

                    # Спикер замолчал - завершаем сегмент
                    if self._segment_start_time is not None and self._segment_speaker_name:
                        duration_ms = int((event_time - self._segment_start_time) * 1000)
                        if duration_ms > 100:  # Минимум 100мс
                            segment = SpeakerSegment(
                                speaker_name=self._segment_speaker_name,
                                speaker_id=self._segment_speaker_id or "",
                                start_time=self._segment_start_time,
                                end_time=event_time,
                                duration_ms=duration_ms,
                                is_local=self._segment_is_local
                            )
                            self._completed_segments.append(segment)
                            completed_segments.append(segment)

                        # Сбрасываем активный сегмент
                        self._segment_start_time = None
                        self._segment_speaker_name = None
                        self._segment_speaker_id = None

                    self._is_speaking = False
                    self._current_speaker = None
                    self._current_speaker_id = None

                # Захватываем данные для callback внутри lock
                speaker_change_name = self._current_speaker
                speaker_change_is_speaking = self._is_speaking

            # Обновляем debug state
            self._write_debug_state()

            # Вызываем callbacks вне lock
            if self._on_speaker_change:
                self._on_speaker_change(speaker_change_name, speaker_change_is_speaking)

            if self._on_segment_complete:
                for segment in completed_segments:
                    self._on_segment_complete(segment)

        elif msg_type == "participants_update":
            # Обновление списка участников
            participants = data.get("participants", [])

            with self._lock:
                self._participants = {}
                for p in participants:
                    pid = p.get("id", "")
                    self._participants[pid] = Participant(
                        id=pid,
                        name=p.get("name", "Unknown"),
                        is_local=p.get("isLocal", False),
                        mic_muted=p.get("micMuted", True)
                    )

            # Обновляем debug state
            self._write_debug_state()

        elif msg_type == "pcm_transport_stats":
            stats = data.get("stats", {})
            if isinstance(stats, dict):
                with self._lock:
                    self._pcm_transport_stats = {
                        **stats,
                        "received_at": time.time(),
                    }
                self._write_debug_state()
            else:
                print(
                    "[KTalk Bridge] Malformed pcm_transport_stats frame, skipped",
                    file=sys.stderr,
                )

        elif msg_type == "connection_status":
            # Статус подключения к конференции
            connected = data.get("connected", False)
            if not connected:
                if self.ktalk_only and websocket is self._meeting_owner_ws:
                    await self._end_meeting_on_extension_gone()
                    self._meeting_owner_ws = None
                self._clear_speaker_state()
                print("[KTalk Bridge] Conference disconnected", file=sys.stderr)
            else:
                print("[KTalk Bridge] Conference connected", file=sys.stderr)

        elif msg_type == "meeting_join":
            # Extension сообщает о входе во встречу (KTalk, Jitsi, etc.)
            url = data.get("url")
            title = data.get("title")
            platform = data.get("platform", "ktalk")
            if self.ktalk_only and platform != "ktalk":
                if websocket is not None:
                    await websocket.send(
                        json.dumps(
                            {
                                "type": "meeting_join_rejected",
                                "reason": "ktalk_only",
                            }
                        )
                    )
                return
            # Single-meeting-at-a-time enforcement: if another extension WS
            # already owns the active meeting and that WS is still alive,
            # reject this join instead of silently flipping current_meeting_id.
            # Without this gate the older tab is left with a stale
            # state.currentMeetingId — its handleTranscript meeting_id
            # filter then drops every new segment broadcast by the bridge.
            # The audio pipeline + storage are singletons, so there is no
            # honest way to transcribe two meetings in parallel; reject is
            # the only consistent option.
            if (self._meeting_storage and url
                    and self._meeting_owner_ws is not None
                    and self._meeting_owner_ws is not websocket
                    and self._meeting_owner_ws in self._connected_clients
                    and self._meeting_storage.current_meeting_path is not None):
                owner_addr = getattr(self._meeting_owner_ws, 'remote_address', 'unknown')
                print(
                    f"[KTalk Bridge] Rejecting meeting_join from second tab "
                    f"(url={url}); owner={owner_addr} still active",
                    file=sys.stderr,
                )
                if websocket is not None:
                    try:
                        await websocket.send(json.dumps({
                            "type": "meeting_join_rejected",
                            "reason": "another_tab_active",
                            "active_meeting_id": self._meeting_storage.current_meeting_id,
                        }, ensure_ascii=False))
                    except Exception:
                        pass
                return
            if self._meeting_storage and url:
                was_recording = self.is_recording_active()
                old_meeting_path = self._meeting_storage.current_meeting_path
                prev_meeting_id = self._meeting_storage.current_meeting_id
                # Capture the pre-join meta source: an empty orphan reused in
                # place (on_meeting_join case 3: source flips orphan→ktalk,
                # path unchanged) is a REAL meeting start, but the
                # path-change guard below would skip the whole Copilot
                # block — no `meeting_started` (supervisor never wakes), no
                # tick-state seed (transcript_tick dead for the meeting).
                prev_source = None
                if old_meeting_path is not None:
                    try:
                        prev_source = (self._meeting_storage._load_meta() or {}).get("source")
                    except Exception:
                        prev_source = None
                # Clear stale speaker state from previous meeting so that
                # get_current_speaker / get_sole_unmuted_remote_participant
                # cannot return old-meeting participants during the transition.
                self._clear_speaker_state()
                # Invalidate in-flight chunks BEFORE on_meeting_join changes
                # current_meeting_path, so that process_chunk/process_pending
                # generation checks discard stale data before the path switches.
                # For reconnects (same live_id) on_meeting_join won't change the
                # path, so at most a few chunks are harmlessly discarded.
                if self._on_pre_meeting_switch:
                    self._on_pre_meeting_switch()
                meeting_id = self._meeting_storage.on_meeting_join(url, title, platform)
                print(f"[KTalk Bridge] Joined {platform} meeting: {url}", file=sys.stderr)
                # Reinit audio recording, reset Copilot state, and emit
                # lifecycle frames only on real meeting changes — a no-op
                # reconnect (same live_id) must not duplicate
                # `meeting_started` chips in the Copilot tab nor reset the
                # proactive tick counters mid-conversation.
                #
                # "Real meeting change" is NOT only a path change: joining a
                # room right after a leave reuses the empty orphan in place
                # (path unchanged, source flips orphan→ktalk). That join must
                # still run the Copilot block below — emit `meeting_started`
                # (the supervisor's spawn trigger) and seed the tick state —
                # but must NOT rotate audio writers (they already point at
                # this meeting's directory since the leave).
                path_changed = self._meeting_storage.current_meeting_path != old_meeting_path
                became_active = not path_changed and (
                    prev_source == "orphan" or (self.ktalk_only and not was_recording)
                )
                if self.ktalk_only:
                    # Empty orphan reuse and same-room reconnect also need writers
                    # because capture was stopped and its buffers were cleared.
                    try:
                        if self._on_meeting_change:
                            self._on_meeting_change(
                                meeting_id, self._meeting_storage.current_meeting_path
                            )
                        self._set_meeting_capture(True)
                    except Exception as exc:
                        self._meeting_owner_ws = None
                        try:
                            self._set_meeting_capture(False)
                        except Exception as cleanup_exc:
                            print(
                                f"[KTalk Bridge] Capture cleanup failed: {cleanup_exc}",
                                file=sys.stderr,
                            )
                        print(f"[KTalk Bridge] Capture start failed: {exc}", file=sys.stderr)
                        if websocket is not None:
                            await self._send_bounded(
                                websocket,
                                json.dumps(
                                    {
                                        "type": "meeting_join_rejected",
                                        "reason": "capture_unavailable",
                                    }
                                ),
                            )
                        return
                    self._meeting_owner_ws = websocket
                if path_changed or became_active:
                    if path_changed and self._on_meeting_change and not self.ktalk_only:
                        meeting_path = self._meeting_storage.current_meeting_path
                        self._on_meeting_change(meeting_id, meeting_path)
                    # Mark every in-flight `copilot_chat_message` request
                    # that belonged to the prior meeting as stale. A direct
                    # meeting_join transition (no preceding meeting_leave)
                    # would otherwise leak late replies for the previous
                    # meeting into the new meeting's Copilot tab — the
                    # cross-meeting `meeting_id` filter at the reply
                    # handler is best-effort and only fires when Claude
                    # echoes `meeting_id` back. (`!= meeting_id` guards the
                    # became_active case where prev id IS the current id —
                    # current-meeting requests must not be marked stale.)
                    if prev_meeting_id and prev_meeting_id != meeting_id:
                        stale_count = 0
                        for req_id, mid in list(self._copilot_pending_requests.items()):
                            if mid == prev_meeting_id:
                                self._copilot_pending_requests[req_id] = f"stale:{mid}"
                                stale_count += 1
                        if stale_count:
                            print(
                                f"[KTalk Bridge] Marked {stale_count} copilot request(s) stale on meeting_join transition",
                                file=sys.stderr,
                            )
                    # Wipe the recent-transcript buffer so a stale segment_id
                    # from the prior meeting cannot collide with the new
                    # meeting's ids (segment_ids restart at 1 per meeting),
                    # which would otherwise corrupt `start_ms` and leave
                    # `transcript_tick`'s deque-tail slice pointing at the
                    # prior meeting's entries.
                    with self._lock:
                        self._copilot_transcript_buffer.clear()
                        self._copilot_transcript_order.clear()
                    # Emit lifecycle event: channel clients use it to attach
                    # meeting context to the next Claude turn, extension
                    # clients render a system entry in the Copilot tab.
                    meeting_started_frame = {
                        "type": "meeting_started",
                        "meeting_id": meeting_id,
                        "meeting_title": title,
                        "created_at": time.time() * 1000,
                    }
                    # Invalidate the manual mirror and its pending requests
                    # synchronously before lifecycle fan-out can re-enter the
                    # bridge with a late reply. Subscriber notification stays
                    # after lifecycle delivery so slow webapp clients cannot
                    # delay the supervisor's manual-to-conference replacement.
                    manual_preempted = await self._preempt_manual_copilot(
                        notify=False
                    )
                    await self._emit_lifecycle(meeting_started_frame)
                    if manual_preempted:
                        await self._notify_manual_copilot_preempted()
                    # Reset proactive transcript-tick state for the new
                    # meeting. `send_transcript` consults this on every
                    # broadcast; seeding `last_emit_ts` to the join time
                    # ensures the time trigger measures elapsed-since-join
                    # for the very first tick (a 0.0 seed would be falsy
                    # and disable the time path until count_trigger fired).
                    # `seg_count` is a monotonic counter incremented per
                    # `send_transcript` (with positive segment_id) —
                    # tracking the deque's `len()` would lose updates once
                    # the deque saturates at maxlen=300.
                    self._copilot_tick_state = {
                        "meeting_id": meeting_id,
                        "meeting_title": title,
                        "seg_count": 0,
                        "last_seg_count": 0,
                        "last_emit_ts": time.time(),
                    }
                else:
                    # No-op reconnect (same meeting path) — KTalk emits these
                    # during tab focus / socket churn. We deliberately keep the
                    # running tick counters untouched. But a prior (possibly
                    # spurious) `meeting_leave` may have nulled
                    # `_copilot_tick_state`; without re-arming here the active
                    # meeting stays permanently tick-blind — transcript keeps
                    # recording, yet no proactive `transcript_tick` ever fires.
                    # Re-seed only when it is missing or points at a stale
                    # meeting, so a healthy reconnect never resets live counters.
                    tick_state = self._copilot_tick_state
                    if tick_state is None or tick_state.get("meeting_id") != meeting_id:
                        self._copilot_tick_state = {
                            "meeting_id": meeting_id,
                            "meeting_title": title,
                            "seg_count": 0,
                            "last_seg_count": 0,
                            "last_emit_ts": time.time(),
                        }
                # Mark this WS as the owner of the active meeting so that a
                # parallel `meeting_join` from a second tab will be rejected
                # rather than silently flipping `current_meeting_id`.
                if websocket is not None:
                    self._meeting_owner_ws = websocket
                # Отправляем историю транскрипта только инициатору join —
                # broadcast перетёр бы `state.currentMeetingId` в соседнем
                # табе, если он сидит в другой встрече.
                await self._send_transcript_history(websocket=websocket)

        elif msg_type == "meeting_leave":
            if self.ktalk_only and websocket is not self._meeting_owner_ws:
                return
            self._set_meeting_capture(False)
            # Extension сообщает о выходе из встречи
            reason = data.get("reason", "user_left")
            # Release meeting ownership on explicit leave so the next
            # `meeting_join` (this WS or another tab) can claim it.
            if websocket is self._meeting_owner_ws:
                self._meeting_owner_ws = None
            # Clear stale speaker state BEFORE bumping the generation.
            # _on_pre_meeting_switch() increments _meeting_generation; after
            # that, the live thread's next chunk will carry the new generation
            # and pass all generation checks.  If speaker state is still
            # populated at that point, the chunk would be attributed to the
            # old meeting's speaker.  Clearing first guarantees
            # get_current_speaker() returns None for any such chunk.
            self._clear_speaker_state()
            if self._meeting_storage:
                # Capture prev meeting context BEFORE on_ktalk_leave switches it
                prev_meeting_id = self._meeting_storage.current_meeting_id
                prev_meeting_path = self._meeting_storage.current_meeting_path
                # Invalidate in-flight chunks BEFORE on_ktalk_leave changes
                # current_meeting_path (same rationale as meeting_join above)
                if self._on_pre_meeting_switch:
                    self._on_pre_meeting_switch()
                new_meeting_id = self._meeting_storage.on_ktalk_leave(reason)
                print(f"[KTalk Bridge] Left meeting (reason: {reason}), new meeting: {new_meeting_id}", file=sys.stderr)
                # Trigger async increment generation for the meeting we just left
                if self._increment_generator and prev_meeting_id and prev_meeting_path:
                    task = asyncio.create_task(self._safe_generate_increment(prev_meeting_id, prev_meeting_path))
                    self._background_tasks.add(task)
                    task.add_done_callback(self._background_tasks.discard)
                if self._on_meeting_change:
                    meeting_path = self._meeting_storage.current_meeting_path
                    self._on_meeting_change(new_meeting_id, meeting_path)
                # Mark every in-flight `copilot_chat_message` request that
                # belonged to the meeting we just left as stale. A late
                # `copilot_channel_reply` / `_status` for one of these
                # requests must not reach the extension because the chat
                # tab now belongs to the next meeting.
                stale_count = 0
                for req_id, mid in list(self._copilot_pending_requests.items()):
                    if mid == prev_meeting_id and not mid.startswith("stale:"):
                        self._copilot_pending_requests[req_id] = f"stale:{mid}"
                        stale_count += 1
                if stale_count:
                    print(
                        f"[KTalk Bridge] Marked {stale_count} copilot request(s) stale on meeting_leave",
                        file=sys.stderr,
                    )
                # Emit lifecycle event: channel clients stop generating
                # proactive ticks for a meeting that just ended; extension
                # clients render a system entry in the Copilot tab.
                meeting_ended_frame = {
                    "type": "meeting_ended",
                    "meeting_id": prev_meeting_id,
                    "created_at": time.time() * 1000,
                }
                # Stop emitting transcript_tick for the meeting that just
                # ended. Any further `send_transcript` (rare, in-flight) will
                # see `_copilot_tick_state is None` and skip the trigger.
                self._copilot_tick_state = None
                # Free the recent-transcript buffer immediately. A late
                # `send_refine` for this meeting that arrives after the
                # next `meeting_join` would otherwise rewrite an entry
                # that now belongs to a different meeting (segment_ids
                # restart at 1 per meeting), and `meeting_join` only
                # clears the buffer when the path actually changes.
                with self._lock:
                    self._copilot_transcript_buffer.clear()
                    self._copilot_transcript_order.clear()
                # Notify channel clients, extension clients, and the copilot
                # supervisor (safe to await here — speaker state is clean).
                await self._emit_lifecycle(meeting_ended_frame)
                # `new_meeting` адресуется ТОЛЬКО табу, который инициировал
                # leave. `handleNewMeeting` в extension'е сбрасывает
                # `state.currentMeetingId = null` и очищает transcriptLines —
                # broadcast одного `meeting_leave` второго таба обнулил бы
                # стейт всех других открытых табов и сломал бы фильтр в
                # `handleTranscript` для них.
                new_meeting_frame = {
                    "type": "new_meeting",
                    "meeting_id": new_meeting_id,
                }
                if websocket is not None:
                    try:
                        await websocket.send(
                            json.dumps(new_meeting_frame, ensure_ascii=False)
                        )
                    except Exception:
                        pass
                else:
                    await self._broadcast_message(new_meeting_frame)

        elif msg_type == "meeting_title_update":
            # Extension отправляет обновлённый title (после "Joining..." -> реальный)
            title = data.get("title")
            if self._meeting_storage and title:
                self._meeting_storage.update_title(title)
                print(f"[KTalk Bridge] Updated meeting title: {title}", file=sys.stderr)

        elif msg_type == "request_transcript":
            # Extension запрашивает историю транскрипта (при reconnect) —
            # отвечаем только запросившему WS, не broadcast'ом, чтобы не
            # подменить чужому табу `state.currentMeetingId`.
            await self._send_transcript_history(websocket=websocket)

        elif msg_type == "ping":
            # Heartbeat
            pass

        elif msg_type == "clock_sync":
            # Синхронизация часов между браузером и Python
            browser_time = data.get("browser_time", 0)
            if browser_time:
                self._clock_offset = time.time() - browser_time / 1000
                print(f"[KTalk Bridge] Clock sync: offset={self._clock_offset:.3f}s", file=sys.stderr)

        elif msg_type == "pcm_chunk":
            if self.ktalk_only and (
                not self.is_recording_active() or websocket is not self._meeting_owner_ws
            ):
                return
            # Per-speaker PCM-чанк от extension worklet'а. На этом этапе
            # (Task 1) — только парсинг + dispatch в callback. Актуальная
            # обработка (batching + GigaAM.forward) появится в Task 3-4.
            try:
                # Coerce pid / speaker_name to str so downstream slicing
                # (`pid[:6]`, `.replace(...)`) cannot crash on non-string
                # inputs from a malformed / malicious frame — otherwise the
                # exception in `_on_pcm_batch` would drop every sibling
                # chunk in the same batch.
                pid = str(data["pid"])
                if not pid:
                    raise ValueError("pid is empty")
                speaker_name = str(data.get("speaker_name") or "")
                # D9 contract: PCM transport is for REMOTE participants only
                # (extension skips local mic at pcm_transport.js:172). Ignore
                # whatever the frame claims and enforce the invariant here so
                # a buggy or forged frame cannot tag a segment as local.
                is_local = False
                seq = int(data["seq"])
                chunk_start_ms = float(data["chunk_start_ms"])
                chunk_end_ms = float(data["chunk_end_ms"])
                sample_rate = int(data["sample_rate"])
                pcm_base64 = data["pcm_base64"]
            except (KeyError, TypeError, ValueError) as e:
                print(
                    f"[KTalk Bridge] Malformed pcm_chunk frame, skipped: {e}",
                    file=sys.stderr,
                )
                return

            # GigaAM's preprocessor assumes 16 kHz; any other rate would
            # produce silent garbage downstream with no error.  Reject
            # mismatches at ingress — the worklet is hardcoded to 48→16 kHz
            # so this only trips on a protocol drift / buggy client.
            if sample_rate != 16000:
                print(
                    f"[KTalk Bridge] pcm_chunk unsupported sample_rate={sample_rate} "
                    f"(pid={pid}, seq={seq}); expected 16000",
                    file=sys.stderr,
                )
                return

            try:
                pcm_bytes = base64.b64decode(pcm_base64, validate=True)
            except (binascii.Error, ValueError) as e:
                print(
                    f"[KTalk Bridge] pcm_chunk base64 decode failed "
                    f"(pid={pid}, seq={seq}): {e}",
                    file=sys.stderr,
                )
                return

            # Int16LE PCM — odd byte counts cannot be interpreted as samples
            # and would raise ValueError inside PerSpeakerIngest's
            # _decode_pcm_int16, dropping the entire in-flight batch along
            # with the malformed chunk.  Reject at the ingress instead.
            # Zero-length payloads are also rejected: they pad to a shape
            # with a length-0 slot that crashes GigaAM's mel preprocessor
            # in `transcribe_batch`, taking the whole batch down with it.
            if len(pcm_bytes) == 0:
                print(
                    f"[KTalk Bridge] pcm_chunk empty payload skipped "
                    f"(pid={pid}, seq={seq})",
                    file=sys.stderr,
                )
                return
            if len(pcm_bytes) % 2 != 0:
                print(
                    f"[KTalk Bridge] pcm_chunk odd-length payload skipped "
                    f"(pid={pid}, seq={seq}, bytes={len(pcm_bytes)})",
                    file=sys.stderr,
                )
                return

            # Тайстампы из extension'а приходят в мс JS-клока, переводим
            # в секунды + корректируем clock_offset (см. clock_sync handler).
            chunk_start_ts = chunk_start_ms / 1000.0 + self._clock_offset
            chunk_end_ts = chunk_end_ms / 1000.0 + self._clock_offset

            chunk = PcmChunk(
                pid=pid,
                speaker_name=speaker_name,
                is_local=is_local,
                seq=seq,
                chunk_start_ts=chunk_start_ts,
                chunk_end_ts=chunk_end_ts,
                sample_rate=sample_rate,
                pcm=pcm_bytes,
            )

            if not self._has_active_ktalk_meeting():
                print(
                    f"[KTalk Bridge] pcm_chunk outside active ktalk meeting skipped "
                    f"(pid={pid}, seq={seq})",
                    file=sys.stderr,
                )
                return

            # Per-tab meeting attribution: when two KTalk tabs are open at the
            # same time, a sibling tab's PCM would otherwise be written into
            # the most-recently-joined meeting's storage and contaminate it.
            # If the frame carries a meeting_id that does not match the
            # bridge's current meeting, drop it. Older builds that don't tag
            # the frame still pass the gate (no field == no filter).
            chunk_meeting_id = data.get("meeting_id")
            if isinstance(chunk_meeting_id, str) and chunk_meeting_id:
                current_meeting_id = self._meeting_storage.get_current_meeting_id()
                if current_meeting_id and chunk_meeting_id != current_meeting_id:
                    # Per-pid throttle: a sibling tab fires ~one chunk per
                    # speaker per 3 s, so unthrottled this can flood stderr.
                    last = self._pcm_cross_meeting_drop_log_ts.get(pid, 0.0)
                    now_ts = time.time()
                    if now_ts - last > 10.0:
                        self._pcm_cross_meeting_drop_log_ts[pid] = now_ts
                        print(
                            f"[KTalk Bridge] pcm_chunk cross-meeting dropped "
                            f"(pid={pid}, seq={seq}, chunk={chunk_meeting_id}, "
                            f"current={current_meeting_id})",
                            file=sys.stderr,
                        )
                    return

            if self._on_pcm_chunk is not None:
                try:
                    self._on_pcm_chunk(chunk)
                except Exception as e:
                    print(
                        f"[KTalk Bridge] pcm_chunk callback raised: {e}",
                        file=sys.stderr,
                    )

        elif msg_type == "copilot_channel_hello":
            # Copilot channel client (recog/copilot_channel.py) announces
            # itself. Move the connection out of `_connected_clients` (where
            # `_handle_client` registered it on connect) into the dedicated
            # channel set so that the meeting-lifecycle path
            # ("last extension disconnect -> end meeting") does not count
            # this socket as an extension. Modelled on the `subscriber_hello`
            # handshake.
            if websocket is None:
                print(
                    "[KTalk Bridge] copilot_channel_hello without websocket — ignored",
                    file=sys.stderr,
                )
                return
            with self._lock:
                self._connected_clients.discard(websocket)
                was_first_channel_client = not self._copilot_channel_clients
                self._copilot_channel_clients.add(websocket)
                no_extension_clients = not self._connected_clients
            print(
                f"[KTalk Bridge] Copilot channel client registered from {getattr(websocket, 'remote_address', 'unknown')}",
                file=sys.stderr,
            )
            await self._send_bounded(websocket, json.dumps({
                "type": "copilot_policy_updated",
                **self.user_identity_snapshot(), **self.copilot_policy_snapshot(),
            }, ensure_ascii=False))
            # Race recovery: between this socket's WS-open and
            # hello-processing, it sat in `_connected_clients`. If an
            # extension client disconnected in that window, its
            # finally-block saw `_connected_clients={channel_ws}` and
            # skipped the meeting-end lifecycle. Re-run the check now
            # that the channel client has been unmasked. Same pattern as
            # `subscriber_hello` above.
            if no_extension_clients:
                await self._end_meeting_on_extension_gone()
            # Mirror of the channel-client disconnect path: when the channel
            # transitions from 0 to 1+ clients, broadcast a channel-level
            # `channel_connected` so extensions can clear any prior
            # `channel_disconnected` indicator. Without this signal the
            # connectivity label would stay stuck on "channel offline"
            # forever — `state.copilotChannelStatus` has no other recovery
            # path. Skip when other channel clients were already present
            # to avoid noisy duplicate signals.
            if was_first_channel_client:
                await self._broadcast_copilot_frame({
                    "type": "copilot_status",
                    "request_id": "",
                    "status": "channel_connected",
                    "message": "Copilot channel server connected",
                })

        elif msg_type == "copilot_chat_message":
            # Extension forwards a user-typed chat line. Forward it to every
            # registered Copilot channel client as a `copilot_user_message`
            # frame, enriched with current meeting metadata so the channel
            # server (recog/copilot_channel.py) can attach context for the
            # MCP-connected Claude session. If no channel client is
            # connected we cannot deliver the message — reply to the
            # originating websocket with a `copilot_status` frame so the UI
            # can surface the failure instead of silently swallowing it.
            request_id = data.get("request_id")
            text = data.get("text")
            if not isinstance(request_id, str) or not request_id:
                print(
                    "[KTalk Bridge] copilot_chat_message missing/invalid request_id — ignored",
                    file=sys.stderr,
                )
                return
            if not isinstance(text, str) or not text:
                print(
                    "[KTalk Bridge] copilot_chat_message missing/invalid text — ignored",
                    file=sys.stderr,
                )
                return

            # Stamp the meeting only for conference requests, whose replies
            # legitimately go stale when that meeting ends. A manual/mic-only
            # request has no conference to belong to, and `current_meeting_id`
            # would hand it the orphan meeting — which `_check_silence_split`
            # rotates on its own schedule, dropping the reply as "stale" for a
            # meeting the request never belonged to. `None` here makes the
            # pending entry `""` (the existing no-meeting escape hatch) and
            # leaves the reply-side cross-check nothing to mismatch against.
            # Extension chat is unaffected: it only happens inside a conference.
            meeting_id = None
            if self._meeting_storage is not None and self._is_meeting_active():
                meeting_id = self._meeting_storage.get_current_meeting_id()
            meeting_title = data.get("meeting_title")
            transcript_window = data.get("transcript_window")
            created_at = data.get("created_at")
            if not isinstance(created_at, (int, float)):
                created_at = time.time() * 1000

            with self._lock:
                channel_clients = list(self._copilot_channel_clients)

            if not channel_clients:
                if websocket is not None:
                    status_frame = json.dumps(
                        {
                            "type": "copilot_status",
                            "request_id": request_id,
                            "status": "channel_disconnected",
                            "message": "Copilot channel server is not connected",
                        },
                        ensure_ascii=False,
                    )
                    try:
                        await websocket.send(status_frame)
                    except Exception as e:
                        print(
                            f"[KTalk Bridge] Failed to send channel_disconnected status: {e}",
                            file=sys.stderr,
                        )
                else:
                    print(
                        "[KTalk Bridge] copilot_chat_message dropped — no channel client and no originator socket",
                        file=sys.stderr,
                    )
                return

            outbound = json.dumps(
                {
                    "type": "copilot_user_message",
                    **self.user_identity_snapshot(), **self.copilot_policy_snapshot(),
                    "request_id": request_id,
                    "text": text,
                    "meeting_id": meeting_id,
                    "meeting_title": meeting_title,
                    "transcript_window": transcript_window,
                    "created_at": created_at,
                },
                ensure_ascii=False,
            )
            # Publish request scope before the first channel-send await. A
            # re-entrant meeting_join must see and stale-mark this entry; the
            # resumed relay must not overwrite that marker afterwards.
            # Empty string is used when no meeting is active (the reply will
            # then be relayed normally — there is no meeting to become stale
            # against).
            self._copilot_pending_requests[request_id] = meeting_id or ""

            async def forward(client):
                try:
                    await self._send_bounded(client, outbound)
                except Exception as e:
                    print(
                        f"[KTalk Bridge] Failed to forward copilot_chat_message to channel client: {e}",
                        file=sys.stderr,
                    )

            await asyncio.gather(
                *(forward(client) for client in channel_clients),
                return_exceptions=True,
            )

        elif msg_type == "copilot_channel_reply":
            # Channel server (recog/copilot_channel.py) emits a reply for a
            # previous user request — or a proactive ("live"/"tick") frame.
            # Re-emit as a `copilot_reply` so extension and subscriber
            # clients can render it. The channel client itself was moved
            # out of `_connected_clients` by `copilot_channel_hello`, so
            # `_broadcast_copilot_frame` cannot loop the frame back to its
            # author.
            request_id = data.get("request_id")
            if not isinstance(request_id, str) or not request_id:
                print(
                    "[KTalk Bridge] copilot_channel_reply missing/invalid request_id — ignored",
                    file=sys.stderr,
                )
                return
            text = data.get("text")
            if not isinstance(text, str):
                print(
                    "[KTalk Bridge] copilot_channel_reply missing/invalid text — ignored",
                    file=sys.stderr,
                )
                return
            kind = data.get("kind")
            if not isinstance(kind, str) or not kind:
                print(
                    "[KTalk Bridge] copilot_channel_reply missing/invalid kind — ignored",
                    file=sys.stderr,
                )
                return
            meeting_id = data.get("meeting_id")
            # `None` (and absent) stays valid — replies generated before any
            # meeting lifecycle must still relay. But a non-string value
            # would silently bypass the cross-meeting guard below, so reject
            # it like the sibling validations. The pending entry is left
            # untouched: the frame never entered the reply flow.
            if meeting_id is not None and not isinstance(meeting_id, str):
                print(
                    "[KTalk Bridge] copilot_channel_reply invalid meeting_id type — ignored",
                    file=sys.stderr,
                )
                return
            tracked = self._copilot_pending_requests.get(request_id)
            if isinstance(tracked, str) and tracked.startswith("stale:"):
                print(
                    f"[KTalk Bridge] copilot_channel_reply for stale request_id={request_id} (kind={kind}) — dropped",
                    file=sys.stderr,
                )
                self._copilot_pending_requests.pop(request_id, None)
                # Terminal echo: without it the asking UI waits forever on
                # "Жду ответа Claude…". Both UIs treat per-request `error`
                # as terminal (resolve the placeholder, re-enable input).
                # The message states the actual cause, keyed off the entry's
                # original scope: an empty id means a manual/no-meeting
                # request (e.g. invalidated by `_preempt_manual_copilot`) —
                # never tell a manual-chat user a meeting ended when no
                # meeting ever existed for their question.
                original_scope = tracked[len("stale:"):]
                await self._broadcast_copilot_frame({
                    "type": "copilot_status",
                    "request_id": request_id,
                    "status": "error",
                    "message": (
                        "Ответ не доставлен: встреча, в которой был задан "
                        "вопрос, уже завершилась."
                        if original_scope
                        else "Ответ не доставлен: ручная сессия copilot "
                        "была прервана."
                    ),
                })
                return
            # Proactive replies use a synthetic `proactive_<uuid>` request_id
            # that was never tracked in `_copilot_pending_requests`, so the
            # stale-prefix check above cannot drop them. Cross-check the
            # reply's `meeting_id` against the bridge's current meeting:
            # mismatched ids almost certainly mean the meeting that produced
            # the originating `transcript_tick` already ended. Fall through
            # if either side is missing — replies generated before any
            # meeting lifecycle should still reach the extension.
            if isinstance(meeting_id, str) and meeting_id:
                current_meeting_id = (
                    self._meeting_storage.get_current_meeting_id()
                    if self._meeting_storage is not None
                    else None
                )
                if current_meeting_id and current_meeting_id != meeting_id:
                    print(
                        f"[KTalk Bridge] copilot_channel_reply for "
                        f"meeting_id={meeting_id} but current is "
                        f"{current_meeting_id} (kind={kind}) — dropped",
                        file=sys.stderr,
                    )
                    tracked_value = self._copilot_pending_requests.pop(request_id, None)
                    # Terminal echo only for tracked requests: proactive
                    # `proactive_<uuid>` replies were never recorded, and an
                    # unmatched per-request `error` frame would render a
                    # spurious error bubble in both UIs. No `stale:` strip
                    # needed here: stale-prefixed entries already returned
                    # at the stale-prefix branch above, and nothing awaits
                    # between the two dict reads on the single event loop.
                    if tracked_value is not None:
                        scope = tracked_value
                        await self._broadcast_copilot_frame({
                            "type": "copilot_status",
                            "request_id": request_id,
                            "status": "error",
                            "message": (
                                "Ответ не доставлен: встреча сменилась до "
                                "получения ответа."
                                if scope
                                else "Ответ не доставлен: ручная сессия "
                                "copilot была прервана."
                            ),
                        })
                    return
            created_at = data.get("created_at")
            if not isinstance(created_at, (int, float)):
                created_at = time.time() * 1000
            await self._broadcast_copilot_frame({
                "type": "copilot_reply",
                "request_id": request_id,
                "kind": kind,
                "text": text,
                "meeting_id": meeting_id,
                "created_at": created_at,
            })
            # Each `copilot_chat_message` request gets exactly one reply by
            # protocol; drop the bookkeeping entry now so the dict cannot
            # grow unboundedly across many chat turns. Stale entries are
            # already popped on the early-return path above; proactive
            # replies (no prior chat message) carry a synthetic
            # `proactive_<uuid>` request_id that was never recorded — the
            # `pop` is a no-op for them.
            self._copilot_pending_requests.pop(request_id, None)

        elif msg_type == "copilot_channel_status":
            # Channel server reports per-request status (queued/working/
            # error/done). Re-emit as `copilot_status` to extension and
            # subscriber clients so the UI can update the request indicator.
            request_id = data.get("request_id")
            if not isinstance(request_id, str) or not request_id:
                print(
                    "[KTalk Bridge] copilot_channel_status missing/invalid request_id — ignored",
                    file=sys.stderr,
                )
                return
            status = data.get("status")
            if not isinstance(status, str) or not status:
                print(
                    "[KTalk Bridge] copilot_channel_status missing/invalid status — ignored",
                    file=sys.stderr,
                )
                return
            tracked = self._copilot_pending_requests.get(request_id)
            if isinstance(tracked, str) and tracked.startswith("stale:"):
                print(
                    f"[KTalk Bridge] copilot_channel_status for stale request_id={request_id} (status={status}) — dropped",
                    file=sys.stderr,
                )
                return
            message_text = data.get("message")
            if not isinstance(message_text, str):
                message_text = ""
            await self._broadcast_copilot_frame({
                "type": "copilot_status",
                "request_id": request_id,
                "status": status,
                "message": message_text,
            })
            # Terminal statuses ("done"/"error") signal the channel server
            # is finished with this request even if no `copilot_channel_reply`
            # follows; drop the bookkeeping entry to bound memory across
            # many chat turns. Non-terminal statuses ("queued"/"working")
            # leave the entry in place — the matching reply will pop it.
            if status in {"done", "error"}:
                self._copilot_pending_requests.pop(request_id, None)

        elif msg_type == "get_recent_transcript_request":
            # Channel server (recog/copilot_channel.py) requests a snapshot
            # of the recent transcript buffer for the current meeting. The
            # response is sent only to the originator socket so the buffer
            # cannot leak across meetings or clients.
            request_id = data.get("request_id")
            if not isinstance(request_id, str) or not request_id:
                print(
                    "[KTalk Bridge] get_recent_transcript_request missing/invalid request_id — ignored",
                    file=sys.stderr,
                )
                return
            window_seconds = data.get("window_seconds", 180)
            if not isinstance(window_seconds, (int, float)) or window_seconds < 0:
                window_seconds = 180
            since_segment_id = data.get("since_segment_id")
            if since_segment_id is not None and not isinstance(since_segment_id, int):
                since_segment_id = None
            include_refined = data.get("include_refined", True)
            if not isinstance(include_refined, bool):
                include_refined = True

            current_meeting_id = (
                self._meeting_storage.get_current_meeting_id()
                if self._meeting_storage is not None
                else None
            )
            cutoff_ms = time.time() * 1000 - float(window_seconds) * 1000

            # Copy entries inside the lock — the refine worker thread
            # mutates buffer entries via `send_refine` and the asyncio
            # thread mutates them via `_record_copilot_transcript`. A
            # reference snapshot would let mid-iteration mutations create
            # torn reads (e.g., `refined` flips True between the filter
            # check and the `dict(entry)` copy below). Snapshotting under
            # the lock guarantees a consistent view.
            with self._lock:
                ordered_entries = [
                    dict(self._copilot_transcript_buffer[sid])
                    for sid in self._copilot_transcript_order
                    if sid in self._copilot_transcript_buffer
                ]

            segments = []
            for entry in ordered_entries:
                if entry.get("meeting_id") != current_meeting_id:
                    continue
                if entry.get("end_ms", 0) < cutoff_ms:
                    continue
                if since_segment_id is not None and entry["segment_id"] <= since_segment_id:
                    continue
                if not include_refined and entry.get("refined"):
                    continue
                segments.append(entry)

            response = {
                "type": "get_recent_transcript_response",
                **self.user_identity_snapshot(), **self.copilot_policy_snapshot(),
                "request_id": request_id,
                "segments": segments,
            }
            if websocket is not None:
                try:
                    await websocket.send(json.dumps(response, ensure_ascii=False))
                except Exception as e:
                    print(
                        f"[KTalk Bridge] Failed to send get_recent_transcript_response: {e}",
                        file=sys.stderr,
                    )
            else:
                print(
                    "[KTalk Bridge] get_recent_transcript_request without websocket — response dropped",
                    file=sys.stderr,
                )

        elif msg_type == "copilot_spawn_request":
            # Webapp asks for a manual copilot session (webapp → frontend →
            # bridge → supervisor). The bridge is a pure relay: it never
            # interprets supervisor state, it only mirrors the outcome in
            # `_manual_copilot_active`.
            request_id = data.get("request_id")
            if not isinstance(request_id, str) or not request_id:
                print(
                    "[KTalk Bridge] copilot_spawn_request missing/invalid request_id — ignored",
                    file=sys.stderr,
                )
                return
            async with self._manual_copilot_request_lock:
                if self._is_meeting_active():
                    # Defence in depth (D5): the supervisor reads this frame as
                    # `meeting_started("manual")`, which would kill the session
                    # bound to the live conference. Never forward it.
                    await self._reply_copilot_status(
                        websocket,
                        request_id,
                        "conference_active",
                        "A conference meeting owns the copilot session",
                    )
                    return
                delivered = await self._relay_to_supervisors(data)
                # Re-check the guard: the relay above awaited `client.send`, and a
                # concurrent `meeting_join` can complete a whole conference start
                # in that window. Its `_preempt_manual_copilot()` saw the mirror
                # still False and no-opped, so arming it now would leave the bridge
                # claiming a manual session for the entire conference and past its
                # end. The supervisor is unaffected — it rebinds to the conference
                # id on the `meeting_started` it just received.
                if self._is_meeting_active():
                    print(
                        "[KTalk Bridge] Conference started while relaying "
                        f"copilot_spawn_request request_id={request_id} — manual mirror not armed",
                        file=sys.stderr,
                    )
                    await self._reply_copilot_status(
                        websocket,
                        request_id,
                        "conference_active",
                        "A conference meeting owns the copilot session",
                    )
                    return
                if not delivered:
                    # No supervisor reached → no tmux session. Arming the mirror
                    # here would make a later `supervisor_hello` reconcile as
                    # "no session + active meeting → spawn" and silently start a
                    # session the user was already told had failed.
                    await self._reply_copilot_status(
                        websocket,
                        request_id,
                        "supervisor_disconnected",
                        "Copilot supervisor is not connected",
                    )
                    return
                self._set_manual_copilot_active(True)

        elif msg_type == "copilot_stop_request":
            # Webapp stops the manual session (immediate kill at the
            # supervisor — `stop_now`, no grace). No conference guard: a stray
            # stop is already inert at the supervisor (`stop_now` no-ops on a
            # mismatched lifecycle id).
            request_id = data.get("request_id")
            if not isinstance(request_id, str) or not request_id:
                print(
                    "[KTalk Bridge] copilot_stop_request missing/invalid request_id — ignored",
                    file=sys.stderr,
                )
                return
            async with self._manual_copilot_request_lock:
                # Mirror of the spawn rule: the flag clears even when the relay
                # fails, because there is no session left to keep alive for.
                self._set_manual_copilot_active(False)
                try:
                    if not await self._relay_to_supervisors(data):
                        await self._reply_copilot_status(
                            websocket,
                            request_id,
                            "supervisor_disconnected",
                            "Copilot supervisor is not connected",
                        )
                finally:
                    # A reconnecting supervisor can report its still-live
                    # manual session while the stop relay awaits I/O. The
                    # completed stop is the newer user intent and must win.
                    self._set_manual_copilot_active(False)

        elif msg_type == "supervisor_hello":
            # Copilot supervisor (recog/copilot_supervisor.py) announces
            # itself. Move the connection out of `_connected_clients` into
            # the dedicated supervisor set so the meeting-lifecycle path
            # ("last extension disconnect -> end meeting") does not count it
            # as an extension. Modelled on `copilot_channel_hello`. Reply with
            # `meeting_state` so the supervisor can reconcile after a
            # (re)connect (adopt / spawn / GRACE) instead of blindly killing a
            # live session.
            if websocket is None:
                print(
                    "[KTalk Bridge] supervisor_hello without websocket — ignored",
                    file=sys.stderr,
                )
                return
            with self._lock:
                self._connected_clients.discard(websocket)
                self._supervisor_clients.add(websocket)
                no_extension_clients = not self._connected_clients
            print(
                f"[KTalk Bridge] Copilot supervisor registered from "
                f"{getattr(websocket, 'remote_address', 'unknown')}",
                file=sys.stderr,
            )
            # Race recovery: between this socket's WS-open and hello-processing
            # it sat in `_connected_clients`. If an extension disconnected in
            # that window, its finally-block saw `_connected_clients={sup_ws}`
            # and skipped the meeting-end lifecycle. Re-run the check now that
            # the supervisor has been unmasked. Same pattern as the other
            # hellos — and it must run BEFORE `_compute_meeting_state` so the
            # reply reflects the reconciled (possibly ended) meeting.
            if no_extension_clients:
                await self._end_meeting_on_extension_gone()
            # Re-arm the manual mirror from the supervisor's own live session
            # id (D5) — the supervisor is the single runtime owner, so a bridge
            # restart mid-manual-session recovers here with no persistence.
            # Must run BEFORE `_compute_meeting_state` so the reconcile reply
            # already reads `{active: true, meeting_id: "manual"}` and the
            # supervisor adopts instead of GRACE-killing. Conference wins: a
            # stale manual claim must never displace a live meeting.
            if (
                data.get("current_meeting_id") == MANUAL_MEETING_ID
                and not self._is_meeting_active()
            ):
                self._set_manual_copilot_active(True)
            state_frame = self._compute_meeting_state()
            try:
                await websocket.send(json.dumps(state_frame, ensure_ascii=False))
            except Exception as e:
                print(
                    f"[KTalk Bridge] Failed to send meeting_state to supervisor: {e}",
                    file=sys.stderr,
                )

        elif msg_type == "copilot_agent_status":
            # Supervisor reports the classified TUI screen state
            # (spawning/online/working/blocked/offline). Store it with a
            # monotonic timestamp — the merge treats a stale timestamp (two
            # missed heartbeats) as offline — and recompute the effective chip.
            status = data.get("status")
            if not isinstance(status, str) or not status:
                print(
                    "[KTalk Bridge] copilot_agent_status missing/invalid status — ignored",
                    file=sys.stderr,
                )
                return
            self._supervisor_agent_status = status
            self._supervisor_status_ts = time.monotonic()
            await self._recompute_agent_state()

        elif msg_type == "claude_sessions":
            # Copilot channel daemon reports its live MCP-session count.
            # `null` = unknown (SDK private attr missing) — kept distinct from
            # a confident 0 so an SDK refactor cannot force a false offline.
            count = data.get("count")
            if count is not None and not isinstance(count, int):
                print(
                    "[KTalk Bridge] claude_sessions invalid count — ignored",
                    file=sys.stderr,
                )
                return
            self._set_claude_sessions(count)
            await self._recompute_agent_state()

    async def _run_server(self):
        """Запускает WebSocket сервер."""
        try:
            import websockets
        except ImportError:
            print("[KTalk Bridge] Error: websockets library not installed", file=sys.stderr)
            print("[KTalk Bridge] Install with: pip install websockets", file=sys.stderr)
            return

        try:
            async with websockets.serve(self._handle_client, self.host, self.port):
                print(f"[KTalk Bridge] WebSocket server started on ws://{self.host}:{self.port}", file=sys.stderr)
                self._running = True

                _flush_counter = 0
                while self._running:
                    await asyncio.sleep(0.1)
                    _flush_counter += 1

                    # Каждую ~1 секунду проверяем таймаут сегмента
                    if _flush_counter % 10 == 0:
                        self._check_segment_timeout()
                        # Recompute the agent-state chip so supervisor silence
                        # (crash) flips it to offline even without an incoming
                        # frame to trigger the merge. Broadcasts only on change.
                        await self._recompute_agent_state()

                    # Периодически обновляем debug state даже без событий
                    self._write_debug_state()

        except Exception as e:
            print(f"[KTalk Bridge] Server error: {e}", file=sys.stderr)
        finally:
            self._set_meeting_capture(False)
            # Drain background tasks (e.g. in-flight increment generation)
            # so they are not silently cancelled when the loop closes.
            await self._drain_background_tasks()

    async def _drain_background_tasks(self, timeout: float = 30.0):
        """Await all background tasks with a timeout so in-flight work completes."""
        if not self._background_tasks:
            return
        pending = list(self._background_tasks)
        print(f"[KTalk Bridge] Draining {len(pending)} background task(s)…", file=sys.stderr)
        done, not_done = await asyncio.wait(pending, timeout=timeout)
        for t in not_done:
            t.cancel()
        if not_done:
            # Await cancelled tasks so their CancellationError handlers run
            # before the event loop closes.
            await asyncio.gather(*not_done, return_exceptions=True)
            print(f"[KTalk Bridge] {len(not_done)} background task(s) cancelled after {timeout}s timeout", file=sys.stderr)

    def _thread_main(self):
        """Главная функция фонового потока."""
        self._loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self._loop)
        self._manual_copilot_request_lock = asyncio.Lock()
        self._user_profile_refresh_lock = asyncio.Lock()
        self._ai_settings_refresh_lock = asyncio.Lock()

        try:
            self._loop.run_until_complete(self._run_server())
        finally:
            self._loop.close()

    def start(self):
        """Запускает сервер в фоновом потоке."""
        if self._thread is not None and self._thread.is_alive():
            return

        self._running = True
        self._thread = threading.Thread(target=self._thread_main, daemon=True)
        self._thread.start()

        # Даём серверу время на запуск
        time.sleep(0.5)

    def stop(self):
        """Останавливает сервер."""
        self._running = False
        self._set_meeting_capture(False)

        if self._thread is not None:
            # Must wait at least as long as _drain_background_tasks timeout (30s)
            # so in-flight work (e.g. increment generation) completes before exit.
            self._thread.join(timeout=35.0)
            self._thread = None

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.stop()


def main():
    """Запуск standalone сервера для тестирования."""
    import argparse

    parser = argparse.ArgumentParser(description="KTalk Bridge WebSocket server")
    parser.add_argument("--host", default="localhost", help="Host to bind to")
    parser.add_argument("--port", type=int, default=8766, help="Port to listen on")
    args = parser.parse_args()

    print(f"Starting KTalk Bridge on ws://{args.host}:{args.port}")
    print("Waiting for Chrome extension connection...")
    print("Press Ctrl+C to stop")
    print("-" * 40)

    def on_speaker_change(speaker: Optional[str], is_speaking: bool):
        if is_speaking and speaker:
            print(f"[SPEAKING] {speaker}")
        else:
            print(f"[SILENCE]")

    bridge = KTalkBridge(host=args.host, port=args.port)
    bridge.set_speaker_callback(on_speaker_change)

    try:
        bridge.start()

        # Держим основной поток живым
        while True:
            time.sleep(1)

    except KeyboardInterrupt:
        print("\nShutting down...")
        bridge.stop()


if __name__ == "__main__":
    main()
