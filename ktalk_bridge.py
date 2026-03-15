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

if TYPE_CHECKING:
    from meeting_storage import MeetingStorage

# Load .env file
def _load_dotenv():
    env_path = Path(__file__).parent / ".env"
    if not env_path.exists():
        return
    with open(env_path, 'r') as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            if '=' in line:
                key, value = line.split('=', 1)
                os.environ.setdefault(key.strip(), value.strip())

_load_dotenv()


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

        # Meeting storage для сохранения транскриптов
        self._meeting_storage: Optional['MeetingStorage'] = None

        # Debug state file
        self._debug_file = Path("/tmp/ktalk_debug.json")
        self._debug_update_interval = 1.0  # seconds
        self._last_debug_update = 0.0

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

    def set_meeting_storage(self, storage: 'MeetingStorage'):
        """
        Устанавливает MeetingStorage для сохранения транскриптов.

        Args:
            storage: экземпляр MeetingStorage
        """
        self._meeting_storage = storage

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
                        "all_mics_muted": all(p.mic_muted for p in self._participants.values()) if self._participants else True
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
                    }
                }

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

    def send_transcript(self, speaker: str, text: str, timestamp: str, is_continuation: bool = False, is_local: bool = False, uncertain: bool = False, uncertain_reason: str = ""):
        """Отправляет транскрипт всем подключённым клиентам."""
        message = json.dumps({
            "type": "transcript",
            "speaker": speaker,
            "text": text,
            "timestamp": timestamp,
            "is_continuation": is_continuation,
            "is_local": is_local,  # True = микрофон (я), False = системный звук (другие)
            "uncertain": uncertain,  # True = непонятно кто говорит
            "uncertain_reason": uncertain_reason  # "local" = системный звук + локальный спикер, "mics_off" = все микрофоны выключены
        })

        # Отправляем асинхронно через event loop
        if self._loop and self._connected_clients:
            for client in list(self._connected_clients):
                try:
                    asyncio.run_coroutine_threadsafe(
                        client.send(message),
                        self._loop
                    )
                except Exception:
                    pass  # Клиент может отключиться

    async def _send_transcript_history(self):
        """Отправляет историю транскрипта текущей встречи всем подключённым клиентам."""
        if not self._meeting_storage:
            return

        segments = self._meeting_storage.get_current_transcript()
        meeting_id = self._meeting_storage.get_current_meeting_id()

        await self._broadcast_message({
            "type": "transcript_history",
            "meeting_id": meeting_id,
            "segments": segments
        })

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

    def _get_node_env(self) -> dict:
        """Get environment with node/nvm paths for subprocess."""
        import shutil
        env = os.environ.copy()

        # If node already in PATH, use current env
        if shutil.which("node"):
            return env

        # Find nvm node directory and add to PATH
        nvm_locations = [
            Path.home() / ".local/share/nvm",
            Path.home() / ".nvm/versions/node",
        ]
        for nvm_base in nvm_locations:
            if nvm_base.exists():
                for version_dir in sorted(nvm_base.iterdir(), reverse=True):
                    node_bin = version_dir / "bin"
                    if (node_bin / "node").exists():
                        env["PATH"] = f"{node_bin}:{env.get('PATH', '')}"
                        return env
        return env

    def _find_cli_binary(self, name: str) -> str:
        """Find CLI binary path, checking common locations."""
        import shutil
        # Try PATH first
        path = shutil.which(name)
        if path:
            return path
        # Check ~/.local/bin (standard location for CLI tools like claude)
        local_bin = Path.home() / ".local/bin" / name
        if local_bin.exists():
            return str(local_bin)
        # Check common nvm locations
        nvm_locations = [
            Path.home() / ".local/share/nvm",
            Path.home() / ".nvm/versions/node",
        ]
        for nvm_base in nvm_locations:
            if nvm_base.exists():
                for version_dir in sorted(nvm_base.iterdir(), reverse=True):
                    candidate = version_dir / "bin" / name
                    if candidate.exists():
                        return str(candidate)
        raise FileNotFoundError(f"{name} not found in PATH, ~/.local/bin, or nvm")

    async def _call_codex_cli(self, prompt: str, model: str = "gpt-5.4") -> str:
        """Call Codex CLI (uses ChatGPT Pro subscription)."""
        codex_path = self._find_cli_binary("codex")
        proc = await asyncio.create_subprocess_exec(
            codex_path, "exec",
            "-m", model,
            "-c", "model_reasoning_effort=xhigh",
            "--json",
            "-",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=self._get_node_env()
        )
        stdout, stderr = await proc.communicate(input=prompt.encode())

        if proc.returncode != 0:
            raise Exception(f"Codex CLI error: {stderr.decode().strip()}")

        for line in stdout.decode().strip().split('\n'):
            if not line:
                continue
            event = json.loads(line)
            if event.get("type") == "item.completed":
                item = event.get("item", {})
                if item.get("type") == "agent_message":
                    return item.get("text", "")

        raise Exception("No response from Codex")

    async def _call_claude_cli(self, prompt: str, model: str = "opus") -> str:
        """Call Claude CLI (uses Max/Pro subscription)."""
        claude_path = self._find_cli_binary("claude")
        proc = await asyncio.create_subprocess_exec(
            claude_path, "-p", prompt,
            "--model", model,
            "--output-format", "text",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=self._get_node_env()
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
                codex_model = model.split(":")[1] if ":" in model else "gpt-5.4"
                print(f"[KTalk Bridge] Using Codex CLI with model: {codex_model}", file=sys.stderr)
                summary = await self._call_codex_cli(prompt, codex_model)

            elif model.startswith("claude"):
                # Claude CLI (uses Max/Pro subscription)
                claude_model = model.split(":")[1] if ":" in model else "opus"
                print(f"[KTalk Bridge] Using Claude CLI with model: {claude_model}", file=sys.stderr)
                summary = await self._call_claude_cli(prompt, claude_model)

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
                codex_model = model.split(":")[1] if ":" in model else "gpt-5.4"
                print(f"[KTalk Bridge] Anchor using Codex CLI with model: {codex_model}", file=sys.stderr)
                raw_text = await self._call_codex_cli(prompt, codex_model)

            elif model.startswith("claude"):
                # Claude CLI (uses Max/Pro subscription)
                claude_model = model.split(":")[1] if ":" in model else "opus"
                print(f"[KTalk Bridge] Anchor using Claude CLI with model: {claude_model}", file=sys.stderr)
                raw_text = await self._call_claude_cli(prompt, claude_model)

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
        """Обрабатывает подключение клиента (Chrome extension)."""
        self._connected_clients.add(websocket)
        # Reset sequence counter on new client connect — extension resets its counter
        # on page reload, so we must accept seq=1 again
        with self._lock:
            self._last_seq = 0
            self._clock_offset = 0.0
        print(f"[KTalk Bridge] Client connected from {websocket.remote_address}", file=sys.stderr)

        try:
            async for message in websocket:
                try:
                    data = json.loads(message)
                    await self._process_message(data)
                except json.JSONDecodeError:
                    print(f"[KTalk Bridge] Invalid JSON: {message[:100]}", file=sys.stderr)
                except Exception as e:
                    print(f"[KTalk Bridge] Error processing message: {e}", file=sys.stderr)
        except Exception as e:
            print(f"[KTalk Bridge] Connection error: {e}", file=sys.stderr)
        finally:
            self._connected_clients.discard(websocket)
            print(f"[KTalk Bridge] Client disconnected", file=sys.stderr)
            # End meeting if this was the last client and meeting is active
            if self._meeting_storage and not self._connected_clients:
                meta = self._meeting_storage._load_meta()
                if meta and meta.get("source") == "ktalk":
                    self._meeting_storage.on_ktalk_leave("disconnected")
                    print("[KTalk Bridge] Ended KTalk meeting due to client disconnect", file=sys.stderr)

    async def _process_message(self, data: dict):
        """Обрабатывает сообщение от extension."""
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

        elif msg_type == "connection_status":
            # Статус подключения к конференции
            connected = data.get("connected", False)
            if not connected:
                with self._lock:
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
                    self._participants = {}
                print("[KTalk Bridge] Conference disconnected", file=sys.stderr)
            else:
                print("[KTalk Bridge] Conference connected", file=sys.stderr)

        elif msg_type == "meeting_join":
            # Extension сообщает о входе во встречу (KTalk, Jitsi, etc.)
            url = data.get("url")
            title = data.get("title")
            platform = data.get("platform", "ktalk")
            if self._meeting_storage and url:
                meeting_id = self._meeting_storage.on_meeting_join(url, title, platform)
                print(f"[KTalk Bridge] Joined {platform} meeting: {url}", file=sys.stderr)
                # Notify about meeting change for audio recording
                if self._on_meeting_change:
                    meeting_path = self._meeting_storage.current_meeting_path
                    self._on_meeting_change(meeting_id, meeting_path)
                # Отправляем историю транскрипта в extension
                await self._send_transcript_history()

        elif msg_type == "meeting_leave":
            # Extension сообщает о выходе из встречи
            reason = data.get("reason", "user_left")
            if self._meeting_storage:
                new_meeting_id = self._meeting_storage.on_ktalk_leave(reason)
                print(f"[KTalk Bridge] Left meeting (reason: {reason}), new meeting: {new_meeting_id}", file=sys.stderr)
                # Notify about meeting change for audio recording
                if self._on_meeting_change:
                    meeting_path = self._meeting_storage.current_meeting_path
                    self._on_meeting_change(new_meeting_id, meeting_path)
                # Оповестить extension о новой встрече
                await self._broadcast_message({
                    "type": "new_meeting",
                    "meeting_id": new_meeting_id
                })

        elif msg_type == "meeting_title_update":
            # Extension отправляет обновлённый title (после "Joining..." -> реальный)
            title = data.get("title")
            if self._meeting_storage and title:
                self._meeting_storage.update_title(title)
                print(f"[KTalk Bridge] Updated meeting title: {title}", file=sys.stderr)

        elif msg_type == "request_transcript":
            # Extension запрашивает историю транскрипта (при reconnect)
            await self._send_transcript_history()

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

                    # Периодически обновляем debug state даже без событий
                    self._write_debug_state()

        except Exception as e:
            print(f"[KTalk Bridge] Server error: {e}", file=sys.stderr)

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
            self._thread.join(timeout=2.0)
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
