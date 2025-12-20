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
from dataclasses import dataclass, asdict
from typing import Optional
from urllib.parse import urlparse
import json
import uuid
import shutil


UTC = timezone.utc


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

    def __init__(self, base_path: Path = None):
        self.base_path = base_path or Path(__file__).parent / "meetings"
        self.base_path.mkdir(exist_ok=True)

        self.current_meeting_id: Optional[str] = None
        self.current_meeting_path: Optional[Path] = None
        self._segment_counter = 0
        self._mode: dict = {}

    def start_session(
        self,
        mic: bool = False,
        system_audio: bool = False,
        ktalk: bool = False,
        screencapture: bool = False
    ) -> str:
        """
        Начать сессию записи. Создаёт первую встречу.

        Returns:
            meeting_id созданной встречи
        """
        # Закрыть все предыдущие незавершённые сессии
        self._close_stale_sessions()

        self._mode = {
            "mic": mic,
            "system_audio": system_audio,
            "ktalk": ktalk,
            "screencapture": screencapture
        }

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
            "source": source,
            "end_reason": None,
            "mode": self._mode.copy(),
            "ktalk": {
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

        self.current_meeting_id = meeting_id
        self.current_meeting_path = meeting_path
        self._segment_counter = 0

        return meeting_id

    def on_ktalk_join(self, url: str, title: Optional[str] = None) -> str:
        """
        Extension сообщает о входе в KTalk встречу.

        Args:
            url: URL встречи (https://example.ktalk.ru/abcd1234wxyz)
            title: Заголовок страницы (опционально)

        Returns:
            meeting_id текущей встречи
        """
        parsed = urlparse(url)
        # Извлекаем ID: /abcd1234wxyz или /history/ExampleHistoryId0001
        path_parts = parsed.path.strip("/").split("/")
        live_id = path_parts[-1] if path_parts else None

        # Проверяем, нужно ли создать новую встречу
        need_new_meeting = False
        if not self.current_meeting_path:
            need_new_meeting = True
        else:
            meta = self._load_meta()
            current_live_id = meta.get("ktalk", {}).get("live_id")

            # Случай 1: другая KTalk комната
            if current_live_id and current_live_id != live_id:
                prev_id = self._end_current_meeting(end_reason="changed_meeting")
                need_new_meeting = True

            # Случай 2: orphan с сегментами -> новая KTalk встреча
            # Не склеивать orphan-сегменты с KTalk встречей
            elif meta["source"] == "orphan" and meta["stats"]["segment_count"] > 0:
                prev_id = self._end_current_meeting(end_reason="ktalk_join")
                need_new_meeting = True

            # Случай 3: пустой orphan -> просто обновляем source на ktalk
            elif meta["source"] == "orphan" and meta["stats"]["segment_count"] == 0:
                meta["source"] = "ktalk"
                self._save_meta(meta)

        if need_new_meeting:
            self._create_meeting(source="ktalk")

        meta = self._load_meta()

        meta["ktalk"]["live_url"] = url
        meta["ktalk"]["live_id"] = live_id
        meta["ktalk"]["host"] = parsed.netloc

        if title:
            meta["title"] = title
            meta["title_source"] = "ktalk"

        meta["updated_at"] = datetime.now(UTC).isoformat()
        self._save_meta(meta)

        return self.current_meeting_id

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

    def _end_current_meeting(self, end_reason: str) -> Optional[str]:
        """Завершить текущую встречу без создания новой."""
        if not self.current_meeting_path:
            return None

        meta = self._load_meta()
        now = datetime.now(UTC)

        meta["ended_at"] = now.isoformat()
        meta["end_reason"] = end_reason

        created = datetime.fromisoformat(meta["created_at"])
        meta["stats"]["duration_seconds"] = int((now - created).total_seconds())

        self._save_meta(meta)
        self._update_index(meta)

        return self.current_meeting_id

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
        uncertain_reason: str = ""
    ):
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
        """
        if not self.current_meeting_path:
            # Нет активной встречи - создать orphan
            self._create_meeting(source="orphan")

        self._segment_counter += 1
        now = datetime.now(UTC)

        segment = TranscriptSegment(
            id=self._segment_counter,
            ts=now.isoformat(),
            ts_display=now.astimezone().strftime("%H:%M:%S"),
            speaker=speaker,
            speaker_id=speaker_id,
            text=text,
            is_local=is_local,
            cont=is_continuation,
            uncertain=uncertain,
            uncertain_reason=uncertain_reason
        )

        # Append to JSONL (эффективнее чем перезапись всего файла)
        with open(self.current_meeting_path / "transcript.jsonl", "a", encoding="utf-8") as f:
            f.write(json.dumps(asdict(segment), ensure_ascii=False) + "\n")

        # Update stats
        meta = self._load_meta()
        meta["stats"]["segment_count"] = self._segment_counter
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
                # (KTalk имя обычно точнее чем speaker_db)
                pass
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

    def list_meetings(self, include_active: bool = True) -> list[dict]:
        """
        Список всех встреч для UI.

        Args:
            include_active: Включать ли активные (незавершённые) встречи

        Returns:
            Список метаданных встреч (из index.json + активные)
        """
        meetings = []

        # Завершённые встречи из index.json
        index_path = self.base_path / "index.json"
        if index_path.exists():
            data = json.loads(index_path.read_text(encoding="utf-8"))
            meetings = data.get("meetings", [])

        if include_active:
            # Добавить активные (незавершённые) встречи
            active = self.list_active_meetings()
            # Добавить в начало (они самые свежие)
            meetings = active + meetings

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
                    "source": meta.get("source"),
                    "end_reason": None,
                    "ktalk_live_url": meta.get("ktalk", {}).get("live_url"),
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
        meeting_path = self.base_path / meeting_id
        if not meeting_path.exists():
            return None

        meta = json.loads((meeting_path / "meta.json").read_text(encoding="utf-8"))

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
        """Сохранить JSON файл."""
        path.write_text(
            json.dumps(data, ensure_ascii=False, indent=2),
            encoding="utf-8"
        )

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
            "ktalk_live_url": meta["ktalk"]["live_url"],
            "segment_count": meta["stats"]["segment_count"],
            "participants": [p["name"] for p in meta["participants"].values()]
        })

        # Сортировка по дате создания (новые сверху)
        index["meetings"].sort(key=lambda m: m["created_at"], reverse=True)

        self._save_json(index_path, index)
