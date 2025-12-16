# ADR-0012: Web Frontend для просмотра транскриптов

## Status
Proposed

## Context

Система recog записывает транскрипты митингов в формате JSONL и хранит метаданные в JSON. Сейчас доступ к записям возможен только через:
- Консольный вывод во время записи
- Прямое чтение файлов из `meetings/`

Нужен веб-интерфейс для:
1. Просмотра списка всех записанных митингов
2. Чтения транскриптов с удобной навигацией
3. Наблюдения за live-выводом сервиса в реальном времени
4. Поиска по транскриптам

## Decision

### 1. Архитектура

```
frontend/
├── server.py              # FastAPI backend
├── static/
│   ├── index.html         # SPA entry point
│   ├── app.js             # Vanilla JS application
│   └── style.css          # Styles
└── requirements.txt       # Python dependencies
```

**Почему эта структура:**
- Минимальные зависимости (FastAPI + uvicorn)
- Vanilla JS без фреймворков — простота, нет node_modules
- Один Python-сервер для API и статики
- Легко запустить: `python server.py`

### 2. Backend API (FastAPI)

#### REST Endpoints:

```
GET /api/meetings                    # Список всех митингов
GET /api/meetings/{id}               # Метаданные митинга
GET /api/meetings/{id}/transcript    # Транскрипт (JSON array)
GET /api/live/status                 # Статус live-режима
```

#### WebSocket Endpoints:

```
WS /api/live/stream                  # Live транскрипт (real-time segments)
WS /api/meetings/{id}/watch          # Наблюдение за обновлениями митинга
```

### 3. Frontend Views

#### a) Meetings List (`/`)
- Таблица/карточки всех митингов
- Сортировка по дате (новые сверху)
- Фильтры: по source (ktalk/local/orphan), по дате
- Показывает: title, date, duration, participants, segment_count

#### b) Meeting Detail (`/meeting/{id}`)
- Метаданные митинга (участники, длительность, source)
- Транскрипт с группировкой по спикерам
- Timestamps с подсветкой
- Поиск по тексту внутри транскрипта
- Экспорт в Markdown

#### c) Live View (`/live`)
- Текущий статус live-режима
- Real-time поток сегментов через WebSocket
- Показывает текущий митинг (если есть)
- Автоскролл к новым сообщениям

### 4. Технологии

| Компонент | Технология | Обоснование |
|-----------|------------|-------------|
| Backend | FastAPI | Async, WebSocket support, минималистичный |
| Frontend | Vanilla JS + CSS | Нет build step, простота |
| Styling | CSS Variables + Flexbox | Тёмная/светлая тема, responsive |
| Icons | Unicode/Emoji | Без внешних зависимостей |
| WebSocket | Native API | Встроен в браузеры |

### 5. Интеграция с существующим кодом

```python
# server.py
from meeting_storage import MeetingStorage

storage = MeetingStorage()

@app.get("/api/meetings")
async def list_meetings():
    return storage.list_meetings()

@app.get("/api/meetings/{meeting_id}/transcript")
async def get_transcript(meeting_id: str):
    meeting = storage.get_meeting(meeting_id)
    return meeting["segments"]
```

### 6. Live-режим интеграция

**Вариант A: Чтение из файла (выбран)**
- Сервер наблюдает за `transcript.jsonl` через watchdog
- При изменении файла — отправка новых строк через WebSocket
- Плюс: не требует изменений в `mp4_notes.py`

**Вариант B: IPC через socket** (отложен)
- `mp4_notes.py` отправляет сегменты на локальный socket
- Сервер ретранслирует в WebSocket
- Минус: требует изменений в live transcriber

### 7. Диаграмма компонентов

```
┌─────────────────────────────────────────────────────┐
│                    Browser                          │
│  ┌──────────────────────────────────────────────┐  │
│  │              Vanilla JS SPA                   │  │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────────────┐ │  │
│  │  │Meetings │ │ Detail  │ │   Live View     │ │  │
│  │  │  List   │ │  View   │ │   (WebSocket)   │ │  │
│  │  └────┬────┘ └────┬────┘ └────────┬────────┘ │  │
│  └───────┼───────────┼───────────────┼──────────┘  │
└──────────┼───────────┼───────────────┼─────────────┘
           │ REST      │ REST          │ WS
           ▼           ▼               ▼
┌─────────────────────────────────────────────────────┐
│                 FastAPI Server                       │
│  ┌──────────────┐  ┌──────────────────────────────┐ │
│  │  REST API    │  │   WebSocket Manager          │ │
│  │  /api/*      │  │   - Live stream              │ │
│  └──────┬───────┘  │   - File watcher             │ │
│         │          └──────────────┬───────────────┘ │
└─────────┼─────────────────────────┼─────────────────┘
          │                         │
          ▼                         ▼
┌─────────────────────────────────────────────────────┐
│               File System                            │
│  meetings/                                           │
│  ├── index.json                                      │
│  └── {id}/                                           │
│      ├── meta.json                                   │
│      └── transcript.jsonl  ◄── watchdog              │
└─────────────────────────────────────────────────────┘
```

### 8. UI/UX Design

#### Тёмная тема по умолчанию
```css
:root {
  --bg-primary: #1a1a2e;
  --bg-secondary: #16213e;
  --text-primary: #eee;
  --text-secondary: #aaa;
  --accent: #e94560;
  --speaker-local: #4ade80;
  --speaker-other: #60a5fa;
}
```

#### Адаптивность
- Desktop: sidebar + main content
- Mobile: bottom tabs + scrollable content

### 9. Запуск

```bash
cd frontend
pip install -r requirements.txt
python server.py
# Opens http://localhost:8080
```

## Consequences

### Positive:
- Удобный просмотр всех записей с любого устройства
- Real-time наблюдение за live-транскрипцией
- Нет сложного build process (no npm/webpack)
- Минимальные зависимости (FastAPI + uvicorn)
- Легко расширять API

### Negative:
- Vanilla JS может стать сложным при большом функционале
- Нет готовых UI компонентов (всё писать вручную)
- File watching может пропустить быстрые обновления

### Mitigation:
- Использовать простую компонентную структуру в JS
- CSS-only компоненты где возможно
- Polling как fallback для file watching

## Alternatives Considered

### 1. React/Vue + Node.js
- **За:** Богатая экосистема, компоненты
- **Против:** Тяжёлый build step, node_modules, сложнее деплой
- **Решение:** Отклонено — over-engineering для простого UI

### 2. Streamlit
- **За:** Быстрый прототип, Python-only
- **Против:** Ограниченная кастомизация, медленный
- **Решение:** Отклонено — недостаточно гибкий

### 3. Flask + Jinja templates
- **За:** Проверенный стек
- **Против:** Server-side rendering не подходит для live updates
- **Решение:** Отклонено — нужен WebSocket для live view

## Future Work
- Экспорт в PDF
- Редактирование транскриптов (исправление ошибок ASR)
- Теги и категории для митингов
- Полнотекстовый поиск по всем митингам
- Аутентификация (если нужен публичный доступ)
