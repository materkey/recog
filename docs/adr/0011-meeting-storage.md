# ADR-0011: Хранилище митингов для KTalk режима

## Status
Accepted. `mode` amended by ADR-0021: `system_audio_backend` carries the
resolved backend (`screencapture` | `pulse` | `device:<id>` | `none`), while
`screencapture` stays a boolean meaning ScreenCaptureKit specifically.

## Context
При использовании live-режима с KTalk возникают следующие потребности:
- Сохранять транскрипты митингов для последующего просмотра
- Восстанавливать транскрипт при перезаходе во встречу (reconnect extension)
- Разделять записи при выходе из одной встречи и входе в другую
- Сохранять записи даже без KTalk (только mic/system audio)

### Особенности KTalk URL:
- Live URL: `https://example.ktalk.ru/abcd1234wxyz`
- History URL: `https://example.ktalk.ru/history/ExampleHistoryId0001`
- ID в live и history URL **могут отличаться**
- Нельзя использовать ID из URL как уникальный ключ

### Сценарии использования:
1. Одна KTalk встреча → один митинг
2. Выход из KTalk, продолжение записи → новый митинг (orphan)
3. Выход из одной KTalk встречи, вход в другую → два митинга
4. Без KTalk вообще → митинг с source="local"

## Decision

### 1. Архитектура: Session vs Meeting

**Session** — одна на запуск `mp4_notes.py live`
**Meeting** — создаётся при:
- Старте сессии
- Выходе из KTalk встречи (автоматически)

```
Recording Session
    │
    ├── Meeting 1 (KTalk: abcd1234wxyz)     14:30 - 15:00
    │
    ├── Meeting 2 (KTalk: abc123xyz)         15:05 - 15:30
    │
    └── Meeting 3 (orphan - после выхода)    15:30 - 15:35
```

### 2. Структура хранения

```
recog/
├── meetings/
│   ├── index.json                     # Индекс всех встреч
│   └── 2025-12-07_143015_abc123/      # {date}_{time}_{short_uuid}
│       ├── meta.json                  # Метаданные
│       └── transcript.jsonl           # Транскрипт (JSON Lines)
```

#### Почему JSONL для транскрипта:
- Append-only операции (не нужно перезаписывать весь файл)
- Легко читать построчно
- Устойчивость к сбоям (потеряется максимум последняя строка)

### 3. Формат данных

#### index.json
```json
{
  "meetings": [
    {
      "id": "2025-12-07_143015_abc123",
      "created_at": "2025-12-07T14:30:15Z",
      "ended_at": "2025-12-07T15:00:00Z",
      "duration_seconds": 1785,
      "title": "Standup команды",
      "source": "ktalk",
      "end_reason": "meeting_leave",
      "ktalk_live_url": "https://example.ktalk.ru/abcd1234wxyz",
      "segment_count": 45,
      "participants": ["Vyacheslav Kovalev", "Владимир Алипов"]
    }
  ]
}
```

#### meta.json
```json
{
  "id": "2025-12-07_143015_abc123",
  "version": 1,

  "created_at": "2025-12-07T14:30:15Z",
  "ended_at": "2025-12-07T15:00:00Z",
  "updated_at": "2025-12-07T14:45:00Z",

  "source": "ktalk",
  "end_reason": "meeting_leave",

  "mode": {
    "mic": true,
    "system_audio": true,
    "ktalk": true,
    "screencapture": false,
    "system_audio_backend": "pulse"
  },

  "ktalk": {
    "live_url": "https://example.ktalk.ru/abcd1234wxyz",
    "live_id": "abcd1234wxyz",
    "host": "example.ktalk.ru"
  },

  "title": "Standup команды",
  "title_source": "ktalk",

  "prev_meeting_id": null,
  "next_meeting_id": "2025-12-07_150005_def456",

  "participants": {
    "local": {
      "name": "Vyacheslav Kovalev",
      "is_local": true,
      "first_seen": "2025-12-07T14:30:15Z"
    },
    "p_abc123": {
      "name": "Владимир Алипов",
      "is_local": false,
      "first_seen": "2025-12-07T14:30:20Z"
    }
  },

  "stats": {
    "segment_count": 45,
    "duration_seconds": 1785,
    "words_count": 1250
  }
}
```

#### transcript.jsonl
```jsonl
{"id":1,"ts":"2025-12-07T14:30:15Z","ts_display":"14:30:15","speaker":"Vyacheslav Kovalev","speaker_id":"local","text":"Начинаем встречу","is_local":true,"cont":false,"uncertain":false,"uncertain_reason":""}
{"id":2,"ts":"2025-12-07T14:30:18Z","ts_display":"14:30:18","speaker":"Владимир Алипов","speaker_id":"p_abc123","text":"Да, я готов","is_local":false,"cont":false,"uncertain":false,"uncertain_reason":""}
```

### 4. Типы source

| source | Описание | Идентификация спикеров |
|--------|----------|------------------------|
| `ktalk` | KTalk подключён | Chrome extension → имена участников |
| `local` | Только mic, без KTalk | Фиксированный speaker |
| `diarized` | System audio без KTalk | Embedding → speaker_db или Speaker_N |
| `orphan` | Запись после выхода из KTalk | Зависит от mode |

### 5. Типы end_reason

| end_reason | Когда |
|------------|-------|
| `meeting_leave` | Extension прислал событие выхода из KTalk |
| `session_end` | Ctrl+C / завершение программы |
| `page_closed` | Закрыта вкладка KTalk |

### 6. Диаграмма состояний

```
                    ┌──────────────┐
                    │  NO MEETING  │
                    └──────┬───────┘
                           │ start_session()
                           ▼
                    ┌──────────────┐
          ┌─────────│   ORPHAN     │◄────────────┐
          │         └──────┬───────┘             │
          │                │                     │
          │ on_ktalk_join()│                     │ on_ktalk_leave()
          │                ▼                     │
          │         ┌──────────────┐             │
          │         │    KTALK     │─────────────┘
          │         └──────┬───────┘
          │                │
          │ end_session()  │ end_session()
          │                │
          ▼                ▼
    ┌─────────────────────────────┐
    │          ENDED              │
    │  (saved to index.json)      │
    └─────────────────────────────┘
```

### 7. WebSocket протокол (новые сообщения)

#### Extension → Bridge:
```json
{"type": "meeting_join", "url": "https://example.ktalk.ru/abcd1234wxyz", "title": "Название"}
{"type": "meeting_leave", "reason": "user_left"}
```

#### Bridge → Extension:
```json
{"type": "transcript_history", "segments": [...]}
{"type": "new_meeting", "meeting_id": "2025-12-07_150005_def456"}
```

### 8. Класс MeetingStorage

```python
class MeetingStorage:
    def __init__(self, base_path: Path = None):
        self.base_path = base_path or Path("meetings")

    # Lifecycle
    def start_session(mic, system_audio, ktalk, screencapture,
                      system_audio_backend, chunk_duration_ms) -> str
    def end_session()

    # KTalk events
    def on_ktalk_join(url: str, title: str = None) -> str
    def on_ktalk_leave(reason: str) -> str  # Returns new meeting_id

    # Data
    def append_segment(speaker, speaker_id, text, is_local, ...)
    def update_participants(participants: dict)
    def get_current_transcript() -> list[dict]

    # Query (для UI)
    def list_meetings() -> list[dict]
    def get_meeting(meeting_id: str) -> dict
```

### 9. Интеграция

#### mp4_notes.py:
```python
class LiveTranscriber:
    def __init__(self, ...):
        self.meeting_storage = MeetingStorage()

    def run(self):
        self.meeting_storage.start_session(
            mic=self.mic_capture is not None,
            system_audio=self.system_capture is not None,
            ktalk=self.use_ktalk,
            screencapture=self.system_audio == "screencapture",
            system_audio_backend=self.system_audio
        )
        try:
            # ... transcription loop ...
            self.meeting_storage.append_segment(...)
        finally:
            self.meeting_storage.end_session()
```

#### ktalk_bridge.py:
```python
class KTalkBridge:
    def set_meeting_storage(self, storage: MeetingStorage):
        self.meeting_storage = storage

    async def _handle_message(self, message):
        if message["type"] == "meeting_join":
            self.meeting_storage.on_ktalk_join(message["url"])
            await self._send_current_transcript()

        elif message["type"] == "meeting_leave":
            self.meeting_storage.on_ktalk_leave(message["reason"])
```

#### Extension (injected.js):
```javascript
let currentMeetingUrl = null;

function checkMeetingStatus() {
    const isInMeeting = window.$conf?._participantsMap?.size > 0;
    const url = window.location.href;

    if (isInMeeting && url !== currentMeetingUrl) {
        currentMeetingUrl = url;
        ws.send(JSON.stringify({type: "meeting_join", url, title: document.title}));
    } else if (!isInMeeting && currentMeetingUrl) {
        ws.send(JSON.stringify({type: "meeting_leave", reason: "user_left"}));
        currentMeetingUrl = null;
    }
}

setInterval(checkMeetingStatus, 2000);
```

## Consequences

### Positive:
- Все транскрипты сохраняются автоматически
- При reconnect extension получает историю
- Чёткое разделение митингов при выходе/входе
- Работает и без KTalk
- Формат удобен для будущего UI просмотра
- Связь между митингами через prev/next_meeting_id

### Negative:
- Дополнительная I/O операция на каждый сегмент (append to jsonl)
- Нужно реализовать garbage collection для старых митингов
- Пустые митинги могут создаваться при быстром reconnect

### Mitigation:
- Пустые митинги (segment_count=0) удаляются автоматически при end
- JSONL append — минимальная I/O нагрузка
- GC можно добавить позже (удаление митингов старше N дней)

## Future Work
- UI для просмотра сохранённых митингов
- Экспорт в Markdown
- Поиск по митингам
- Merge митингов (если случайно разделились)
- Синхронизация с облаком
