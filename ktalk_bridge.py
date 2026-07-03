#!/usr/bin/env python3
"""
KTalk Bridge - WebSocket server для приёма данных о спикерах из Chrome extension.

Запускается параллельно с live транскрипцией и предоставляет real-time
информацию о том, кто сейчас говорит в KTalk конференции.

Использование:
    # Standalone (для тестирования)
    python ktalk_bridge.py --port 8766

    # Как модуль из mp4_notes.py
    from ktalk_bridge import KTalkBridge
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
import os
import re
from dataclasses import dataclass, field
from datetime import datetime
from typing import Optional, Dict, List, Callable, TYPE_CHECKING
import sys
from pathlib import Path
from recog.node_env import get_node_env
from recog.ai_provider import build_codex_exec_command, mark_ccs_automation, resolve_cli
from recog.config import load_dotenv, MIC_SPEAKER_NAME

if TYPE_CHECKING:
    from meeting_storage import MeetingStorage

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

def _truncate_text(text: str, max_len: int) -> str:
    clean = " ".join(text.split())
    if len(clean) <= max_len:
        return clean
    return clean[: max_len - 3].rstrip() + "..."


def _extract_json_payload(text: str) -> Optional[dict]:
    if not text:
        return None
    cleaned = text.strip()

    if cleaned.startswith("```"):
        lines = cleaned.splitlines()
        if lines and lines[0].startswith("```"):
            lines = lines[1:]
        if lines and lines[-1].startswith("```"):
            lines = lines[:-1]
        cleaned = "\n".join(lines).strip()

    start = cleaned.find("{")
    end = cleaned.rfind("}")
    if start != -1 and end != -1 and end > start:
        cleaned = cleaned[start:end + 1]

    try:
        return json.loads(cleaned)
    except json.JSONDecodeError:
        return None


def _parse_anchor_lines(transcript: str) -> list[dict]:
    lines = []
    if not transcript:
        return lines

    pattern = re.compile(r"^\[(\d{2}:\d{2}:\d{2})\]\s*([^:]+):\s*(.*)$")
    for raw in transcript.splitlines():
        raw = raw.strip()
        if not raw:
            continue
        match = pattern.match(raw)
        if match:
            ts_display, speaker, text = match.groups()
            lines.append({
                "ts_display": ts_display,
                "speaker": speaker.strip(),
                "text": text.strip()
            })
        else:
            lines.append({
                "ts_display": "",
                "speaker": "",
                "text": raw
            })
    return lines


def _detect_user_mention(lines: list[dict], user_name: str) -> tuple[bool, str, str]:
    if not user_name:
        return False, "", ""

    names = [n.strip().lower() for n in re.split(r"[;,]", user_name) if n.strip()]
    if not names:
        return False, "", ""

    for line in reversed(lines):
        text = line.get("text", "")
        text_lower = text.lower()
        for name in names:
            if name and name in text_lower:
                return True, _truncate_text(text, 160), line.get("ts_display", "")

    return False, "", ""


def _fallback_anchor_payload(transcript: str, user_name: str) -> dict:
    lines = _parse_anchor_lines(transcript)
    current_text = lines[-1]["text"] if lines else ""
    current_topic = _truncate_text(current_text, 140) if current_text else "No recent transcript."

    bullets = []
    for line in lines[-3:]:
        text = line.get("text") or ""
        if not text:
            continue
        bullets.append({
            "text": _truncate_text(text, 120),
            "ts_display": line.get("ts_display", "")
        })

    mentioned, mention_context, mention_ts = _detect_user_mention(lines, user_name)

    return {
        "current_topic": current_topic,
        "topic_tone": "neutral",
        "summary_bullets": bullets if bullets else [{"text": "No recent transcript.", "ts_display": ""}],
        "user_mentioned": mentioned,
        "mention_context": mention_context if mentioned else "",
        "mention_ts_display": mention_ts if mentioned else ""
    }


def _normalize_anchor_payload(payload: dict, transcript: str, user_name: str) -> tuple[dict, bool]:
    approximate = False
    if not isinstance(payload, dict):
        return _fallback_anchor_payload(transcript, user_name), True

    current_topic = str(payload.get("current_topic") or "").strip()
    if not current_topic:
        approximate = True
        current_topic = _fallback_anchor_payload(transcript, user_name)["current_topic"]

    topic_tone = str(payload.get("topic_tone") or "").strip().lower()
    if topic_tone not in {"neutral", "calm", "heated"}:
        topic_tone = "neutral"
        approximate = True

    raw_bullets = payload.get("summary_bullets") or []
    bullets = []
    for item in raw_bullets:
        if isinstance(item, dict):
            text = str(item.get("text") or "").strip()
            ts_display = str(item.get("ts_display") or "").strip()
        else:
            text = str(item).strip()
            ts_display = ""
        if text:
            bullets.append({"text": text, "ts_display": ts_display})

    if not bullets:
        approximate = True
        bullets = _fallback_anchor_payload(transcript, user_name)["summary_bullets"]

    if len(bullets) > 3:
        bullets = bullets[:3]

    user_mentioned = payload.get("user_mentioned")
    mention_context = str(payload.get("mention_context") or "").strip()
    mention_ts_display = str(payload.get("mention_ts_display") or "").strip()

    if not isinstance(user_mentioned, bool):
        approximate = True
        user_mentioned, mention_context, mention_ts_display = _detect_user_mention(
            _parse_anchor_lines(transcript),
            user_name
        )
    elif user_mentioned and not mention_context:
        approximate = True
        _, mention_context, mention_ts_display = _detect_user_mention(
            _parse_anchor_lines(transcript),
            user_name
        )

    if not user_mentioned:
        mention_context = ""
        mention_ts_display = ""

    return {
        "current_topic": current_topic,
        "topic_tone": topic_tone,
        "summary_bullets": bullets,
        "user_mentioned": user_mentioned,
        "mention_context": mention_context,
        "mention_ts_display": mention_ts_display
    }, approximate


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

    def __init__(self, host: str = "localhost", port: int = 8766):
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
        self._server = None
        self._thread: Optional[threading.Thread] = None
        self._running = False
        self._loop = None
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

        # Agent-state merge (D4). Last status reported by the supervisor
        # (screen classification) and by the copilot channel daemon (live MCP
        # session count), reduced to one effective chip status. `None` status
        # = no supervisor report yet; `None` session count = unknown (SDK
        # private attr missing) — merge must not treat it as a confident 0.
        self._supervisor_agent_status: Optional[str] = None
        self._supervisor_status_ts: float = 0.0  # time.monotonic of last report
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
        """Return True only while current storage points at a KTalk meeting."""
        if not self._meeting_storage or not self._meeting_storage.current_meeting_path:
            return False
        try:
            meta = self._meeting_storage._load_meta()
        except Exception as e:
            print(f"[KTalk Bridge] pcm_chunk meeting guard failed: {e}", file=sys.stderr)
            return False
        return bool(meta and meta.get("source") == "ktalk")

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
            "local_user_name": MIC_SPEAKER_NAME,
        }
        message = json.dumps(frame, ensure_ascii=False)
        for client in recipients:
            try:
                asyncio.run_coroutine_threadsafe(client.send(message), self._loop)
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

    async def _broadcast_message(self, data: dict):
        """Отправляет сообщение всем подключённым клиентам."""
        if not self._connected_clients:
            return

        message = json.dumps(data, ensure_ascii=False)
        for client in list(self._connected_clients):
            try:
                await client.send(message)
            except Exception:
                pass  # Клиент может отключиться

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
        for client in channel_clients:
            try:
                await client.send(message)
            except Exception as e:
                print(
                    f"[KTalk Bridge] Failed to send {data.get('type')} to channel client: {e}",
                    file=sys.stderr,
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
        await self._broadcast_to_channel_clients(frame)
        await self._broadcast_message(frame)
        with self._lock:
            supervisors = list(self._supervisor_clients)
        if not supervisors:
            return
        message = json.dumps(frame, ensure_ascii=False)
        for client in supervisors:
            try:
                await client.send(message)
            except Exception as e:
                print(
                    f"[KTalk Bridge] Failed to send {frame.get('type')} to supervisor: {e}",
                    file=sys.stderr,
                )

    def _is_meeting_active(self) -> bool:
        """True when a KTalk meeting is genuinely in progress.

        Deliberately NOT `current_meeting_path is not None` — that field is
        never None (`on_ktalk_leave` immediately creates an orphan). A meeting
        is active only when the current meeting was opened by KTalk
        (`meta["source"] == "ktalk"`) AND at least one extension client is
        connected to drive it.
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
        if not (meta and meta.get("source") == "ktalk"):
            return False
        with self._lock:
            return bool(self._connected_clients)

    def _compute_meeting_state(self) -> dict:
        """Build the `meeting_state` reconciliation reply for `supervisor_hello`.

        `active` follows `_is_meeting_active`; meeting_id / meeting_title are
        only populated when active so a stale orphan meeting never looks live
        to the supervisor.
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
        live sessions AND the supervisor is past `spawning` AND a meeting is
        active. Anchoring `_sessions_zero_since` to when that combined
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
            and self._is_meeting_active()
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
        # Daemon reports a confident zero live MCP sessions during an active
        # meeting while the supervisor is past `spawning`: the TUI is up but
        # the copilot never connected. Debounced so leaving `spawning` before
        # the daemon's first poll does not flap the chip. `None` count is
        # unknown (not a confident 0) and never forces offline here.
        if (
            self._claude_sessions_count == 0
            and status != "spawning"
            and self._sessions_zero_since is not None
            and (now - self._sessions_zero_since) >= AGENT_SESSIONS_ZERO_DEBOUNCE_S
            and self._is_meeting_active()
        ):
            return "offline"
        return status

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
        if effective == self._last_effective_agent_state:
            return
        self._last_effective_agent_state = effective
        await self._broadcast_message({
            "type": "copilot_status",
            "request_id": "",
            "status": "agent_state",
            "agent_state": effective,
        })

    async def _call_codex_cli(
        self,
        prompt: str,
        model: str = "gpt-5.5",
        task: str = "analysis",
    ) -> str:
        """Call Codex CLI (uses ChatGPT Pro subscription)."""
        codex_path = resolve_cli("codex")
        cmd = build_codex_exec_command(codex_path, model, "xhigh")
        marked_prompt = mark_ccs_automation(prompt, task)
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env()
        )
        stdout, stderr = await proc.communicate(input=marked_prompt.encode())

        if proc.returncode != 0:
            raise Exception(f"Codex CLI error: {stderr.decode().strip()}")

        for line in stdout.decode().strip().split('\n'):
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get("type") == "item.completed":
                item = event.get("item", {})
                if item.get("type") == "agent_message":
                    return item.get("text", "")

        raise Exception("No response from Codex")

    async def _call_claude_cli(
        self,
        prompt: str,
        model: str = "opus",
        task: str = "analysis",
    ) -> str:
        """Call Claude CLI (uses Max/Pro subscription)."""
        claude_path = resolve_cli("claude")
        marked_prompt = mark_ccs_automation(prompt, task)
        proc = await asyncio.create_subprocess_exec(
            claude_path, "-p", marked_prompt,
            "--model", model,
            "--output-format", "text",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=get_node_env()
        )
        stdout, stderr = await proc.communicate()

        if proc.returncode != 0:
            raise Exception(f"Claude CLI error: {stderr.decode().strip()}")

        return stdout.decode().strip()

    async def _handle_summarize(self, transcript: str, summary_type: str = "catchup", question: str = "", previous_summary: str = "", model: str = "gemini-2.5-flash", meeting_title: str = ""):
        """Обрабатывает запрос на AI summarize через Gemini API, Codex CLI или Claude CLI."""
        try:
            if not transcript.strip():
                await self._broadcast_message({
                    "type": "summarize_response",
                    "error": "Нет транскрипта для анализа"
                })
                return

            # Meeting context
            meeting_context = f"Встреча: {meeting_title}\n\n" if meeting_title else ""

            # Build prompt based on summary type
            if summary_type == "catchup":
                prompt = f"""Ты помощник Вячеслава Ковалева на рабочей встрече. Он отвлёкся на минуту и хочет быстро включиться в дискуссию.

{meeting_context}Проанализируй транскрипт и помоги ему:

1. **Контекст** (2-3 предложения): Что сейчас обсуждают? Какая проблема или решение на столе?

2. **Один умный вопрос**: Предложи один вопрос, который Вячеслав может задать прямо сейчас. Вопрос должен:
   - Показать, что он следит за дискуссией
   - Продвинуть обсуждение вперёд
   - Быть из одной из категорий:
     * Исследовательский: "А мы проверяли...?", "Какие данные говорят...?"
     * Спекулятивный: "Что если...?", "А можно ли...?"
     * Продуктивный: "Какой следующий шаг?", "Кто возьмёт на себя...?"
     * Интерпретационный: "То есть это значит, что...?", "Правильно ли я понимаю...?"
     * Субъективный: "Все согласны с этим подходом?", "Есть ли сомнения?"

Формат ответа — кратко, без воды:

## Контекст
[2-3 предложения]

## Вопрос
> [Один конкретный вопрос который можно задать прямо сейчас]

Транскрипт:
{transcript}
"""
            elif summary_type == "brief":
                prompt = f"""{meeting_context}Дай краткое изложение того, что обсуждалось на встрече за последние 2 минуты.

Формат — кратко и по делу:

## Темы
- Что обсуждали (буллеты)

## Решения
- Какие решения приняли или к чему склоняются

## Открытые вопросы
- Что осталось нерешённым

Транскрипт:
{transcript}
"""
            elif summary_type == "full":
                prompt = f"""{meeting_context}Дай полное summary встречи.

Формат ответа:

## Участники
- Кто участвовал в обсуждении

## Основные темы
- Что обсуждали (по пунктам)

## Ключевые решения
- Что решили

## Action items
- Кто что взял на себя (если упоминалось)

## Открытые вопросы
- Что осталось нерешённым

Транскрипт:
{transcript}
"""
            elif summary_type == "question":
                prompt = f"""{meeting_context}Контекст встречи (summary):
{previous_summary}

Полный транскрипт:
{transcript}

Вопрос пользователя: {question}

Ответь на вопрос кратко и по существу, основываясь на транскрипте встречи.
"""
            else:
                prompt = f"Кратко изложи:\n{transcript}"

            # Log prompt to file for debugging
            with open("/tmp/ktalk_summarize_prompt.txt", "w") as f:
                f.write(f"Model: {model}\n")
                f.write(f"Meeting: {meeting_title}\n")
                f.write(f"Type: {summary_type}\n")
                f.write(f"---\n{prompt}\n")

            # Route to appropriate provider based on model prefix
            summary = None

            if model.startswith("codex"):
                # Codex CLI (uses ChatGPT Pro subscription)
                codex_model = model.split(":")[1] if ":" in model else "gpt-5.5"
                print(f"[KTalk Bridge] Using Codex CLI with model: {codex_model}", file=sys.stderr)
                summary = await self._call_codex_cli(prompt, codex_model, task="summarize")

            elif model.startswith("claude"):
                # Claude CLI (uses Max/Pro subscription)
                claude_model = model.split(":")[1] if ":" in model else "opus"
                print(f"[KTalk Bridge] Using Claude CLI with model: {claude_model}", file=sys.stderr)
                summary = await self._call_claude_cli(prompt, claude_model, task="summarize")

            else:
                # Gemini API (default)
                api_key = os.getenv("GEMINI_API_KEY") or os.getenv("GOOGLE_API_KEY")
                if not api_key:
                    await self._broadcast_message({
                        "type": "summarize_response",
                        "error": "GEMINI_API_KEY не установлен"
                    })
                    return

                url = f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"

                import aiohttp
                async with aiohttp.ClientSession() as session:
                    # Pro models use thinking tokens, need higher limit
                    max_tokens = 8192 if "pro" in model else 2048

                    async with session.post(
                        url,
                        json={
                            "contents": [{"parts": [{"text": prompt}]}],
                            "generationConfig": {
                                "temperature": 0.3,
                                "maxOutputTokens": max_tokens
                            }
                        },
                        headers={"Content-Type": "application/json"},
                        timeout=aiohttp.ClientTimeout(total=60 if "pro" in model else 30)
                    ) as response:
                        if response.status != 200:
                            error_text = await response.text()
                            print(f"[KTalk Bridge] Gemini error: {error_text}", file=sys.stderr)
                            await self._broadcast_message({
                                "type": "summarize_response",
                                "error": f"Gemini API error: {response.status}"
                            })
                            return

                        data = await response.json()

                # Extract text from response
                candidates = data.get("candidates", [])
                if not candidates:
                    print(f"[KTalk Bridge] No candidates in response: {data}", file=sys.stderr)
                    await self._broadcast_message({
                        "type": "summarize_response",
                        "error": f"No candidates: {data.get('error', data.get('promptFeedback', 'unknown'))}"
                    })
                    return

                # Check finish reason
                finish_reason = candidates[0].get("finishReason", "")
                if finish_reason and finish_reason not in ["STOP", "MAX_TOKENS"]:
                    print(f"[KTalk Bridge] Unexpected finish reason: {finish_reason}", file=sys.stderr)
                    await self._broadcast_message({
                        "type": "summarize_response",
                        "error": f"Gemini blocked: {finish_reason}"
                    })
                    return

                summary = candidates[0].get("content", {}).get("parts", [{}])[0].get("text", "")

            if not summary:
                await self._broadcast_message({
                    "type": "summarize_response",
                    "error": "Пустой ответ от модели"
                })
                return

            print(f"[KTalk Bridge] Summary generated ({model}): {len(summary)} chars", file=sys.stderr)

            await self._broadcast_message({
                "type": "summarize_response",
                "summary": summary
            })

        except ImportError:
            await self._broadcast_message({
                "type": "summarize_response",
                "error": "aiohttp не установлен. Выполните: pip install aiohttp"
            })
        except Exception as e:
            print(f"[KTalk Bridge] Summarize error: {e}", file=sys.stderr)
            await self._broadcast_message({
                "type": "summarize_response",
                "error": str(e)
            })

    async def _handle_anchor(self, transcript: str, user_name: str = "User", model: str = "codex", meeting_title: str = ""):
        """Обрабатывает запрос на WDIM/Context через Codex CLI, Claude CLI или Gemini API."""
        try:
            if not transcript.strip():
                await self._broadcast_message({
                    "type": "anchor_response",
                    "error": "Нет транскрипта для анализа"
                })
                return

            if not user_name:
                user_name = "User"

            meeting_context = f"Встреча: {meeting_title}\n\n" if meeting_title else ""

            prompt = f"""Ты — ассистент для быстрого восстановления контекста встречи.

{meeting_context}Участник: {user_name}

Вход: транскрипт последних минут. Формат строк:
[HH:MM:SS] Speaker: text

Твоя задача:
1) current_topic: 1 короткая фраза, что обсуждают прямо сейчас.
2) topic_tone: одно из "neutral" | "calm" | "heated".
3) summary_bullets: 3 пункта (каждый до 10 слов) + ts_display из транскрипта.
4) user_mentioned: true/false если обращались к участнику или назначали задачи.
5) mention_context: короткий контекст (если user_mentioned=true).
6) mention_ts_display: ts_display (если user_mentioned=true).

Если данных недостаточно — не выдумывай, заполни безопасными значениями.

Верни строго валидный JSON без markdown и комментариев:
{{
  "current_topic": "...",
  "topic_tone": "neutral|calm|heated",
  "summary_bullets": [
    {{"text": "...", "ts_display": "HH:MM:SS"}}
  ],
  "user_mentioned": false,
  "mention_context": "",
  "mention_ts_display": ""
}}

Транскрипт:
{transcript}
"""

            with open("/tmp/ktalk_anchor_prompt.txt", "w") as f:
                f.write(f"Model: {model}\n")
                f.write(f"Meeting: {meeting_title}\n")
                f.write(f"---\n{prompt}\n")

            # Route to appropriate provider based on model prefix
            raw_text = None

            if model.startswith("codex"):
                # Codex CLI (uses ChatGPT Pro subscription)
                codex_model = model.split(":")[1] if ":" in model else "gpt-5.5"
                print(f"[KTalk Bridge] Anchor using Codex CLI with model: {codex_model}", file=sys.stderr)
                raw_text = await self._call_codex_cli(prompt, codex_model, task="anchor")

            elif model.startswith("claude"):
                # Claude CLI (uses Max/Pro subscription)
                claude_model = model.split(":")[1] if ":" in model else "opus"
                print(f"[KTalk Bridge] Anchor using Claude CLI with model: {claude_model}", file=sys.stderr)
                raw_text = await self._call_claude_cli(prompt, claude_model, task="anchor")

            else:
                # Gemini API (default for gemini-* models)
                if not model.startswith("gemini-"):
                    model = "gemini-3-pro-preview"

                api_key = os.getenv("GEMINI_API_KEY") or os.getenv("GOOGLE_API_KEY")
                if not api_key:
                    await self._broadcast_message({
                        "type": "anchor_response",
                        "error": "GEMINI_API_KEY не установлен"
                    })
                    return

                url = f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"

                import aiohttp
                async with aiohttp.ClientSession() as session:
                    max_tokens = 2048 if "pro" in model else 1024

                    async with session.post(
                        url,
                        json={
                            "contents": [{"parts": [{"text": prompt}]}],
                            "generationConfig": {
                                "temperature": 0.2,
                                "maxOutputTokens": max_tokens
                            }
                        },
                        headers={"Content-Type": "application/json"},
                        timeout=aiohttp.ClientTimeout(total=60 if "pro" in model else 30)
                    ) as response:
                        if response.status != 200:
                            error_text = await response.text()
                            print(f"[KTalk Bridge] Gemini anchor error: {error_text}", file=sys.stderr)
                            await self._broadcast_message({
                                "type": "anchor_response",
                                "error": f"Gemini API error: {response.status}"
                            })
                            return

                        data = await response.json()

                candidates = data.get("candidates", [])
                if not candidates:
                    print(f"[KTalk Bridge] Anchor no candidates: {data}", file=sys.stderr)
                    await self._broadcast_message({
                        "type": "anchor_response",
                        "error": "No candidates from Gemini"
                    })
                    return

                finish_reason = candidates[0].get("finishReason", "")
                if finish_reason and finish_reason not in ["STOP", "MAX_TOKENS"]:
                    print(f"[KTalk Bridge] Anchor finish reason: {finish_reason}", file=sys.stderr)
                    await self._broadcast_message({
                        "type": "anchor_response",
                        "error": f"Gemini blocked: {finish_reason}"
                    })
                    return

                raw_text = candidates[0].get("content", {}).get("parts", [{}])[0].get("text", "")

            if not raw_text:
                await self._broadcast_message({
                    "type": "anchor_response",
                    "error": "Пустой ответ от AI"
                })
                return

            payload = _extract_json_payload(raw_text)
            if payload is None:
                normalized = _fallback_anchor_payload(transcript, user_name)
                approximate = True
            else:
                normalized, approximate = _normalize_anchor_payload(payload, transcript, user_name)

            await self._broadcast_message({
                "type": "anchor_response",
                "data": normalized,
                "approximate": approximate,
                "model": model
            })

        except ImportError:
            await self._broadcast_message({
                "type": "anchor_response",
                "error": "aiohttp не установлен. Выполните: pip install aiohttp"
            })
        except Exception as e:
            print(f"[KTalk Bridge] Anchor error: {e}", file=sys.stderr)
            await self._broadcast_message({
                "type": "anchor_response",
                "error": str(e)
            })

    async def _handle_client(self, websocket):
        """Обрабатывает подключение клиента (Chrome extension или subscriber)."""
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
        # whitelist and `frontend/bridge_subscriber.py` only forwards
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
                        # clock_sync resets. No further messages are
                        # expected from a subscriber; the loop remains open
                        # to relay server→client broadcasts until close.
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
                            await self._end_meeting_on_last_extension_gone()
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
                # Release meeting ownership if the owner WS just dropped —
                # otherwise the rejection gate would permanently block every
                # subsequent meeting_join from a fresh tab (the dead WS
                # reference would still match nothing in _connected_clients
                # but the comparison check happens before that read).
                if websocket is self._meeting_owner_ws:
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
                await self._broadcast_message({
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
            if no_extension_clients:
                await self._end_meeting_on_last_extension_gone()

    async def _end_meeting_on_last_extension_gone(self) -> None:
        """Run the end-of-meeting cleanup when no extension clients remain.

        Idempotent: safe to call when no meeting is active (the inner
        checks skip the cleanup). Called from two sites:
        1. `_handle_client.finally` when an extension closes and leaves
           `_connected_clients` empty.
        2. the `subscriber_hello` handler, to recover from the race where
           an extension disconnected while this subscriber was still
           transiently counted in `_connected_clients` (between WS-open
           and hello-processing), causing the extension's finally-block
           to skip the cleanup.

        Mirrors the explicit `meeting_leave` Copilot cleanup so a hard
        extension disconnect (page reload, crash, network drop) cannot
        leak tick state, transcript-buffer entries, or in-flight
        `copilot_chat_message` request bookkeeping into the next meeting.
        """
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
        if not (meta and meta.get("source") == "ktalk"):
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
            # Extensions are already gone here (this path fires on the last
            # extension disconnect), but `_emit_lifecycle` still reaches
            # channel clients and the supervisor; the empty extension
            # broadcast is a harmless no-op.
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

        if msg_type == "speaker_update":
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
                self._clear_speaker_state()
                print("[KTalk Bridge] Conference disconnected", file=sys.stderr)
            else:
                print("[KTalk Bridge] Conference connected", file=sys.stderr)

        elif msg_type == "meeting_join":
            # Extension сообщает о входе во встречу (KTalk, Jitsi, etc.)
            url = data.get("url")
            title = data.get("title")
            platform = data.get("platform", "ktalk")
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
                became_active = not path_changed and prev_source == "orphan"
                if path_changed or became_active:
                    if path_changed and self._on_meeting_change:
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
                    await self._emit_lifecycle(meeting_started_frame)
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

        elif msg_type == "summarize_request":
            # AI summary request from extension
            transcript = data.get("transcript", "")
            summary_type = data.get("summary_type", "catchup")
            lines_count = data.get("lines_count", 0)
            question = data.get("question", "")
            previous_summary = data.get("previous_summary", "")
            model = data.get("model", "codex")
            meeting_title = data.get("meeting_title", "")
            print(f"[KTalk Bridge] Summarize request ({summary_type}): {lines_count} lines, model: {model}, meeting: {meeting_title[:50]}", file=sys.stderr)

            # Run summarize in background
            asyncio.create_task(self._handle_summarize(transcript, summary_type, question, previous_summary, model, meeting_title))
        elif msg_type == "anchor_request":
            transcript = data.get("transcript", "")
            lines_count = data.get("lines_count", 0)
            window_seconds = data.get("window_seconds", 0)
            model = data.get("model", "gemini-3-pro-preview")
            user_name = data.get("user_name", "User")
            meeting_title = data.get("meeting_title", "")
            print(f"[KTalk Bridge] Anchor request: {lines_count} lines, window: {window_seconds}s, model: {model}, meeting: {meeting_title[:50]}", file=sys.stderr)

            asyncio.create_task(self._handle_anchor(transcript, user_name, model, meeting_title))

        elif msg_type == "pcm_chunk":
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
            # Race recovery: between this socket's WS-open and
            # hello-processing, it sat in `_connected_clients`. If an
            # extension client disconnected in that window, its
            # finally-block saw `_connected_clients={channel_ws}` and
            # skipped the meeting-end lifecycle. Re-run the check now
            # that the channel client has been unmasked. Same pattern as
            # `subscriber_hello` above.
            if no_extension_clients:
                await self._end_meeting_on_last_extension_gone()
            # Mirror of the channel-client disconnect path: when the channel
            # transitions from 0 to 1+ clients, broadcast a channel-level
            # `channel_connected` so extensions can clear any prior
            # `channel_disconnected` indicator. Without this signal the
            # connectivity label would stay stuck on "channel offline"
            # forever — `state.copilotChannelStatus` has no other recovery
            # path. Skip when other channel clients were already present
            # to avoid noisy duplicate signals.
            if was_first_channel_client:
                await self._broadcast_message({
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

            meeting_id = (
                self._meeting_storage.get_current_meeting_id()
                if self._meeting_storage is not None
                else None
            )
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
                    "request_id": request_id,
                    "text": text,
                    "meeting_id": meeting_id,
                    "meeting_title": meeting_title,
                    "transcript_window": transcript_window,
                    "created_at": created_at,
                },
                ensure_ascii=False,
            )
            for client in channel_clients:
                try:
                    await client.send(outbound)
                except Exception as e:
                    print(
                        f"[KTalk Bridge] Failed to forward copilot_chat_message to channel client: {e}",
                        file=sys.stderr,
                    )
            # Record request_id -> meeting_id so a late reply that
            # arrives after `meeting_leave` can be filtered out.
            # Empty string is used when no meeting is active (the reply
            # will then be relayed normally — there is no meeting to
            # become stale against).
            self._copilot_pending_requests[request_id] = meeting_id or ""

        elif msg_type == "copilot_channel_reply":
            # Channel server (recog/copilot_channel.py) emits a reply for a
            # previous user request — or a proactive ("live"/"tick") frame.
            # Re-emit as a `copilot_reply` so extension clients can render
            # it in the Copilot tab. The channel client itself was moved
            # out of `_connected_clients` by `copilot_channel_hello`, so
            # `_broadcast_message` cannot loop the frame back to its
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
            tracked = self._copilot_pending_requests.get(request_id)
            if isinstance(tracked, str) and tracked.startswith("stale:"):
                print(
                    f"[KTalk Bridge] copilot_channel_reply for stale request_id={request_id} (kind={kind}) — dropped",
                    file=sys.stderr,
                )
                self._copilot_pending_requests.pop(request_id, None)
                return
            meeting_id = data.get("meeting_id")
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
                    self._copilot_pending_requests.pop(request_id, None)
                    return
            created_at = data.get("created_at")
            if not isinstance(created_at, (int, float)):
                created_at = time.time() * 1000
            await self._broadcast_message({
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
            # error/done). Re-emit as `copilot_status` to extension
            # clients so the UI can update the request indicator.
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
            await self._broadcast_message({
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
                await self._end_meeting_on_last_extension_gone()
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
