#!/usr/bin/env python3
"""
KTalk Bridge - WebSocket server для приёма данных о спикерах из Chrome extension.

Запускается параллельно с live транскрипцией и предоставляет real-time
информацию о том, кто сейчас говорит в KTalk конференции.

Использование:
    # Standalone (для тестирования)
    python ktalk_bridge.py --port 8765

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
from dataclasses import dataclass, field
from typing import Optional, Dict, List, Callable, TYPE_CHECKING
import sys

if TYPE_CHECKING:
    from meeting_storage import MeetingStorage


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
    SPEAKER_MEMORY_SECONDS = 3.0

    # Максимальная длительность сегмента (сек) - если спикер говорит дольше, разбиваем на части
    MAX_SEGMENT_DURATION = 20.0

    def __init__(self, host: str = "localhost", port: int = 8765):
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

        # Callbacks для уведомления о событиях
        self._on_speaker_change: Optional[Callable[[Optional[str], bool], None]] = None
        self._on_segment_complete: Optional[Callable[[SpeakerSegment], None]] = None

        # Meeting storage для сохранения транскриптов
        self._meeting_storage: Optional['MeetingStorage'] = None

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

    def get_current_speaker(self) -> Optional[str]:
        """
        Возвращает имя текущего говорящего.

        Если никто не говорит, но кто-то говорил недавно (в пределах SPEAKER_MEMORY_SECONDS),
        возвращает имя последнего говорившего. Это нужно для компенсации задержки
        между аудио capture и WebSocket событиями.
        """
        with self._lock:
            if self._is_speaking and self._current_speaker:
                return self._current_speaker

            # Если никто не говорит, проверяем "память" о последнем спикере
            if self._last_speaker:
                elapsed = time.time() - self._last_speaker_time
                if elapsed < self.SPEAKER_MEMORY_SECONDS:
                    return self._last_speaker

            return None

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
                return True
            return all(p.mic_muted for p in self._participants.values())

    def is_local_mic_muted(self) -> bool:
        """Возвращает True если микрофон локального пользователя замьючен в KTalk."""
        with self._lock:
            for p in self._participants.values():
                if p.is_local:
                    return p.mic_muted
            # Нет локального участника - считаем что не замьючен
            return False

    def get_speaker_history(self) -> List[SpeakerEvent]:
        """Возвращает историю спикеров."""
        with self._lock:
            return self._speaker_history.copy()

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

    async def _handle_client(self, websocket):
        """Обрабатывает подключение клиента (Chrome extension)."""
        self._connected_clients.add(websocket)
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

    async def _process_message(self, data: dict):
        """Обрабатывает сообщение от extension."""
        msg_type = data.get("type")

        if msg_type == "speaker_update":
            # Основное событие: обновление текущего спикера
            speaker = data.get("speaker")  # {name, id, isLocal} или null
            event_time = data.get("timestamp", time.time() * 1000) / 1000  # JS timestamp в мс -> секунды

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

                    # Добавляем в историю
                    event = SpeakerEvent(
                        speaker_name=self._current_speaker,
                        speaker_id=self._current_speaker_id or "",
                        timestamp=event_time,
                        is_local=new_is_local
                    )
                    self._speaker_history.append(event)
                else:
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

                # Вызываем callback
                if self._on_speaker_change:
                    self._on_speaker_change(self._current_speaker, self._is_speaking)

            # Вызываем segment callback вне lock
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

        elif msg_type == "connection_status":
            # Статус подключения к конференции
            connected = data.get("connected", False)
            if not connected:
                with self._lock:
                    self._is_speaking = False
                    self._current_speaker = None
                    self._participants = {}
                print("[KTalk Bridge] Conference disconnected", file=sys.stderr)
            else:
                print("[KTalk Bridge] Conference connected", file=sys.stderr)

        elif msg_type == "meeting_join":
            # Extension сообщает о входе во встречу KTalk
            url = data.get("url")
            title = data.get("title")
            if self._meeting_storage and url:
                meeting_id = self._meeting_storage.on_ktalk_join(url, title)
                print(f"[KTalk Bridge] Joined meeting: {url}", file=sys.stderr)
                # Отправляем историю транскрипта в extension
                await self._send_transcript_history()

        elif msg_type == "meeting_leave":
            # Extension сообщает о выходе из встречи
            reason = data.get("reason", "user_left")
            if self._meeting_storage:
                new_meeting_id = self._meeting_storage.on_ktalk_leave(reason)
                print(f"[KTalk Bridge] Left meeting (reason: {reason}), new meeting: {new_meeting_id}", file=sys.stderr)
                # Оповестить extension о новой встрече
                await self._broadcast_message({
                    "type": "new_meeting",
                    "meeting_id": new_meeting_id
                })

        elif msg_type == "request_transcript":
            # Extension запрашивает историю транскрипта (при reconnect)
            await self._send_transcript_history()

        elif msg_type == "ping":
            # Heartbeat
            pass

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

                while self._running:
                    await asyncio.sleep(0.1)

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
    parser.add_argument("--port", type=int, default=8765, help="Port to listen on")
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
