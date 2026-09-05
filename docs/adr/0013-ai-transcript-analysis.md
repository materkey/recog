# ADR-0013: AI-анализ транскриптов

## Status
Proposed

## Context

После встречи пользователю нужно быстро понять:
- Какой инкремент от него ожидается к следующему синку
- Какие action items были назначены
- Краткое summary встречи

Ручной анализ длинных транскриптов занимает время. AI может извлечь ключевую информацию за секунды.

### Требования:
1. Кнопка в UI для анализа транскрипта
2. Поддержка нескольких AI-провайдеров (разные бюджеты/подписки)
3. Имя пользователя брать из `local` participant
4. Минимальная стоимость (использовать подписки где возможно)

## Decision

### 1. Поддерживаемые провайдеры

| Провайдер | Способ вызова | Биллинг | Когда использовать |
|-----------|---------------|---------|-------------------|
| **Claude CLI** | `claude -p "..."` | Подписка Max $100 | Есть подписка Claude |
| **Codex CLI** | `codex exec --prompt "..."` | Подписка Pro $200 | Есть подписка ChatGPT |
| **OpenRouter** | HTTP API | Pay-per-token | Нет подписок / CI |

### 2. Архитектура

```
┌─────────────────────────────────────────────────────────────┐
│                        Frontend                              │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  Meeting Detail View                                 │    │
│  │  [Copy] [Export MD] [🤖 What's my increment?]       │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────┬───────────────────────────────────────┘
                      │ POST /api/meetings/{id}/analyze
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                     Backend (server.py)                      │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  /api/meetings/{id}/analyze                          │    │
│  │  - Load transcript                                   │    │
│  │  - Get local participant name                        │    │
│  │  - Build prompt                                      │    │
│  │  - Call AI provider                                  │    │
│  └─────────────────────┬───────────────────────────────┘    │
└─────────────────────────┼───────────────────────────────────┘
                          │
         ┌────────────────┼────────────────┐
         ▼                ▼                ▼
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│ Claude CLI  │  │ Codex CLI   │  │ OpenRouter  │
│ (subprocess)│  │ (subprocess)│  │ (HTTP)      │
└─────────────┘  └─────────────┘  └─────────────┘
```

### 3. Конфигурация

**recog/web/.env:**
```bash
# AI Provider: "claude" | "codex" | "openrouter"
AI_PROVIDER=claude

# Only for OpenRouter
OPENROUTER_API_KEY=sk-or-...
OPENROUTER_MODEL=anthropic/claude-3-haiku
```

### 4. AI Provider абстракция

**recog/web/ai_provider.py:**
```python
import os
import asyncio
from abc import ABC, abstractmethod

class AIProvider(ABC):
    @abstractmethod
    async def complete(self, prompt: str) -> str:
        pass

class ClaudeCliProvider(AIProvider):
    """Claude CLI - использует подписку Max/Pro."""

    async def complete(self, prompt: str) -> str:
        proc = await asyncio.create_subprocess_exec(
            "claude", "-p", prompt,
            "--output-format", "text",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )
        stdout, stderr = await proc.communicate()
        if proc.returncode != 0:
            raise Exception(f"Claude CLI error: {stderr.decode()}")
        return stdout.decode().strip()

class CodexCliProvider(AIProvider):
    """Codex CLI - использует подписку ChatGPT Pro."""

    async def complete(self, prompt: str) -> str:
        proc = await asyncio.create_subprocess_exec(
            "codex", "exec",
            "--prompt", prompt,
            "--output-format", "text",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )
        stdout, stderr = await proc.communicate()
        if proc.returncode != 0:
            raise Exception(f"Codex CLI error: {stderr.decode()}")
        return stdout.decode().strip()

class OpenRouterProvider(AIProvider):
    """OpenRouter API - pay-per-token."""

    def __init__(self):
        self.api_key = os.getenv("OPENROUTER_API_KEY")
        self.model = os.getenv("OPENROUTER_MODEL", "anthropic/claude-3-haiku")

    async def complete(self, prompt: str) -> str:
        import httpx
        async with httpx.AsyncClient() as client:
            response = await client.post(
                "https://openrouter.ai/api/v1/chat/completions",
                headers={"Authorization": f"Bearer {self.api_key}"},
                json={
                    "model": self.model,
                    "messages": [{"role": "user", "content": prompt}]
                },
                timeout=60.0
            )
            response.raise_for_status()
            return response.json()["choices"][0]["message"]["content"]

def get_provider() -> AIProvider:
    provider = os.getenv("AI_PROVIDER", "claude")
    if provider == "claude":
        return ClaudeCliProvider()
    elif provider == "codex":
        return CodexCliProvider()
    elif provider == "openrouter":
        return OpenRouterProvider()
    else:
        raise ValueError(f"Unknown AI provider: {provider}")
```

### 5. Промпты

**Increment prompt:**
```python
INCREMENT_PROMPT = """Проанализируй транскрипт рабочей встречи.

Участник: {user_name}

Ответь кратко (3-5 предложений) на вопрос:
Какой инкремент ожидается от {user_name} к следующему синку?

Выдели:
- Конкретные задачи
- Дедлайны (если упоминались)
- Ожидаемые результаты

Транскрипт:
{transcript}
"""
```

**Summary prompt (future):**
```python
SUMMARY_PROMPT = """Проанализируй транскрипт рабочей встречи.

Дай краткое summary (5-7 предложений):
- Основные темы обсуждения
- Ключевые решения
- Action items с ответственными

Транскрипт:
{transcript}
"""
```

### 6. Backend API

**Новый endpoint в server.py:**
```python
from pydantic import BaseModel
from ai_provider import get_provider, INCREMENT_PROMPT

class AnalyzeRequest(BaseModel):
    question: str = "increment"  # "increment" | "summary" | "custom"
    custom_prompt: str | None = None

class AnalyzeResponse(BaseModel):
    answer: str
    provider: str
    tokens_used: int | None = None

@app.post("/api/meetings/{meeting_id}/analyze")
async def analyze_meeting(meeting_id: str, request: AnalyzeRequest):
    # 1. Load meeting
    storage = MeetingStorage(MEETINGS_DIR)
    meeting = storage.get_meeting(meeting_id)
    if not meeting:
        raise HTTPException(404, "Meeting not found")

    # 2. Get local participant name
    participants = meeting["meta"].get("participants", {})
    local_participant = next(
        (p["name"] for p in participants.values() if p.get("is_local")),
        "User"
    )

    # 3. Build transcript text
    segments = meeting.get("segments", [])
    transcript = "\n".join(
        f"[{s['ts_display']}] {s['speaker']}: {s['text']}"
        for s in segments
    )

    # 4. Build prompt
    if request.question == "increment":
        prompt = INCREMENT_PROMPT.format(
            user_name=local_participant,
            transcript=transcript
        )
    elif request.question == "summary":
        prompt = SUMMARY_PROMPT.format(transcript=transcript)
    else:
        prompt = request.custom_prompt

    # 5. Call AI
    provider = get_provider()
    answer = await provider.complete(prompt)

    return AnalyzeResponse(
        answer=answer,
        provider=os.getenv("AI_PROVIDER", "claude")
    )
```

### 7. Frontend UI

**index.html - новая кнопка:**
```html
<div class="meeting-actions">
    <button class="btn" onclick="copyToClipboard()">Copy</button>
    <button class="btn" onclick="exportMarkdown()">Export MD</button>
    <button class="btn btn-ai" onclick="analyzeIncrement()">
        <span class="btn-ai-icon">🤖</span> My increment
    </button>
</div>

<!-- Modal для ответа AI -->
<div id="ai-modal" class="modal" style="display: none;">
    <div class="modal-content">
        <div class="modal-header">
            <h3>AI Analysis</h3>
            <button class="modal-close" onclick="closeAiModal()">&times;</button>
        </div>
        <div class="modal-body" id="ai-response">
            Loading...
        </div>
        <div class="modal-footer">
            <button class="btn" onclick="copyAiResponse()">Copy</button>
            <button class="btn" onclick="closeAiModal()">Close</button>
        </div>
    </div>
</div>
```

**app.js - функции:**
```javascript
async function analyzeIncrement() {
    const meeting = views.meeting;
    if (!meeting.id) return;

    // Show modal with loading
    document.getElementById('ai-modal').style.display = 'flex';
    document.getElementById('ai-response').innerHTML =
        '<div class="loading">Analyzing transcript...</div>';

    try {
        const response = await fetch(`/api/meetings/${meeting.id}/analyze`, {
            method: 'POST',
            headers: {'Content-Type': 'application/json'},
            body: JSON.stringify({question: 'increment'})
        });

        if (!response.ok) throw new Error('Analysis failed');

        const data = await response.json();
        document.getElementById('ai-response').innerHTML =
            `<div class="ai-answer">${escapeHtml(data.answer)}</div>
             <div class="ai-provider">via ${data.provider}</div>`;
    } catch (e) {
        document.getElementById('ai-response').innerHTML =
            `<div class="error">Error: ${e.message}</div>`;
    }
}

function closeAiModal() {
    document.getElementById('ai-modal').style.display = 'none';
}

function copyAiResponse() {
    const text = document.querySelector('.ai-answer')?.textContent;
    if (text) navigator.clipboard.writeText(text);
}
```

### 8. Стили для модального окна

**style.css:**
```css
.btn-ai {
    background: linear-gradient(135deg, #6366f1, #8b5cf6);
    border: none;
    color: white;
}

.btn-ai:hover {
    background: linear-gradient(135deg, #818cf8, #a78bfa);
}

.modal {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0,0,0,0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
}

.modal-content {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    max-width: 600px;
    width: 90%;
    max-height: 80vh;
    overflow: hidden;
}

.modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 16px;
    border-bottom: 1px solid var(--border);
}

.modal-body {
    padding: 16px;
    overflow-y: auto;
    max-height: 50vh;
}

.modal-footer {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding: 16px;
    border-top: 1px solid var(--border);
}

.ai-answer {
    white-space: pre-wrap;
    line-height: 1.6;
}

.ai-provider {
    margin-top: 16px;
    font-size: 12px;
    color: var(--text-muted);
}
```

### 9. План реализации

| Шаг | Файл | Описание |
|-----|------|----------|
| 1 | `recog/web/ai_provider.py` | Создать абстракцию провайдеров |
| 2 | `recog/web/.env.example` | Пример конфигурации |
| 3 | `recog/web/server.py` | Добавить `/api/meetings/{id}/analyze` |
| 4 | `recog/web/static/index.html` | Добавить кнопку и модальное окно |
| 5 | `recog/web/static/app.js` | Добавить analyzeIncrement() |
| 6 | `recog/web/static/style.css` | Стили для модалки и кнопки |
| 7 | `deploy/python/requirements-web.txt` | Добавить httpx (для OpenRouter) |

### 10. Ограничения и TODO

**Текущие ограничения:**
- Длинные транскрипты (>100k tokens) могут не поместиться в контекст
- CLI провайдеры требуют установленных и авторизованных `claude`/`codex`

**Future work:**
- Chunking длинных транскриптов
- Кеширование ответов
- Дополнительные вопросы (summary, action items, decisions)
- Streaming ответов для лучшего UX

## Consequences

### Positive:
- Быстрый анализ встреч без ручного чтения
- Гибкость в выборе провайдера (подписка vs API)
- Использование существующих подписок без доп. оплаты

### Negative:
- Зависимость от внешних CLI/API
- Лимиты подписок могут исчерпаться
- Качество ответов зависит от модели

### Mitigation:
- Fallback между провайдерами при ошибках
- Показывать warning при приближении к лимитам
- Возможность выбора модели в OpenRouter
