#!/usr/bin/env python3
"""
Meeting Storage - хранилище митингов для KTalk режима.

Сохраняет транскрипты митингов в JSON/JSONL формате для:
- Восстановления при reconnect extension
- Просмотра через UI
- Экспорта

См. ADR-0011 для деталей архитектуры.
"""

from pathlib import Path
from datetime import datetime, timezone, timedelta
from dataclasses import dataclass, asdict, field
from typing import Optional
from urllib.parse import urlparse
import json
import re
import uuid
import shutil
import hashlib
import os
import stat
import sys
import threading
import time

from recog.config import get_data_dir
from recog.conference_hosts import canonical_source


UTC = timezone.utc
EXTERNAL_INDEX_CACHE_DIR = Path.home() / ".cache" / "recog" / "external_indexes"

# Sources that represent an extension-driven conference meeting. All four share
# the same lifecycle — copilot auto-spawn, end-on-last-extension-gone, and
# increment generation — so the bridge gates them through a single predicate
# instead of a hardcoded `source == "ktalk"`. "orphan"/"local" are plain
# recordings, not conferences.
CONFERENCE_SOURCES = frozenset({"ktalk", "vas3k", "jitsi", "matrix"})


def is_conference_source(source: Optional[str]) -> bool:
    """True when `source` is an extension-driven conference platform.

    Groups ktalk + the Jitsi platforms (vas3k/jitsi) + matrix so copilot
    lifecycle, end-on-last-extension-gone, and increment generation treat them
    identically. orphan/local meetings are not conferences.
    """
    return canonical_source(source) in CONFERENCE_SOURCES


def get_external_paths() -> list[tuple[Path, Optional[str]]]:
    """Get external meeting paths from environment variable.

    Format: path1:source1,path2:source2 or just path1,path2
    If source not specified, uses parent directory name.

    Returns:
        List of (path, source_override) tuples
    """
    paths_str = os.getenv("EXTERNAL_MEETINGS_PATHS", "")
    if not paths_str:
        return []

    result = []
    for entry in paths_str.split(","):
        entry = entry.strip()
        if not entry:
            continue
        if ":" in entry:
            path, source = entry.rsplit(":", 1)
            result.append((Path(path.strip()), source.strip()))
        else:
            # Use parent directory name as source
            path = Path(entry)
            source = path.parent.name if path.parent.name else None
            result.append((path, source))
    return result


@dataclass
class TranscriptSegment:
    """Сегмент транскрипта."""
    id: int
    ts: str                      # ISO timestamp
    ts_display: str              # HH:MM:SS
    speaker: str
    speaker_id: str
    text: str
    is_local: bool
    cont: bool                   # is_continuation
    uncertain: bool
    uncertain_reason: str = ""
    audio_offset_ms: int = 0     # Offset from audio recording start in milliseconds
    refined: bool = field(default=False)  # True once word-timestamp refine has rewritten `text`


class MeetingStorage:
    """
    Хранилище митингов.

    Жизненный цикл:
    1. start_session() - при запуске live режима
    2. on_ktalk_join() - когда extension сообщает о входе в KTalk
    3. append_segment() - при каждом новом сегменте транскрипта
    4. on_ktalk_leave() - когда extension сообщает о выходе (создаёт новый митинг)
    5. end_session() - при завершении (Ctrl+C)
    """

    def __init__(self, base_path: Path = None, external_paths: list[tuple[Path, Optional[str]]] = None):
        self.base_path = base_path or get_data_dir() / "meetings"
        self.base_path.mkdir(parents=True, exist_ok=True)
        # External paths are read-only (for federated access)
        # Format: list of (path, source_override) tuples
        self.external_paths = external_paths or []

        self.current_meeting_id: Optional[str] = None
        self.current_meeting_path: Optional[Path] = None
        self._segment_counter = 0
        self._segment_lock = threading.Lock()
        self._mode: dict = {}

    def start_session(
        self,
        mic: bool = False,
        system_audio: bool = False,
        ktalk: bool = False,
        screencapture: bool = False,
        system_audio_backend: str = "none",
        chunk_duration_ms: int = 3000
    ) -> str:
        """
        Начать сессию записи. Создаёт первую встречу.

        Returns:
            meeting_id созданной встречи
        """
        # Закрыть все предыдущие незавершённые сессии
        self._close_stale_sessions()

        # Cleanup old audio files (24h retention)
        self.cleanup_old_audio()

        self._mode = {
            "mic": mic,
            "system_audio": system_audio,
            "ktalk": ktalk,
            # `screencapture` — именно ScreenCaptureKit; фактический бэкенд
            # (`pulse`, `device:<id>`, …) живёт в `system_audio_backend`.
            "screencapture": screencapture,
            "system_audio_backend": system_audio_backend
        }
        self._chunk_duration_ms = chunk_duration_ms

        # Определяем начальный source
        if ktalk:
            source = "orphan"  # Ждём meeting_join от extension
        elif system_audio and not mic:
            source = "diarized"
        else:
            source = "local"

        return self._create_meeting(source=source)

    def _close_stale_sessions(self):
        """
        Закрыть все незавершённые сессии от предыдущих запусков.
        Вызывается при start_session().
        """
        closed_count = 0
        for meeting_dir in self.base_path.iterdir():
            if not meeting_dir.is_dir():
                continue

            meta_path = meeting_dir / "meta.json"
            if not meta_path.exists():
                continue

            try:
                meta = json.loads(meta_path.read_text(encoding="utf-8"))

                # Только незавершённые
                if meta.get("ended_at") is not None:
                    continue

                # Закрыть сессию
                now = datetime.now(UTC)
                created = datetime.fromisoformat(meta["created_at"])

                meta["ended_at"] = meta.get("updated_at") or now.isoformat()
                meta["end_reason"] = "stale_session"
                meta["stats"]["duration_seconds"] = int(
                    (datetime.fromisoformat(meta["ended_at"]) - created).total_seconds()
                )

                self._save_json(meta_path, meta)
                self._update_index(meta)
                closed_count += 1

            except (json.JSONDecodeError, KeyError, ValueError) as e:
                print(f"Warning: Could not close stale session {meeting_dir.name}: {e}")
                continue

        if closed_count > 0:
            print(f"Closed {closed_count} stale session(s) from previous runs")

    def _create_meeting(
        self,
        source: str,
        prev_meeting_id: Optional[str] = None
    ) -> str:
        """Создать новую встречу."""
        now = datetime.now(UTC)
        short_uuid = uuid.uuid4().hex[:8]
        meeting_id = now.strftime("%Y-%m-%d_%H%M%S") + f"_{short_uuid}"

        meeting_path = self.base_path / meeting_id
        meeting_path.mkdir()

        meta = {
            "id": meeting_id,
            "version": 1,
            "created_at": now.isoformat(),
            "ended_at": None,
            "updated_at": now.isoformat(),
            "source": canonical_source(source),
            "end_reason": None,
            "mode": self._mode.copy(),
            "chunk_duration_ms": getattr(self, '_chunk_duration_ms', 3000),
            "conference": {
                "live_url": None,
                "live_id": None,
                "host": None
            },
            "title": None,
            "title_source": None,
            "prev_meeting_id": prev_meeting_id,
            "next_meeting_id": None,
            "participants": {},
            "stats": {
                "segment_count": 0,
                "duration_seconds": 0,
                "words_count": 0
            }
        }

        self._save_json(meeting_path / "meta.json", meta)
        (meeting_path / "transcript.jsonl").touch()

        # Обновить prev meeting если есть
        if prev_meeting_id:
            prev_path = self.base_path / prev_meeting_id / "meta.json"
            if prev_path.exists():
                prev_meta = json.loads(prev_path.read_text())
                prev_meta["next_meeting_id"] = meeting_id
                self._save_json(prev_path, prev_meta)

        with self._segment_lock:
            self.current_meeting_id = meeting_id
            self.current_meeting_path = meeting_path
            self._segment_counter = 0

        return meeting_id

    def _find_same_day_meeting(self, live_id: str, platform: str, host: str) -> Optional[str]:
        """
        Найти завершённую встречу с тем же live_id и platform за сегодня.

        Используется для группировки встреч одной комнаты в один день.

        Args:
            live_id: ID комнаты (например, abcd1234wxyz)
            platform: Платформа встречи (ktalk, vas3k, jitsi)
            host: Сервер конференции; одинаковые комнаты на разных серверах независимы

        Returns:
            meeting_id если найдена, иначе None
        """
        if not live_id:
            return None

        today_local = datetime.now().date()

        for meeting_dir in self.base_path.iterdir():
            if not meeting_dir.is_dir():
                continue

            meta_path = meeting_dir / "meta.json"
            if not meta_path.exists():
                continue

            try:
                meta = json.loads(meta_path.read_text(encoding="utf-8"))

                # Только завершённые встречи с той же платформой
                if meta.get("ended_at") is None:
                    continue
                if canonical_source(meta.get("source")) != canonical_source(platform):
                    continue

                # Тот же live_id
                if meta.get("conference", {}).get("live_id") != live_id:
                    continue
                conference = meta.get("conference", {})
                stored_host = conference.get("host") or urlparse(conference.get("live_url") or "").netloc
                if stored_host != host:
                    continue

                # Тот же день (по локальному времени)
                created_at = datetime.fromisoformat(meta["created_at"])
                meeting_date = created_at.astimezone().date()

                if meeting_date == today_local:
                    return meta["id"]

            except (json.JSONDecodeError, KeyError, ValueError):
                continue

        return None

    def _reopen_meeting(self, meeting_id: str) -> bool:
        """
        Переоткрыть завершённую встречу для продолжения записи.

        Args:
            meeting_id: ID встречи для переоткрытия

        Returns:
            True если успешно, False если встреча не найдена
        """
        meeting_path = self.base_path / meeting_id
        meta_path = meeting_path / "meta.json"

        if not meta_path.exists():
            return False

        meta = json.loads(meta_path.read_text(encoding="utf-8"))

        # Снять флаг завершения
        meta["ended_at"] = None
        meta["end_reason"] = None
        meta["updated_at"] = datetime.now(UTC).isoformat()

        # Preserve original chunk_duration_ms so offline diarization aligns
        # old segments correctly; only set if not already present.
        if "chunk_duration_ms" not in meta and hasattr(self, '_chunk_duration_ms'):
            meta["chunk_duration_ms"] = self._chunk_duration_ms

        self._save_json(meta_path, meta)

        # Удалить из index.json (встреча снова активная)
        index_path = self.base_path / "index.json"
        if index_path.exists():
            index = json.loads(index_path.read_text(encoding="utf-8"))
            index["meetings"] = [m for m in index["meetings"] if m["id"] != meeting_id]
            self._save_json(index_path, index)

        # Установить как текущую и восстановить счётчик сегментов
        transcript_path = meeting_path / "transcript.jsonl"
        with self._segment_lock:
            self.current_meeting_id = meeting_id
            self.current_meeting_path = meeting_path
            if transcript_path.exists():
                content = transcript_path.read_text(encoding="utf-8").strip()
                self._segment_counter = len(content.split("\n")) if content else 0
            else:
                self._segment_counter = 0

        # Добавить маркер возобновления сессии
        self._append_system_event("session_resume", "Сессия возобновлена")

        return True

    def on_meeting_join(self, url: str, title: Optional[str] = None, platform: str = "ktalk") -> str:
        """
        Extension сообщает о входе во встречу (KTalk, Jitsi, etc.).

        Args:
            url: URL встречи (https://example.ktalk.ru/abcd1234wxyz)
            title: Заголовок страницы (опционально)
            platform: Платформа встречи (ktalk, vas3k, jitsi)

        Returns:
            meeting_id текущей встречи
        """
        parsed = urlparse(url)
        # Извлекаем ID: /abcd1234wxyz или /history/ExampleHistoryId0001
        path_parts = parsed.path.strip("/").split("/")
        live_id = path_parts[-1] if path_parts else None

        # Fallback для платформ, где id комнаты живёт во фрагменте URL, а не в
        # пути (Element Call: https://element.example.org/#/room/<roomId>).
        # Там путь пустой, поэтому вытаскиваем id из полной строки url.
        # Используем именно full `url`, а не parsed.fragment: urlparse срезает
        # ведущий '#', ломая alias-форму #/room/#recog-spike:...
        if not live_id:
            match = re.search(r"#/room/([^/?]+)", url)
            if match:
                live_id = match.group(1)

        # Проверяем, нужно ли создать/переоткрыть встречу
        need_new_or_reopen = False
        if not self.current_meeting_path:
            need_new_or_reopen = True
        else:
            meta = self._load_meta()
            current_live_id = meta.get("conference", {}).get("live_id")

            conference = meta.get("conference", {})
            current_host = conference.get("host") or urlparse(conference.get("live_url") or "").netloc

            # Случай 1: другая комната, сервер или платформа
            if current_live_id and (
                current_live_id != live_id or current_host != parsed.netloc
                or canonical_source(meta.get("source")) != canonical_source(platform)
            ):
                self._end_current_meeting(end_reason="changed_meeting")
                need_new_or_reopen = True

            # Случай 2: orphan с сегментами -> новая встреча
            # Не склеивать orphan-сегменты с conference встречей
            elif meta["source"] == "orphan" and meta["stats"]["segment_count"] > 0:
                self._end_current_meeting(end_reason="meeting_join")
                need_new_or_reopen = True

            # Случай 3: пустой orphan -> ищем существующую или обновляем source
            elif meta["source"] == "orphan" and meta["stats"]["segment_count"] == 0:
                # Сначала проверим, есть ли существующая встреча за сегодня
                existing_meeting_id = self._find_same_day_meeting(live_id, platform, parsed.netloc)
                if existing_meeting_id:
                    # Удалить пустой orphan и переоткрыть существующую
                    orphan_path = self.current_meeting_path
                    self._reopen_meeting(existing_meeting_id)
                    # Удалить пустой orphan
                    if orphan_path and orphan_path.exists():
                        shutil.rmtree(orphan_path, ignore_errors=True)
                    print(f"Reopened same-day meeting: {existing_meeting_id}")
                else:
                    meta["source"] = canonical_source(platform)
                    self._save_meta(meta)

        if need_new_or_reopen:
            # Попробовать найти существующую встречу с тем же live_id за сегодня
            existing_meeting_id = self._find_same_day_meeting(live_id, platform, parsed.netloc)
            if existing_meeting_id:
                # Переоткрыть существующую встречу (группировка)
                self._reopen_meeting(existing_meeting_id)
                print(f"Reopened same-day meeting: {existing_meeting_id}")
            else:
                self._create_meeting(source=platform)

        meta = self._load_meta()

        meta["conference"]["live_url"] = url
        meta["conference"]["live_id"] = live_id
        meta["conference"]["host"] = parsed.netloc

        # Не перезаписывать title если уже есть (при reopen)
        if title and not meta.get("title"):
            meta["title"] = title
            meta["title_source"] = platform

        meta["updated_at"] = datetime.now(UTC).isoformat()
        self._save_meta(meta)

        return self.current_meeting_id

    # Backward compatibility alias
    def on_ktalk_join(self, url: str, title: Optional[str] = None) -> str:
        """Deprecated: use on_meeting_join instead."""
        return self.on_meeting_join(url, title, platform="ktalk")

    def on_ktalk_leave(self, reason: str = "user_left") -> str:
        """
        Extension сообщает о выходе из KTalk встречи.
        Завершает текущую встречу и создаёт новую.

        Args:
            reason: причина выхода (user_left, call_ended, page_closed, disconnected)

        Returns:
            meeting_id новой встречи
        """
        if not self.current_meeting_path:
            return self._create_meeting(source="orphan")

        # Завершить текущую встречу
        prev_id = self._end_current_meeting(end_reason=reason)

        # Создать новую (orphan пока не зайдём в следующую)
        new_id = self._create_meeting(source="orphan", prev_meeting_id=prev_id)

        return new_id

    def split_on_silence_timeout(self) -> Optional[str]:
        """
        Разделить orphan-встречу при timeout тишины.
        Вызывается из live.py перед append_segment().

        Returns:
            meeting_id новой встречи или None если split не нужен
        """
        if not self.current_meeting_path:
            return None

        meta = self._load_meta()

        # Только для orphan-встреч
        if meta["source"] != "orphan":
            return None

        # Только если есть сегменты (не пустой orphan)
        if meta["stats"]["segment_count"] == 0:
            return None

        prev_id = self._end_current_meeting(end_reason="silence_timeout")
        new_id = self._create_meeting(source="orphan", prev_meeting_id=prev_id)

        return new_id

    def _end_current_meeting(self, end_reason: str) -> Optional[str]:
        """Завершить текущую встречу без создания новой."""
        if not self.current_meeting_path:
            return None

        meta = self._load_meta()

        # Для conference-встреч добавляем маркер паузы (может быть переоткрыта)
        if is_conference_source(meta.get("source")) and end_reason != "session_end":
            self._append_system_event("session_pause", "Сессия приостановлена")

        now = datetime.now(UTC)

        meta["ended_at"] = now.isoformat()
        meta["end_reason"] = end_reason

        created = datetime.fromisoformat(meta["created_at"])
        meta["stats"]["duration_seconds"] = int((now - created).total_seconds())

        # Generate title for orphan sessions without title
        if meta.get("source") == "orphan" and not meta.get("title"):
            if meta["stats"]["segment_count"] > 0:
                title_result = self._generate_title_for_meeting()
                if title_result:
                    meta.update(title_result)

        self._save_meta(meta)
        self._update_index(meta)

        return self.current_meeting_id

    def _generate_title_for_meeting(self) -> Optional[dict]:
        """Generate a title and its provider/settings provenance for this meeting."""
        try:
            from recog.ai_settings import AiSettingsStore
            from recog.title_generator import generate_title

            transcript_text = self._get_transcript_as_text()
            if not transcript_text:
                return None

            settings = AiSettingsStore(self.base_path.parent).resolve_task("title")
            meta = self._load_meta()
            participants = meta.get("participants", {})
            variables = {
                "user_name": ", ".join(info.get("name", pid) for pid, info in participants.items() if info.get("is_local")),
                "participants": ", ".join(info.get("name", pid) for pid, info in participants.items()),
                "meeting_title": meta.get("title") or "",
            }
            title = generate_title(transcript_text, settings=settings, variables=variables)
            if title:
                print(f"Generated title: {title}")
                provider = settings["providers"][0]
                return {"title": title, "title_source": provider,
                        "title_model": settings["models"][provider],
                        "title_settings_revision": settings["revision"]}
            return None
        except ImportError:
            print("Warning: title_generator not available")
            return None
        except Exception as e:
            print(f"Warning: Title generation failed: {e}")
            return None

    def _get_transcript_as_text(self) -> str:
        """Get current meeting transcript as plain text."""
        if not self.current_meeting_path:
            return ""

        transcript_path = self.current_meeting_path / "transcript.jsonl"
        if not transcript_path.exists():
            return ""

        lines = []
        content = transcript_path.read_text(encoding="utf-8").strip()
        if not content:
            return ""

        for line in content.split("\n"):
            if not line:
                continue
            try:
                segment = json.loads(line)
                # Skip system events
                if segment.get("speaker_id") == "__system__":
                    continue
                speaker = segment.get("speaker", "Unknown")
                text = segment.get("text", "")
                ts = segment.get("ts_display", "")
                lines.append(f"[{ts}] {speaker}: {text}")
            except json.JSONDecodeError:
                continue

        return "\n".join(lines)

    def end_session(self) -> Optional[str]:
        """
        Завершить сессию записи (Ctrl+C).

        Returns:
            meeting_id завершённой встречи или None
        """
        meeting_id = None
        if self.current_meeting_path:
            meeting_id = self._end_current_meeting(end_reason="session_end")

        self.current_meeting_id = None
        self.current_meeting_path = None

        return meeting_id

    def append_segment(
        self,
        speaker: str,
        speaker_id: str,
        text: str,
        is_local: bool = False,
        is_continuation: bool = False,
        uncertain: bool = False,
        uncertain_reason: str = "",
        audio_offset_ms: int = 0,
        captured_at: Optional[float] = None,
    ) -> int:
        """
        Добавить сегмент в текущую встречу.

        Args:
            speaker: Имя спикера
            speaker_id: ID спикера (из KTalk или internal)
            text: Транскрибированный текст
            is_local: True если это микрофон (я)
            is_continuation: True если продолжение предыдущей реплики
            uncertain: True если неуверены в спикере
            uncertain_reason: Причина неуверенности ("local", "mics_off")
            audio_offset_ms: Offset from audio recording start in milliseconds
            captured_at: Speech time as Unix seconds; omitted for events emitted now

        Returns:
            Segment ID of the appended segment
        """
        if not self.current_meeting_path:
            # Нет активной встречи - создать orphan
            self._create_meeting(source="orphan")

        # Hold _segment_lock for the entire read-modify-write cycle (counter,
        # JSONL append, meta.json update) so concurrent mic/system threads don't
        # overwrite each other's words_count or segment_count increments.
        with self._segment_lock:
            meeting_path = self.current_meeting_path
            self._segment_counter += 1
            segment_id = self._segment_counter

            now = datetime.now(UTC)
            spoken_at = datetime.fromtimestamp(captured_at, UTC) if captured_at is not None else now

            segment = TranscriptSegment(
                id=segment_id,
                ts=spoken_at.isoformat(),
                ts_display=spoken_at.astimezone().strftime("%H:%M:%S"),
                speaker=speaker,
                speaker_id=speaker_id,
                text=text,
                is_local=is_local,
                cont=is_continuation,
                uncertain=uncertain,
                uncertain_reason=uncertain_reason,
                audio_offset_ms=audio_offset_ms
            )

            # Append to JSONL (эффективнее чем перезапись всего файла)
            with open(meeting_path / "transcript.jsonl", "a", encoding="utf-8") as f:
                f.write(json.dumps(asdict(segment), ensure_ascii=False) + "\n")

            # Update stats
            meta_path = meeting_path / "meta.json"
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            meta["stats"]["segment_count"] = segment_id
            meta["stats"]["words_count"] += len(text.split())
            meta["updated_at"] = now.isoformat()

            # Track participants
            # Для локальных участников: если уже есть другой локальный — не дублируем
            if is_local and speaker_id not in meta["participants"]:
                existing_local = next(
                    (pid for pid, p in meta["participants"].items() if p.get("is_local")),
                    None
                )
                if existing_local:
                    # Уже есть локальный участник — пропускаем добавление нового
                    if speaker and meta["participants"][existing_local]["name"] != speaker:
                        meta["participants"][existing_local]["name"] = speaker
                else:
                    meta["participants"][speaker_id] = {
                        "name": speaker,
                        "is_local": is_local,
                        "first_seen": now.isoformat()
                    }
            elif speaker_id not in meta["participants"]:
                meta["participants"][speaker_id] = {
                    "name": speaker,
                    "is_local": is_local,
                    "first_seen": now.isoformat()
                }
            else:
                # Обновить имя если изменилось
                if meta["participants"][speaker_id]["name"] != speaker:
                    meta["participants"][speaker_id]["name"] = speaker

            self._save_json(meta_path, meta)

        return segment_id

    def update_segments(
        self,
        updates: list[dict],
        meeting_path: Optional[Path] = None,
    ):
        """
        Update existing segments in JSONL in-place.

        Rewrites transcript.jsonl lines matching segment IDs. Per-row keys are
        optional and applied only if present:
            - speaker (str): sets both `speaker` and `speaker_id` on the segment;
              the legacy retroactive-correction path always bundles this with
              `uncertain` / `uncertain_reason`.
            - uncertain (bool): overrides the `uncertain` flag.
            - uncertain_reason (str): overrides the reason.
            - text (str): overrides `text` AND sets `refined=True` (word-level
              refine pipeline — ADR-0019 / §5).

        Args:
            updates: List of dicts. Each row must include `segment_id`; any
                combination of the keys above is honoured.
            meeting_path: Explicit meeting directory. When provided, the update
                targets this path regardless of current_meeting_path. This
                prevents a TOCTOU race where a meeting switch between the
                caller's generation check and this call would redirect
                corrections to the wrong JSONL.
        """
        target_path = meeting_path or self.current_meeting_path
        if not target_path or not updates:
            return

        with self._segment_lock:
            transcript_path = target_path / "transcript.jsonl"
            if not transcript_path.exists():
                return

            # Build lookup: segment_id -> update
            update_map = {u["segment_id"]: u for u in updates}

            # Rewrite JSONL with corrections
            lines = transcript_path.read_text(encoding="utf-8").strip().split("\n")
            new_lines = []
            words_delta = 0
            for line in lines:
                if not line:
                    continue
                try:
                    segment = json.loads(line)
                    seg_id = segment.get("id")
                    if seg_id in update_map:
                        upd = update_map[seg_id]
                        if "speaker" in upd:
                            segment["speaker"] = upd["speaker"]
                            segment["speaker_id"] = upd["speaker"]
                        if "uncertain" in upd:
                            segment["uncertain"] = upd["uncertain"]
                        if "uncertain_reason" in upd:
                            segment["uncertain_reason"] = upd["uncertain_reason"]
                        if "text" in upd:
                            words_delta += (
                                len(upd["text"].split())
                                - len(str(segment.get("text", "")).split())
                            )
                            segment["text"] = upd["text"]
                            segment["refined"] = True
                    new_lines.append(json.dumps(segment, ensure_ascii=False))
                except json.JSONDecodeError:
                    new_lines.append(line)

            tmp_path = transcript_path.with_suffix(".jsonl.tmp")
            tmp_path.write_text("\n".join(new_lines) + "\n", encoding="utf-8")
            os.replace(str(tmp_path), str(transcript_path))

            if words_delta:
                meta_path = target_path / "meta.json"
                if meta_path.exists():
                    meta = json.loads(meta_path.read_text(encoding="utf-8"))
                    stats = meta.setdefault("stats", {})
                    current_words = int(stats.get("words_count", 0))
                    stats["words_count"] = max(0, current_words + words_delta)
                    self._save_json(meta_path, meta)

    def _append_system_event(self, event_type: str, message: str):
        """
        Добавить системное событие в транскрипт.

        Используется для маркировки пауз/возобновлений сессии.

        Args:
            event_type: Тип события (session_pause, session_resume)
            message: Текст сообщения для отображения
        """
        if not self.current_meeting_path:
            return

        with self._segment_lock:
            meeting_path = self.current_meeting_path
            self._segment_counter += 1
            segment_id = self._segment_counter

            now = datetime.now(UTC)

            event = {
                "id": segment_id,
                "ts": now.isoformat(),
                "ts_display": now.astimezone().strftime("%H:%M:%S"),
                "speaker": "Система",
                "speaker_id": "__system__",
                "text": message,
                "is_local": False,
                "cont": False,
                "uncertain": False,
                "uncertain_reason": "",
                "event_type": event_type
            }

            with open(meeting_path / "transcript.jsonl", "a", encoding="utf-8") as f:
                f.write(json.dumps(event, ensure_ascii=False) + "\n")

            # Update stats
            meta_path = meeting_path / "meta.json"
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            meta["stats"]["segment_count"] = segment_id
            meta["updated_at"] = now.isoformat()
            self._save_json(meta_path, meta)

    def update_title(self, title: str):
        """
        Обновить title текущей встречи.

        Вызывается когда extension отправляет обновлённый title
        (например, после смены с 'Joining the meeting' на реальное название).

        Args:
            title: Новый заголовок встречи
        """
        if not self.current_meeting_path or not title:
            return

        meta = self._load_meta()

        # Обновляем title только если он ещё не установлен или начинается с "Joining"
        current_title = meta.get("title") or ""
        if not current_title or current_title.startswith("Joining"):
            meta["title"] = title
            meta["title_source"] = "ktalk"
            meta["updated_at"] = datetime.now(UTC).isoformat()
            self._save_meta(meta)

    def update_participants(self, participants: dict):
        """
        Обновить список участников от KTalk.

        Args:
            participants: dict вида {id: {"name": str, "is_local": bool, ...}}
        """
        if not self.current_meeting_path:
            return

        meta = self._load_meta()
        now = datetime.now(UTC).isoformat()

        for pid, pinfo in participants.items():
            is_local = pinfo.get("is_local", False)

            if pid not in meta["participants"]:
                # Для локальных: проверяем нет ли уже другого локального
                if is_local:
                    existing_local_id = next(
                        (p_id for p_id, p in meta["participants"].items() if p.get("is_local")),
                        None
                    )
                    if existing_local_id and existing_local_id != pid:
                        # Уже есть локальный с другим ID — обновляем его имя (KTalk имя точнее)
                        new_name = pinfo.get("name")
                        if new_name:
                            meta["participants"][existing_local_id]["name"] = new_name
                        continue

                meta["participants"][pid] = {
                    "name": pinfo.get("name", f"Participant_{pid}"),
                    "is_local": is_local,
                    "first_seen": now
                }
            else:
                # Обновить имя если изменилось
                new_name = pinfo.get("name")
                if new_name and new_name != meta["participants"][pid]["name"]:
                    meta["participants"][pid]["name"] = new_name

        meta["updated_at"] = now
        self._save_meta(meta)

    def get_current_transcript(self) -> list[dict]:
        """
        Получить транскрипт текущей встречи (для extension при реконнекте).

        Returns:
            Список сегментов в формате dict
        """
        if not self.current_meeting_path:
            return []

        segments = []
        transcript_path = self.current_meeting_path / "transcript.jsonl"
        if transcript_path.exists():
            content = transcript_path.read_text(encoding="utf-8").strip()
            if content:
                for line in content.split("\n"):
                    if line:
                        segments.append(json.loads(line))
        return segments

    def get_current_meeting_id(self) -> Optional[str]:
        """Получить ID текущей встречи."""
        return self.current_meeting_id

    def list_meetings(self, include_active: bool = True, refresh_cache: bool = False) -> list[dict]:
        """
        Список всех встреч для UI.

        Args:
            include_active: Включать ли активные (незавершённые) встречи
            refresh_cache: Принудительно пересканировать external paths

        Returns:
            Список метаданных встреч (из index.json + external + активные)
        """
        meetings = []

        # 1. Завершённые встречи из index.json (основная директория)
        index_path = self.base_path / "index.json"
        if index_path.exists():
            data = json.loads(index_path.read_text(encoding="utf-8"))
            meetings = data.get("meetings", [])

        # 2. External directories (cached)
        for ext_path, source_override in self.external_paths:
            if ext_path.exists():
                meetings.extend(self._get_external_index(ext_path, source_override, refresh_cache))

        # 3. Active meetings (only from base_path)
        if include_active:
            active = self.list_active_meetings()
            meetings = active + meetings

        # Sort by created_at descending
        meetings.sort(key=lambda m: m.get("created_at", ""), reverse=True)

        return [dict(meeting, source=canonical_source(meeting.get("source")))
                for meeting in meetings]

    def _get_external_index(self, ext_path: Path, source_override: Optional[str], refresh: bool = False) -> list[dict]:
        """Get or create cached index for external path."""
        # Cache key = hash of path + source_override
        cache_key = hashlib.md5(f"{ext_path}:{source_override}".encode()).hexdigest()[:8]
        cache_file = EXTERNAL_INDEX_CACHE_DIR / f"{cache_key}.json"

        # Check if cache is fresh (compare mtime of directory)
        if not refresh and cache_file.exists():
            try:
                cache_mtime = cache_file.stat().st_mtime
                dir_mtime = ext_path.stat().st_mtime
                if cache_mtime >= dir_mtime:
                    data = json.loads(cache_file.read_text(encoding="utf-8"))
                    return data.get("meetings", [])
            except (OSError, json.JSONDecodeError):
                pass  # Rebuild cache on error

        # Rebuild cache
        meetings = self._scan_external_meetings(ext_path, source_override)
        EXTERNAL_INDEX_CACHE_DIR.mkdir(parents=True, exist_ok=True)
        cache_file.write_text(json.dumps({
            "path": str(ext_path),
            "source_override": source_override,
            "meetings": meetings,
            "updated_at": datetime.now(UTC).isoformat()
        }, ensure_ascii=False, indent=2), encoding="utf-8")

        return meetings

    def _scan_external_meetings(self, path: Path, source_override: Optional[str] = None) -> list[dict]:
        """Scan external directory for meetings (read-only)."""
        meetings = []
        for meeting_dir in path.iterdir():
            if not meeting_dir.is_dir():
                continue
            meta_path = meeting_dir / "meta.json"
            if not meta_path.exists():
                continue
            try:
                meta = json.loads(meta_path.read_text(encoding="utf-8"))
                # Use source_override if provided, otherwise use original source
                source = source_override if source_override else meta.get("source", "unknown")
                # Convert meta to index entry format
                entry = {
                    "id": meta["id"],
                    "created_at": meta.get("created_at"),
                    "ended_at": meta.get("ended_at"),
                    "duration_seconds": meta.get("stats", {}).get("duration_seconds", 0),
                    "title": meta.get("title"),
                    "source": canonical_source(source),
                    "end_reason": meta.get("end_reason"),
                    "conference_url": meta.get("conference", meta.get("ktalk", {})).get("live_url"),
                    "segment_count": meta.get("stats", {}).get("segment_count", 0),
                    "participants": [p.get("name", k) for k, p in meta.get("participants", {}).items()],
                    "_external": True,
                    "_external_path": str(path)
                }
                meetings.append(entry)
            except (json.JSONDecodeError, KeyError) as e:
                print(f"Warning: Failed to load {meta_path}: {e}", file=sys.stderr)
                continue
        return meetings

    def list_active_meetings(self) -> list[dict]:
        """
        Список активных (незавершённых) встреч.

        Сканирует директории meetings/ и находит те, где ended_at is null.

        Returns:
            Список метаданных активных встреч
        """
        active = []

        for meeting_dir in self.base_path.iterdir():
            if not meeting_dir.is_dir():
                continue

            meta_path = meeting_dir / "meta.json"
            if not meta_path.exists():
                continue

            try:
                meta = json.loads(meta_path.read_text(encoding="utf-8"))

                # Только незавершённые
                if meta.get("ended_at") is not None:
                    continue

                # Формат как в index.json
                active.append({
                    "id": meta["id"],
                    "created_at": meta["created_at"],
                    "ended_at": None,
                    "duration_seconds": self._calc_duration(meta["created_at"]),
                    "title": meta.get("title"),
                    "source": canonical_source(meta.get("source")),
                    "end_reason": None,
                    "ktalk_live_url": meta.get("conference", meta.get("ktalk", {})).get("live_url"),
                    "segment_count": meta.get("stats", {}).get("segment_count", 0),
                    "participants": [p["name"] for p in meta.get("participants", {}).values()],
                    "is_active": True  # Маркер активной встречи
                })
            except (json.JSONDecodeError, KeyError):
                continue

        # Сортировка по дате создания (новые сверху)
        active.sort(key=lambda m: m["created_at"], reverse=True)
        return active

    def _calc_duration(self, created_at: str) -> int:
        """Вычислить длительность от created_at до сейчас."""
        try:
            created = datetime.fromisoformat(created_at)
            now = datetime.now(UTC)
            return int((now - created).total_seconds())
        except (ValueError, TypeError):
            return 0

    def get_meeting(self, meeting_id: str) -> Optional[dict]:
        """
        Получить полные данные встречи.

        Args:
            meeting_id: ID встречи

        Returns:
            {"meta": {...}, "segments": [...]} или None
        """
        # 1. Try base_path first
        meeting_path = self.base_path / meeting_id
        if meeting_path.exists():
            return self._load_meeting_from_path(meeting_path)

        # 2. Try external paths
        for ext_path, _ in self.external_paths:
            meeting_path = ext_path / meeting_id
            if meeting_path.exists():
                return self._load_meeting_from_path(meeting_path)

        return None

    def _read_meeting_meta(self, meeting_path: Path) -> dict:
        """Разрешает WAV относительно встречи, не переписывая сохранённые метаданные."""
        meta = json.loads((meeting_path / "meta.json").read_text(encoding="utf-8"))
        if "source" in meta:
            meta["source"] = canonical_source(meta["source"])
        for track in ("mic", "system"):
            audio_path = meeting_path / f"audio_{track}.wav"
            if audio_path.is_file():
                meta[f"audio_{track}_path"] = str(audio_path)
        return meta

    def _load_meeting_from_path(self, meeting_path: Path) -> dict:
        """Load meeting data from a specific path."""
        meta = self._read_meeting_meta(meeting_path)

        segments = []
        transcript_path = meeting_path / "transcript.jsonl"
        if transcript_path.exists():
            content = transcript_path.read_text(encoding="utf-8").strip()
            if content:
                for line in content.split("\n"):
                    if line:
                        segments.append(json.loads(line))

        return {"meta": meta, "segments": segments}

    def _load_meta(self) -> dict:
        """Загрузить meta.json текущей встречи."""
        return json.loads(
            (self.current_meeting_path / "meta.json").read_text(encoding="utf-8")
        )

    def _save_meta(self, meta: dict):
        """Сохранить meta.json текущей встречи."""
        self._save_json(self.current_meeting_path / "meta.json", meta)

    def _save_json(self, path: Path, data: dict):
        """Сохранить JSON файл атомарно (уникальный temp file + os.replace).

        Конкурентные читатели (например _is_meeting_active в ktalk_bridge)
        всегда видят либо полный старый, либо полный новый файл — никогда
        обрезанный/частичный.
        """
        # Read the target mode before creating the temp so a restrictive
        # existing mode also protects the temp while its contents are written.
        try:
            mode = stat.S_IMODE(os.stat(path).st_mode)
        except FileNotFoundError:
            mode = None

        while True:
            tmp_path = path.parent / f"{path.name}.{uuid.uuid4().hex}.tmp"
            try:
                fd = os.open(
                    tmp_path,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                    mode if mode is not None else 0o666,
                )
                break
            except FileExistsError:
                continue

        raw_fd = fd
        try:
            file_obj = os.fdopen(fd, "w", encoding="utf-8")
            raw_fd = -1  # file_obj owns the descriptor from this point
            with file_obj as f:
                f.write(json.dumps(data, ensure_ascii=False, indent=2))

            # Atomic replacement changes the inode, so explicitly carry over
            # the existing target's permissions. A first write already has
            # the old open(..., "w") mode because os.open applied the umask.
            if mode is not None:
                os.chmod(tmp_path, mode)
            os.replace(tmp_path, path)
        except BaseException:
            if raw_fd != -1:
                try:
                    os.close(raw_fd)
                except OSError:
                    pass
            try:
                os.unlink(tmp_path)
            except OSError:
                pass
            raise

    def _update_index(self, meta: dict):
        """Обновить index.json после завершения встречи."""
        index_path = self.base_path / "index.json"

        if index_path.exists():
            index = json.loads(index_path.read_text(encoding="utf-8"))
        else:
            index = {"meetings": []}

        # Удалить старую запись если есть
        index["meetings"] = [m for m in index["meetings"] if m["id"] != meta["id"]]

        # Не добавлять пустые встречи - удалить папку
        if meta["stats"]["segment_count"] == 0:
            self._save_json(index_path, index)
            # Удалить пустую папку
            meeting_path = self.base_path / meta["id"]
            if meeting_path.exists():
                shutil.rmtree(meeting_path, ignore_errors=True)
            return

        # Добавить запись
        index["meetings"].append({
            "id": meta["id"],
            "created_at": meta["created_at"],
            "ended_at": meta["ended_at"],
            "duration_seconds": meta["stats"]["duration_seconds"],
            "title": meta["title"],
            "source": meta["source"],
            "end_reason": meta["end_reason"],
            "ktalk_live_url": meta.get("conference", meta.get("ktalk", {})).get("live_url"),
            "segment_count": meta["stats"]["segment_count"],
            "participants": [p["name"] for p in meta["participants"].values()]
        })

        # Сортировка по дате создания (новые сверху)
        index["meetings"].sort(key=lambda m: m["created_at"], reverse=True)

        self._save_json(index_path, index)

    # ========== Audio & Diarization Methods ==========

    def set_audio_paths(
        self,
        mic_path: Optional[str] = None,
        system_path: Optional[str] = None
    ):
        """
        Store audio file paths in meeting meta.

        Args:
            mic_path: Path to mic audio WAV file
            system_path: Path to system audio WAV file
        """
        if not self.current_meeting_path:
            return

        meta = self._load_meta()
        if mic_path:
            meta["audio_mic_path"] = mic_path
        if system_path:
            meta["audio_system_path"] = system_path
        meta["updated_at"] = datetime.now(UTC).isoformat()
        self._save_meta(meta)

    def get_audio_paths(self, meeting_id: str) -> tuple[Optional[Path], Optional[Path]]:
        """
        Get audio file paths for a meeting.

        Args:
            meeting_id: Meeting ID

        Returns:
            Tuple of (mic_path, system_path), each can be None
        """
        meeting_path = self.base_path / meeting_id
        meta_path = meeting_path / "meta.json"

        if not meta_path.exists():
            return (None, None)

        meta = self._read_meeting_meta(meeting_path)

        mic_path = meta.get("audio_mic_path")
        system_path = meta.get("audio_system_path")

        return (
            Path(mic_path) if mic_path and Path(mic_path).exists() else None,
            Path(system_path) if system_path and Path(system_path).exists() else None
        )

    def create_transcript_backup(self, meeting_id: str) -> Optional[Path]:
        """
        Create backup of transcript.jsonl before diarization.

        Args:
            meeting_id: Meeting ID

        Returns:
            Path to backup file, or None if failed
        """
        meeting_path = self.base_path / meeting_id
        transcript_path = meeting_path / "transcript.jsonl"
        backup_path = meeting_path / "transcript.jsonl.backup"

        if not transcript_path.exists():
            return None

        # Don't overwrite existing backup
        if backup_path.exists():
            return backup_path

        shutil.copy2(transcript_path, backup_path)

        # Store backup path in meta
        meta_path = meeting_path / "meta.json"
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        meta["transcript_backup_path"] = str(backup_path)
        meta["updated_at"] = datetime.now(UTC).isoformat()
        self._save_json(meta_path, meta)

        return backup_path

    def restore_transcript_backup(self, meeting_id: str) -> bool:
        """
        Restore transcript from backup (undo diarization).

        Args:
            meeting_id: Meeting ID

        Returns:
            True if restored, False if no backup found
        """
        meeting_path = self.base_path / meeting_id
        transcript_path = meeting_path / "transcript.jsonl"
        backup_path = meeting_path / "transcript.jsonl.backup"

        if not backup_path.exists():
            return False

        shutil.copy2(backup_path, transcript_path)
        backup_path.unlink()

        # Remove diarization info from meta
        meta_path = meeting_path / "meta.json"
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        meta.pop("diarization_mic", None)
        meta.pop("diarization_system", None)
        meta.pop("transcript_backup_path", None)
        meta["updated_at"] = datetime.now(UTC).isoformat()
        self._save_json(meta_path, meta)

        return True

    def cleanup_old_audio(self, retention_hours: float = 24.0) -> int:
        """
        Delete per-meeting audio WAVs whose last write is older than retention.

        Age is based on file mtime, not meta created_at: an actively-recorded
        file always has a fresh mtime, so the current meeting's audio can
        never be deleted mid-write. The scan is stat-only per meeting dir;
        meta.json is parsed only for dirs that still contain audio files, so
        a periodic call over thousands of meeting dirs stays cheap.

        Args:
            retention_hours: Hours to keep audio files after last write

        Returns:
            Number of files deleted
        """
        cutoff_ts = time.time() - retention_hours * 3600
        deleted_count = 0

        for meeting_dir in self.base_path.iterdir():
            if not meeting_dir.is_dir():
                continue

            stale_tracks = []
            for track in ("mic", "system"):
                wav_path = meeting_dir / f"audio_{track}.wav"
                try:
                    if wav_path.exists() and wav_path.stat().st_mtime < cutoff_ts:
                        stale_tracks.append((track, wav_path))
                except OSError:
                    continue

            if not stale_tracks:
                continue

            try:
                for _track, wav_path in stale_tracks:
                    wav_path.unlink()
                    deleted_count += 1

                meta_path = meeting_dir / "meta.json"
                if meta_path.exists():
                    meta = json.loads(meta_path.read_text(encoding="utf-8"))
                    for track, _wav_path in stale_tracks:
                        meta[f"audio_{track}_path"] = None
                    meta["audio_deleted_at"] = datetime.now(UTC).isoformat()
                    meta["updated_at"] = datetime.now(UTC).isoformat()
                    self._save_json(meta_path, meta)

            except (json.JSONDecodeError, OSError) as e:
                print(f"Warning: Cleanup error for {meeting_dir.name}: {e}")
                continue

        if deleted_count > 0:
            print(f"Cleaned up {deleted_count} audio file(s) older than {retention_hours:g}h")

        return deleted_count

    def delete_meeting(self, meeting_id: str) -> bool:
        """Permanently delete a completed meeting: its whole directory tree and
        its index.json entry. Returns True if deleted, False if the meeting dir
        is not found under base_path (e.g. it is external/read-only or unknown).
        Raises ValueError if the meeting is still active (being recorded)."""
        meeting_path = self.base_path / meeting_id
        if not meeting_path.is_dir():
            return False

        meta = None
        meta_path = meeting_path / "meta.json"
        if meta_path.exists():
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            # Guard: never delete a meeting the recorder is still writing to —
            # it holds open WAV handles + current_meeting_path (would crash live.py).
            if meta.get("ended_at") is None:
                raise ValueError("Cannot delete an active meeting")

        # Keep the prev/next linked list internally consistent (fields are
        # currently vestigial — nothing reads them — but a proper delete should
        # not leave dangling pointers). Best-effort.
        if meta is not None:
            self._relink_neighbors(meta)

        # Drop from the completed-meetings index (no-op for active meetings,
        # which are never indexed). Mirrors the removal in _reopen_meeting.
        index_path = self.base_path / "index.json"
        if index_path.exists():
            index = json.loads(index_path.read_text(encoding="utf-8"))
            kept = [m for m in index.get("meetings", []) if m.get("id") != meeting_id]
            if len(kept) != len(index.get("meetings", [])):
                index["meetings"] = kept
                self._save_json(index_path, index)

        shutil.rmtree(meeting_path, ignore_errors=True)
        return True

    def _relink_neighbors(self, meta: dict) -> None:
        """Patch a deleted meeting's neighbors so the prev/next chain stays
        consistent: prev.next -> deleted.next, next.prev -> deleted.prev."""
        prev_id, next_id = meta.get("prev_meeting_id"), meta.get("next_meeting_id")
        for nbr_id, field, target in (
            (prev_id, "next_meeting_id", next_id),
            (next_id, "prev_meeting_id", prev_id),
        ):
            if not nbr_id:
                continue
            p = self.base_path / nbr_id / "meta.json"
            if not p.exists():
                continue
            nbr = json.loads(p.read_text(encoding="utf-8"))
            if nbr.get(field) == meta.get("id"):
                nbr[field] = target
                self._save_json(p, nbr)

    def set_diarization_result(
        self,
        meeting_id: str,
        track: str,  # "mic" or "system"
        speakers_found: int
    ):
        """
        Store diarization result in meeting meta.

        Args:
            meeting_id: Meeting ID
            track: "mic" or "system"
            speakers_found: Number of speakers found
        """
        meeting_path = self.base_path / meeting_id
        meta_path = meeting_path / "meta.json"

        if not meta_path.exists():
            return

        meta = json.loads(meta_path.read_text(encoding="utf-8"))

        diarization_key = f"diarization_{track}"
        meta[diarization_key] = {
            "performed_at": datetime.now(UTC).isoformat(),
            "speakers_found": speakers_found
        }
        meta["updated_at"] = datetime.now(UTC).isoformat()

        self._save_json(meta_path, meta)
