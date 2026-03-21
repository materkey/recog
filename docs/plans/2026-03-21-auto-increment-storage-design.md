# Auto Increment Storage — Design

## Summary

Автоматическая генерация и хранение "My increment" при каждом `ktalk_leave`. Версионирование при reconnect (v1, v2, ...). UI показывает сохранённые версии вместо повторной генерации.

## Requirements

1. При `ktalk_leave` — автоматически генерировать инкремент через Codex 5.4 xhigh
2. Хранить в `meetings/<id>/increments/v1.md`, `v2.md`
3. При reconnect в ту же встречу (coalescence) — генерировать новую версию
4. UI: кнопка "My increment" показывает сохранённые версии; если нет — генерирует
5. Генерация async, не блокирует leave flow

## Architecture

### Approach B: Отдельный IncrementGenerator + async

```
ktalk_bridge.py                    recog/increment.py
┌─────────────────┐                ┌──────────────────────┐
│ meeting_leave    │                │ IncrementGenerator   │
│ or disconnect    │───callback────▶│                      │
│                  │                │ 1. Read transcript   │
│ on_ktalk_leave() │                │ 2. Build prompt      │
│ returns new_id   │                │ 3. Call Codex        │
│                  │                │ 4. Write vN.md       │
│                  │                │ 5. Write vN_meta.json│
│                  │◀──notify───── │ 6. Notify frontend   │
└─────────────────┘                └──────────────────────┘
                                          │
                                          ▼ WebSocket
                                   ┌──────────────┐
                                   │ frontend      │
                                   │ app.js        │
                                   │ show versions │
                                   └──────────────┘
```

## Data Model

### Storage layout

```
meetings/2026-03-21_143025_abc12def/
  meta.json
  transcript.jsonl
  index.json
  increments/               ← NEW
    v1.md                   ← markdown output from Codex
    v1_meta.json            ← generation metadata
    v2.md                   ← after reconnect + second leave
    v2_meta.json
```

### vN_meta.json

```json
{
  "version": 1,
  "generated_at": "2026-03-21T14:45:00+03:00",
  "provider": "codex",
  "model": "gpt-5.4",
  "reasoning_effort": "xhigh",
  "segment_range": [0, 41],
  "segment_count": 42,
  "status": "completed",
  "duration_seconds": 18.3,
  "error": null
}
```

- `segment_range`: `[first_segment_id, last_segment_id]` — фиксирует срез транскрипта
- `status`: `"generating"` | `"completed"` | `"failed"`
- `error`: сообщение об ошибке если `status == "failed"`

### meta.json additions

Добавить в meta.json встречи:

```json
{
  "increments": {
    "count": 2,
    "latest_version": 2,
    "latest_status": "completed"
  }
}
```

## Module: recog/increment.py

```python
class IncrementGenerator:
    def __init__(self, meetings_base: Path):
        self._meetings_base = meetings_base
        self._on_increment_ready: Optional[Callable] = None

    def set_callback(self, callback: Callable[[str, int], None]):
        """callback(meeting_id, version) — called when increment is ready."""
        self._on_increment_ready = callback

    async def generate(self, meeting_id: str, meeting_path: Path):
        """
        Generate increment for a meeting. Called on ktalk_leave.
        Determines version number from existing increments/.
        Runs Codex, writes vN.md + vN_meta.json.
        """

    def list_increments(self, meeting_id: str) -> list[dict]:
        """Return list of increment metadata for a meeting."""

    def get_increment(self, meeting_id: str, version: int) -> Optional[dict]:
        """Return {meta: ..., content: "markdown..."} for a specific version."""
```

### Version numbering

- Scan `increments/` directory for existing `v*.md` files
- Next version = max existing + 1
- If no `increments/` dir — create it, version = 1

### Generation flow

1. Create `increments/` dir if needed
2. Write `vN_meta.json` with `status: "generating"`
3. Read `transcript.jsonl`, build transcript text
4. Build INCREMENT_PROMPT with user_name from meta.json
5. Call `CodexCliProvider().complete(prompt)`
6. Write `vN.md` with result
7. Update `vN_meta.json` with `status: "completed"`, timing
8. Update `meta.json` increments summary
9. Fire callback to notify frontend

### Error handling

- If Codex fails: write `vN_meta.json` with `status: "failed"`, `error: "message"`
- Don't write `vN.md` on failure
- Log error to stderr
- Frontend can show "Generation failed" with option to retry

## Integration Points

### 1. ktalk_bridge.py — trigger generation

В двух местах где вызывается `on_ktalk_leave()`:

**a) `meeting_leave` message (line ~1390):**
```python
new_meeting_id = self._meeting_storage.on_ktalk_leave(reason)
# NEW: trigger async increment generation for the PREVIOUS meeting
if self._increment_generator:
    prev_meeting_id = ...  # meeting_id before leave
    prev_meeting_path = ...  # path before leave
    asyncio.create_task(
        self._increment_generator.generate(prev_meeting_id, prev_meeting_path)
    )
```

**b) Client disconnect (line ~1171):** same pattern.

Нужно захватить `meeting_id` и `meeting_path` ДО вызова `on_ktalk_leave()`, т.к. после вызова storage уже указывает на новую встречу.

### 2. frontend/server.py — REST API

```python
@app.get("/api/meetings/{meeting_id}/increments")
async def list_increments(meeting_id: str) -> list[dict]:
    """List all increment versions for a meeting."""

@app.get("/api/meetings/{meeting_id}/increments/{version}")
async def get_increment(meeting_id: str, version: int) -> dict:
    """Get specific increment version content + meta."""
```

### 3. frontend/server.py — WebSocket notification

При готовности инкремента отправить через LiveWatcher:

```json
{
  "type": "increment_ready",
  "meeting_id": "2026-03-21_143025_abc12def",
  "version": 1,
  "status": "completed"
}
```

### 4. frontend/static/app.js — UI changes

Кнопка "My increment":
- `onclick` → fetch `/api/meetings/{id}/increments`
- Если есть сохранённые версии → показать dropdown/список с версиями
- Если нет версий → генерировать как раньше (через `analyze-stream`)
- Если версия со `status: "generating"` → показать spinner
- WebSocket `increment_ready` → обновить UI автоматически

UI для версий:
```
┌─────────────────────────────┐
│ My increment                │
│ ┌─────────────────────────┐ │
│ │ v2 — 14:45 (latest)   ▼│ │
│ ├─────────────────────────┤ │
│ │ v1 — 14:20             │ │
│ └─────────────────────────┘ │
│                             │
│ ## Задачи                   │
│ - Задача 1                  │
│ - Задача 2                  │
│                             │
│ ## Дедлайны                 │
│ - ...                       │
│                             │
│         [Regenerate]        │
└─────────────────────────────┘
```

## Edge Cases

1. **Пустая встреча (0 сегментов):** не генерировать инкремент — `_end_current_meeting` уже удаляет пустые встречи
2. **Очень короткая встреча (<3 сегментов):** генерировать, но Codex сам напишет "конкретных задач не назначено"
3. **Codex недоступен:** запишем failed meta, UI покажет ошибку + кнопку Retry
4. **Параллельные leave:** маловероятно, но version scan + atomic write защитит
5. **Meeting coalescence:** при reconnect в ту же комнату `on_meeting_join` возвращает тот же meeting_id → при следующем leave будет v2 с полным (расширенным) транскриптом

## Testing

1. **Unit test:** `IncrementGenerator` с mock Codex provider — проверить version numbering, file layout, meta.json
2. **Unit test:** Error handling — Codex failure записывает `status: "failed"`
3. **Integration test:** ktalk_leave → increment appears in meeting dir
4. **Frontend test:** кнопка показывает версии, WebSocket обновляет UI
