# Copilot Full-Transcript On Start — Implementation Plan

**Date**: 2026-05-04
**Status**: Draft (non-interactive — several decisions need user confirmation, see Open Questions)
**Reference**: `openspec/changes/archive/2026-04-27-ktalk-claude-copilot-chat/` (parent feature)
**Reference plans**: `docs/plans/2026-04-22-per-speaker-asr-pipeline.md` (decisions-table format), `docs/plans/completed/2026-04-26-ktalk-copilot-channel-scaffold.md` (channel scaffold)

## Цель / Проблема

Две связанные хотелки в copilot-режиме (Claude Code сидит «рядом» с активной KTalk-встречей через MCP `recog/copilot_channel.py`):

1. **Стандартный способ прочитать весь транскрипт встречи целиком, без ручного чанкинга.**
   Сейчас единственный путь — `mcp__copilot__get_recent_transcript(window_seconds, since_segment_id, include_refined)`. На больших окнах (>~25k токенов) tool result режется харнессом Claude Code (`MAX_MCP_OUTPUT_TOKENS=25000`), spillit в файл, и приходится потом читать через `Read` чанками по ~700 строк. Для встречи 60+ мин это ~2700 строк JSON ≈ 80 KB и ≥3 повторных Read-вызова. Нужно штатное API/хелпер, который выдаст всю встречу целиком (paged or compact), с предсказуемым форматом и без побочных эффектов на харнесс.

2. **На старте copilot-сессии весь текущий транскрипт сразу попадает в контекст.**
   Сейчас агент `copilot-session` (`.claude/agents/copilot-session.md`) знает только initialPrompt + загруженный skill `copilot-mode`. Если встреча уже идёт когда стартует Claude Code — копилот видит лишь rolling-window из `transcript_tick` (5+ новых сегментов или 15 с). Прошлое — невидимо, пока не попросишь `get_recent_transcript`. Нужно: при старте сессии (или при `meeting_started`) автоматически вытянуть полный буфер активной встречи и поместить его в контекст модели как одно сообщение, без явного prompt-а от пользователя.

## Decisions table (что предлагается; пункты «нужно подтверждение» помечены ⚠️)

| # | Решение | Значение | Альтернативы | Обоснование |
|---|---|---|---|---|
| D1 | **Формат полной выдачи** | **Compact text по умолчанию + параметр `format="json"\|"compact"`**. Compact: одна строка на сегмент `HH:MM:SS Speaker: text` (для refined — суффикс `*`). | (a) только JSON, (b) только compact, (c) markdown table | Compact даёт x3–5 экономию токенов vs JSON-объекты; для копилота канон — текст транскрипта, не структура. JSON оставлен для редких нужд (фильтр по segment_id и т.п.). |
| D2 | **Способ доставки большого транскрипта** | **MCP resource** `copilot://meetings/{id}/transcript[?format=...]` + дополнительно **paged tool** `get_full_transcript(meeting_id?, page=1, page_size_chars=15000, format="compact")` | (a) подкрутить `MAX_MCP_OUTPUT_TOKENS` через settings, (b) только resource, (c) только paged tool, (d) один tool, возвращающий путь к временному файлу | Resource — нативный путь Claude Code для длинного контента (стримится через `ReadMcpResourceTool`, обходит лимит tool result). Paged tool — fallback для агентов/клиентов, у которых MCP resources не поддержаны полноценно. Settings-bump (`MAX_MCP_OUTPUT_TOKENS=200000`) отбрасываем — действует на ВСЕ tools, маскирует баги других серверов и не решает корневую проблему. ⚠️ Нужно подтверждение: ок ли иметь оба или только resource. |
| D3 | **Где живёт код для полного транскрипта** | **Bridge-side**: bridge принимает `get_full_transcript_request`, читает `transcript.jsonl` через `MeetingStorage.get_current_transcript()` (или новый метод `get_meeting_transcript(meeting_id)` для исторических), отвечает `get_full_transcript_response`. Channel server (`recog/copilot_channel.py`) экспонирует это через MCP tool/resource. | (a) channel server читает JSONL напрямую (минуя bridge), (b) in-memory `_copilot_transcript_buffer` (cap=300) | In-memory буфер ограничен 300 сегментами (~25–35 мин) — недостаточно. JSONL — single source of truth. Bridge уже владеет meeting state, имеет доступ к `MeetingStorage` и единственный знает текущий `meeting_id`. Channel server в идеале остаётся stateless. |
| D4 | **Auto-load на старте: триггер** | **Hook `SessionStart` в `.claude/settings.json`** для агента `copilot-session`: shell-скрипт спрашивает channel server (или bridge напрямую) «есть ли активная встреча?» — если да, печатает в stdout полный транскрипт в compact-формате. Хук добавляет это в контекст через standard hook output mechanism (см. `additionalContext` field в SessionStart hook output). | (a) push-уведомление через `notifications/claude/channel` на старте, (b) ручная команда `/copilot-load`, (c) initialPrompt инструктирует Claude вызвать tool сразу | SessionStart hook — самый чистый: контекст приходит до первого user message, без token-cost'а на tool call. Channels-push на старте требует, чтобы Claude уже умел listen'ить — race с инициализацией. ⚠️ Уточнить: какой формат hook output Claude Code понимает для inject в системный/user-context — нужно подтверждение пути инжекта (см. Open Questions). |
| D5 | **Что считать «стартом» для auto-load** | **Оба триггера**: (a) `SessionStart` hook — если встреча уже идёт; (b) `meeting_started` event — если встреча начинается в середине сессии копилота, channel server уже умеет push'ить её через `notifications/claude/channel` (`copilot_channel.py:_format_channel_event`), расширим payload включая «вот полный транскрипт пока» (на старте встречи он почти пустой, но sequence симметричный). | Только SessionStart, только meeting_started | Полный охват: «пришёл, встреча идёт» И «уже сижу, началась встреча» — обе ситуации валидны для копилота. ⚠️ На `meeting_started` транскрипт почти пустой (~0–3 сегмента), так что это no-op чаще всего. |
| D6 | **Лимит на размер auto-load** | **Cap по символам, не по сегментам**: `COPILOT_AUTOLOAD_MAX_CHARS=80000` (≈25k токенов compact-формата). Если транскрипт длиннее — урезаем «сначала», оставляя последние N символов + одну строку-маркер `[truncated: prior segments hidden, use copilot://meetings/{id}/transcript for full]`. | (a) обрезать в начале, (b) обрезать в середине, (c) summarize старое через AI, (d) hard error если > cap | Хвост важнее головы (свежий контекст для копилота). Summarize в hook'е — отдельная фича, плодит зависимости (нужна AI-провайдер инвокация в hook'е, может медленно). MCP resource остаётся доступным для глубокой выгрузки. ⚠️ Конкретное число `80000` — оценочное, нужно подтверждение порога. |
| D7 | **Refined vs raw text в полной выдаче** | **Только refined где доступно, fallback raw**, маркер `*` при compact. Параметр `include_refined: bool = True`. | Возвращать обе версии, только raw, только refined без fallback | Refined — каноничнее (см. ADR-0019). Если для сегмента ещё нет refine — отдаём live. Двойную выдачу не делаем — удваивает токены ради edge case. |
| D8 | **Где хранится sequence-watermark для consumer'а** | **Возвращаем `last_segment_id` в response** (как resource metadata и как поле tool result). Consumer может позже звонить `get_recent_transcript(since_segment_id=last_segment_id)` для инкрементального обновления. | Не возвращать; возвращать только timestamp | Бесплатный признак, упрощает hand-off от auto-load → подписка на rolling. Не ломает существующий `get_recent_transcript`. |
| D9 | **Регистрация SessionStart hook'a — где** | **Project-level `.claude/settings.json`** (новый файл — сейчас не существует). Применяется только в этом repo, не загрязняет глобальный `~/.claude/settings.json`. Команда хука: `bash recog/scripts/copilot_autoload_transcript.sh` (новый скрипт). | User-level settings, hardcode в агенте | Проектная локализация: hook не должен срабатывать в обычных сессиях `claude` в других репах. Скрипт-обёртка изолирует logic от JSON-конфига. ⚠️ Нужно подтверждение: project-level settings.json допустимо чекинить (или должно быть `settings.local.json` gitignored)? |
| D10 | **Channel server — endpoint для hook-скрипта** | **Новый HTTP endpoint** на channel server'е: `GET http://127.0.0.1:8767/copilot/active-meeting-transcript?format=compact` (без MCP-обёртки). Hook-скрипт делает curl, получает body или 204 (no active meeting). | (a) hook-скрипт делает MCP-инициализацию + tool call, (b) hook-скрипт читает `meetings/<latest>/transcript.jsonl` напрямую, (c) hook-скрипт коннектится к bridge WS | (a) — тяжело, MCP-handshake + JSONRPC ради одного запроса. (b) — обходит logic «какая встреча сейчас активна» (надо реплицировать). (c) — лишняя WS-сессия. Plain HTTP проще, channel server уже слушает 8767 для MCP — добавим side-route. ⚠️ Нужно подтверждение: ок ли эту сторону держать non-MCP, или предпочесть единый MCP-протокол даже для hook'а? |
| D11 | **Когда транскрипт > cap — что в hook output** | Hook печатает: (1) последние `COPILOT_AUTOLOAD_MAX_CHARS` chars, (2) маркер `[truncated]`, (3) явная подсказка Claude: «Полный транскрипт доступен через MCP resource `copilot://meetings/{meeting_id}/transcript`, вызови `ReadMcpResourceTool` если нужно увидеть pre-cap сегменты». | Не упоминать resource, тихо обрезать | Без подсказки модель не догадается, что есть способ прочитать полное. Подсказка дешёвая (один абзац). |
| D12 | **Backwards compat** | `get_recent_transcript` остаётся **без изменений**. Новый tool/resource — параллельный путь. Никаких breaking changes для существующего copilot-mode skill, agent, extension UI. | Заменить get_recent_transcript на get_full_transcript с дефолтным window_seconds | get_recent_transcript нужен для частых snapshot'ов «свежее за последние 3 мин» — это другой use case. |

## Архитектура (sequence)

### Кейс A: `claude` стартует, KTalk-встреча уже идёт

```
1. User: claude (starting copilot-session agent or normal)
2. Claude Code harness: SessionStart hook fires
   bash recog/scripts/copilot_autoload_transcript.sh
3. Hook script: curl http://127.0.0.1:8767/copilot/active-meeting-transcript?format=compact
4. Channel server (recog/copilot_channel.py):
   - asyncio task on the same uvicorn that hosts /mcp serves /copilot/active-meeting-transcript
   - asks bridge via WS: {"type": "get_full_transcript_request", "request_id": "..."}
   - bridge: get_current_meeting_id() -> read transcript.jsonl via MeetingStorage
   - bridge -> {"type": "get_full_transcript_response", segments, last_segment_id, meeting_id, meeting_title}
   - channel server formats compact text, returns HTTP 200 (or 204 if no active meeting)
5. Hook script: prints to stdout (truncated to COPILOT_AUTOLOAD_MAX_CHARS) plus header line
   "[copilot autoload] active meeting <title> — N segments since HH:MM:SS"
6. Claude Code harness injects stdout as additionalContext into the first user message
7. Claude opens with full meeting context already in scope
```

### Кейс B: Сессия копилота сидит, новая встреча начинается

```
1. User joins KTalk → bridge fires meeting_started
2. ktalk_bridge.py broadcasts to channel clients (existing flow)
3. recog/copilot_channel.py._handle_inbound enqueues meeting_started + pushes
   notifications/claude/channel (existing)
4. Сейчас payload содержит meeting_title (см. _format_channel_event:253) — НО не содержит "вот предыдущий контекст" потому что для нового meeting'а ничего нет.
5. Так как это case "встреча только началась" — транскрипт пуст, auto-load не нужен. Skill copilot-mode уже ловит meeting_started и сбрасывает state.
6. (Edge case) Если bridge поднялся посреди уже идущего KTalk meeting'а (например, restart of recog-service) → meeting_started НЕ эмитится, потому что встреча была заджойнена раньше. В этом случае channel server при reconnect к bridge может запросить полный транскрипт активной встречи и push'нуть его как notifications/claude/channel (новая логика). См. Task 4.
```

## Где в коде что меняем

- `recog/ktalk_bridge.py` — новый dispatcher case `get_full_transcript_request` → читает `MeetingStorage` (текущий meeting), формирует response. Прокидывает meeting_id, meeting_title, last_segment_id.
- `recog/meeting_storage.py` — возможно переиспользовать `get_current_transcript()` (уже читает JSONL целиком). Добавить тонкую обёртку `get_meeting_transcript_or_current(meeting_id: Optional[str])` для случая, когда ID не передан.
- `recog/copilot_channel.py`:
  - Новый MCP tool `get_full_transcript(format="compact", page=1, page_size_chars=15000, include_refined=True, meeting_id=None) -> dict`. Внутри запрашивает bridge `get_full_transcript_request`, paginates по символам.
  - Новый MCP resource `copilot://meetings/{id}/transcript` (или дефолт `copilot://meetings/active/transcript`). Реализуется через `@mcp.resource(...)` декоратор FastMCP. Полный текст без paging — поток через resource gracefully handled харнессом.
  - Side-route на uvicorn'е (FastMCP внутри использует Starlette/uvicorn): `GET /copilot/active-meeting-transcript` для hook-скрипта (D10).
- `recog/scripts/copilot_autoload_transcript.sh` — новый bash-скрипт: `curl -s --max-time 3 http://127.0.0.1:8767/copilot/active-meeting-transcript?format=compact ...`.
- `.claude/settings.json` (новый файл в repo) — `SessionStart` hook regs командой выше. ⚠️ Уточнить: settings.json или settings.local.json (gitignored)?
- `.claude/skills/copilot-mode/SKILL.md` — добавить параграф «при старте сессии в `<session_start_context>` (или аналог) приходит полный транскрипт активной встречи; используй его как baseline, не дёргай `get_recent_transcript` повторно если есть свежее».
- `tests/test_copilot_channel.py`, `tests/test_ktalk_bridge.py` — расширить.

## Tasks

### Task 1: Bridge — `get_full_transcript_request` handler

**Goal**: bridge умеет по WS запросить полный транскрипт активной (или указанной) встречи, отвечает структурированным response.

- [ ] 1.1 В `recog/meeting_storage.py` добавить метод `get_meeting_transcript(meeting_id: str) -> list[dict]` — читает `meetings/<id>/transcript.jsonl` для произвольного meeting_id (не только текущего). Если meeting_id не существует → пустой список.
- [ ] 1.2 В `recog/ktalk_bridge.py` `_process_message`: добавить ветку `elif msg_type == "get_full_transcript_request":` после `get_recent_transcript_request`. Поля: `request_id` (required), `meeting_id` (optional — fallback на current).
- [ ] 1.3 Handler читает segments через `MeetingStorage.get_current_transcript()` (если meeting_id не передан) или `get_meeting_transcript(meeting_id)`. Формирует response:
  ```
  {
    "type": "get_full_transcript_response",
    "request_id": ...,
    "meeting_id": ...,
    "meeting_title": ...,
    "segments": [...],
    "last_segment_id": int | null,
    "total_count": int
  }
  ```
  Отправляет только originating socket (как `get_recent_transcript_request`).
- [ ] 1.4 Если активной встречи нет И `meeting_id` не передан → response с пустыми `segments` и `meeting_id: null` (НЕ ошибка — channel server превратит в 204).
- [ ] 1.5 Unit tests в `tests/test_ktalk_bridge.py`:
  - `test_get_full_transcript_returns_all_segments_for_current_meeting`: seed JSONL через MeetingStorage, send request, assert response.
  - `test_get_full_transcript_with_explicit_meeting_id_reads_other_meeting`.
  - `test_get_full_transcript_no_active_meeting_returns_empty`.
  - `test_get_full_transcript_invalid_request_id_logged_not_dispatched`.

**Acceptance**: `pytest tests/test_ktalk_bridge.py -k "full_transcript" -v` зелёный.

**Files**: `recog/meeting_storage.py`, `recog/ktalk_bridge.py`, `tests/test_ktalk_bridge.py`.

**Commit**: `bridge: add get_full_transcript request/response for whole-meeting snapshot`

---

### Task 2: Channel server — MCP tool `get_full_transcript` + compact formatter

**Goal**: copilot-сервер экспонирует тулзу, которая получает полный транскрипт от bridge и отдаёт компактный текст с пагинацией.

- [ ] 2.1 В `recog/copilot_channel.py` добавить helper `_format_segments_compact(segments: list[dict]) -> str`. Каждый сегмент → строка `HH:MM:SS Speaker: text` (timestamp из `start_ms`, перевод в локальное время). Refined segments: суффикс ` *` после speaker. Empty text → skip.
- [ ] 2.2 Добавить MCP tool `get_full_transcript(format="compact", page=1, page_size_chars=15000, include_refined=True, meeting_id=None) -> dict`. Логика:
  - Через `link.send` отправить `get_full_transcript_request` с `request_id = full_<uuid>`.
  - Зарегистрировать pending future, ждать `get_full_transcript_response` (timeout 10s — больше чем у recent, JSONL может быть большим).
  - Если `format == "compact"`: применить `_format_segments_compact`, нарезать по `page_size_chars` (page-границы по newline'у, не в середине строки), вернуть `{"ok": True, "page": p, "total_pages": N, "text": "...", "meeting_id", "meeting_title", "last_segment_id"}`.
  - Если `format == "json"`: вернуть raw segments с теми же metadata.
- [ ] 2.3 Расширить `_handle_inbound`: при `msg_type == "get_full_transcript_response"` — `link.resolve_pending(request_id, data)` (как для `get_recent_transcript_response`).
- [ ] 2.4 Обновить `FastMCP.instructions` (system prompt): добавить упоминание новых tools/resource ниже существующего блока tool-list. ⚠️ Аккуратно: instructions уже большие, не раздувать.
- [ ] 2.5 Unit tests в `tests/test_copilot_channel.py`:
  - `test_get_full_transcript_returns_compact_text_for_short_meeting`: mock bridge response, assert text format.
  - `test_get_full_transcript_paginates_when_over_size`: synthetic 100 segments, page_size_chars=200, assert N pages, no text loss across boundaries.
  - `test_get_full_transcript_format_json_returns_raw_segments`.
  - `test_get_full_transcript_no_active_meeting_returns_empty_text_with_metadata`.
  - `test_format_segments_compact_marks_refined_with_asterisk`.

**Acceptance**: `pytest tests/test_copilot_channel.py -k "full_transcript or format_segments" -v` зелёный.

**Files**: `recog/copilot_channel.py`, `tests/test_copilot_channel.py`.

**Commit**: `copilot: add get_full_transcript MCP tool with compact text + pagination`

---

### Task 3: Channel server — MCP resource `copilot://meetings/{id}/transcript`

**Goal**: альтернативный путь для агентов, у которых ReadMcpResourceTool работает лучше чем большие tool result'ы.

- [ ] 3.1 В `recog/copilot_channel.py` зарегистрировать resource через FastMCP API. Schema URI: `copilot://meetings/{meeting_id}/transcript` плюс alias `copilot://meetings/active/transcript` (последний резолвится к текущей встрече).
- [ ] 3.2 Resource handler reuses ту же `link.send(get_full_transcript_request)` логику. Возвращает `text/plain` с compact-форматом (без paging — resource протокол должен сам отдавать full content одним блобом, харнесс умеет читать длинно через resource API).
- [ ] 3.3 Опционально: ресурс `copilot://meetings/active/transcript.json` для JSON-формата (если FastMCP поддерживает несколько resource_types).
- [ ] 3.4 Unit tests:
  - `test_resource_active_meeting_transcript_returns_compact_text`.
  - `test_resource_unknown_meeting_id_returns_404_or_empty`.
  - ⚠️ FastMCP test harness для resource'ов — посмотреть как уже тестятся (если нет — добавить smoke через прямой вызов handler'а).

**Acceptance**: `pytest tests/test_copilot_channel.py -k resource -v` зелёный. Manual: `curl -X POST .../mcp` с `resources/read` для `copilot://meetings/active/transcript` отдаёт текст.

**Files**: `recog/copilot_channel.py`, `tests/test_copilot_channel.py`.

**Commit**: `copilot: expose meeting transcripts as MCP resources`

---

### Task 4: Channel server — HTTP side-route для hook-скрипта

**Goal**: `GET /copilot/active-meeting-transcript?format=compact` отдаёт текст без MCP-handshake — для bash-хука.

- [ ] 4.1 В `recog/copilot_channel.py._build_mcp` или вокруг `mcp.run_streamable_http_async` — добавить custom Starlette route. FastMCP под капотом использует Starlette `app`; у `mcp.streamable_http_app()` есть способ extend routes (см. SDK docs/source — если нет — обернуть FastMCP app в внешний Starlette с extra route и перенаправлять `/mcp` через mount).
- [ ] 4.2 Endpoint logic:
  - Параметры: `format` (compact|json, default compact), `max_chars` (int, default 80000).
  - Внутри: тот же `link.send(get_full_transcript_request)`. Если bridge не connected → HTTP 503. Если активной встречи нет → HTTP 204 No Content. Иначе HTTP 200 + body.
  - Body для compact: header строка `[copilot autoload] meeting=<title> id=<id> segments=N first=HH:MM:SS last=HH:MM:SS\n\n` + текст (truncated с tail-приоритетом если > max_chars + явный маркер `[... truncated, see copilot://meetings/<id>/transcript ...]`).
- [ ] 4.3 Unit tests:
  - `test_http_endpoint_returns_compact_text_with_header`.
  - `test_http_endpoint_returns_204_when_no_active_meeting`.
  - `test_http_endpoint_returns_503_when_bridge_disconnected`.
  - `test_http_endpoint_truncates_to_max_chars_with_marker`.

**Acceptance**: `pytest tests/test_copilot_channel.py -k http_endpoint -v` зелёный. Manual: `./recog-service.sh restart`, во время активной встречи `curl http://127.0.0.1:8767/copilot/active-meeting-transcript` отдаёт текст.

**Files**: `recog/copilot_channel.py`, `tests/test_copilot_channel.py`.

**Commit**: `copilot: add /copilot/active-meeting-transcript HTTP side-route`

---

### Task 5: Hook script + SessionStart wiring

**Goal**: `claude` старт автоматически инжектит транскрипт активной встречи в контекст первого user сообщения.

- [ ] 5.1 Создать `recog/scripts/copilot_autoload_transcript.sh`:
  - `#!/usr/bin/env bash; set -euo pipefail`
  - `URL="http://127.0.0.1:8767/copilot/active-meeting-transcript?format=compact&max_chars=${COPILOT_AUTOLOAD_MAX_CHARS:-80000}"`
  - `RESPONSE=$(curl -sS --max-time 3 -o /tmp/copilot_autoload.$$.txt -w "%{http_code}" "$URL" || echo "000")`
  - 200 → `cat /tmp/copilot_autoload.$$.txt`; 204 → silent exit 0; иначе → `echo "[copilot autoload] not available (HTTP $RESPONSE)" >&2; exit 0` (не failing, иначе hook ломает старт `claude`).
  - cleanup tmp файла в trap.
  - ⚠️ Format hook output: посмотреть/уточнить (см. Open Q1) — может потребоваться JSON wrapper вида `{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "..."}}` через jq или heredoc. Сейчас пишем обычный stdout.
- [ ] 5.2 chmod +x `recog/scripts/copilot_autoload_transcript.sh`.
- [ ] 5.3 Создать `.claude/settings.json` (новый файл в проекте):
  ```json
  {
    "hooks": {
      "SessionStart": [
        {
          "matcher": "*",
          "hooks": [
            {"type": "command", "command": "bash recog/scripts/copilot_autoload_transcript.sh", "timeout": 5}
          ]
        }
      ]
    }
  }
  ```
  ⚠️ Подтвердить: project-level `settings.json` чекаем в git (этот файл — часть контракта repo) или в `settings.local.json` (gitignored)? Гайдлайн «не меняй settings.json» в моём CLAUDE.md касается user-level — здесь project-level, других правил нет. Предложение: `settings.json` (зачекать), но **обязательно подтвердить с пользователем перед коммитом**.
- [ ] 5.4 Unit test для скрипта (`tests/test_copilot_autoload_script.py` или bash-test): мок curl через локальный python http.server, проверить exit codes для 200/204/503.

**Acceptance**: `bash recog/scripts/copilot_autoload_transcript.sh` при активной встрече печатает текст; при no meeting — silent. SessionStart hook срабатывает в `claude` (smoke).

**Files**: `recog/scripts/copilot_autoload_transcript.sh` (new), `.claude/settings.json` (new), `tests/test_copilot_autoload_script.py` (new).

**Commit**: `copilot: SessionStart hook auto-injects active meeting transcript`

---

### Task 6: copilot-mode skill + agent prompt update

**Goal**: skill объясняет, что делать с авто-инжектированным транскриптом; не дёргать `get_recent_transcript` без необходимости.

- [ ] 6.1 В `.claude/skills/copilot-mode/SKILL.md` добавить секцию `## Session bootstrap (autoload)`:
  - При старте сессии в первом user message может оказаться блок `[copilot autoload] meeting=<title> id=<id> segments=N first=HH:MM:SS last=HH:MM:SS` + сжатый транскрипт.
  - Это caretaker context — не отвечать на него, не комментировать. Использовать как baseline для последующих `transcript_tick` / `copilot_user_message`.
  - Если блок содержит `[truncated, see copilot://meetings/<id>/transcript]` — при первом запросе про прошлый контекст вызывать `ReadMcpResourceTool` для resource или `get_full_transcript(page=N)` для нужной страницы. **Не** дёргать `get_recent_transcript` с большим окном — для глубокого контекста есть resource.
- [ ] 6.2 В `.claude/agents/copilot-session.md` `initialPrompt` добавить одну строку: «Если в первом сообщении пришёл блок `[copilot autoload]` — это снимок активной встречи, не отвечай на него, держи как контекст».

**Acceptance**: `cat .claude/skills/copilot-mode/SKILL.md | grep -A 5 autoload` показывает новую секцию.

**Files**: `.claude/skills/copilot-mode/SKILL.md`, `.claude/agents/copilot-session.md`.

**Commit**: `copilot-mode: document session bootstrap autoload + full-transcript resource`

---

### Task 7: Integration smoke + docs

**Goal**: end-to-end проверка: hook fires, bridge отвечает, текст в контексте.

- [ ] 7.1 Manual smoke (operator): запустить `./recog-service.sh restart`, зайти в KTalk-встречу 5+ мин, добавить несколько сегментов в JSONL (или говорить вживую), затем в новом терминале запустить `claude` (агент `copilot-session`). Проверить логи: SessionStart hook отработал, в первом сообщении присутствует блок autoload.
- [ ] 7.2 Manual edge case: запустить `claude` БЕЗ активной встречи — hook должен silent exit, никаких ошибок в `claude` startup.
- [ ] 7.3 Manual edge case: длинная встреча (создать тестовый JSONL >100 KB через скрипт-фабрикатор) — проверить, что hook truncate'ит и оставляет маркер.
- [ ] 7.4 Обновить `CLAUDE.md` (project-level): новая секция «Copilot session bootstrap» с описанием hook-цепочки, env var `COPILOT_AUTOLOAD_MAX_CHARS`, путей.
- [ ] 7.5 Обновить `recog/copilot_channel.py` module docstring: упомянуть новые tool/resource/HTTP-route в comment'е сверху файла.

**Acceptance**: smoke checklist в Smoke result секции (template ниже) пройден; CLAUDE.md обновлён.

**Files**: `CLAUDE.md`, `recog/copilot_channel.py` (docstring).

**Commit**: `docs: copilot session bootstrap and full-transcript access`

---

### Task 8: OpenSpec proposal (next step, not in this plan's commits)

**Goal**: формализовать как OpenSpec change-set после ручного smoke.

- [ ] 8.1 Создать `openspec/changes/copilot-full-transcript-bootstrap/{proposal,design,tasks}.md`. Скоп: новые tool/resource/HTTP-endpoint/SessionStart hook. Ссылки на этот plan и parent change `ktalk-claude-copilot-chat`.
- [ ] 8.2 Когда smoke OK → апдейт `openspec/specs/ktalk-copilot-chat/spec.md` с новыми capabilities.

**Note**: эта задача делается отдельным запросом пользователя — план только её фиксирует, ничего не создаёт сейчас.

---

## Тестовая стратегия

| Слой | Что покрываем |
|---|---|
| Unit (bridge) | `get_full_transcript_request` happy-path, no-active-meeting, explicit meeting_id, malformed request |
| Unit (channel server) | tool returns compact text, paging boundaries, JSON format, formatter marks refined, resource handler |
| Unit (channel HTTP route) | 200/204/503, truncation marker, tail-priority |
| Unit (hook script) | curl 200/204/503 codes → correct stdout/exit |
| Manual smoke | live меетинг → hook → claude получает контекст; long meeting → truncate; no meeting → no-op |
| Regression | существующий `get_recent_transcript`, `await_copilot_event`, `send_copilot_message` не сломаны (run полный `pytest tests/test_copilot_channel.py tests/test_ktalk_bridge.py`) |

⚠️ **Edge case без покрытия unit'ом** (only manual): SessionStart hook actually injecting context into Claude Code's first turn — это поведение харнесса, не нашего кода. Best мы можем — проверить, что скрипт печатает в stdout правильный формат; интеграцию с моделью валидирует только manual smoke.

## Open questions (нужно подтверждение пользователя)

1. **Формат hook output для inject в контекст**. SessionStart hook'и в Claude Code могут возвращать stdout как `additionalContext` (через JSON wrapper `{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "..."}}` или через plain stdout). Какой формат сейчас работает у тебя? Если знаешь — поправь Task 5.1 соответственно. Если нет — попробую plain stdout первым, fallback на JSON wrapper если не сработает.

2. **Resource vs только-paged-tool (D2)**. Реально ли в твоей версии Claude Code (`opus 4.7`) `ReadMcpResourceTool` адекватно стримит длинный контент без обрезаний — или резоннее ставить только на paged tool + side-HTTP? Если не уверен — оставляем оба (текущий план), можно будет дропнуть resource если избыточен.

3. **`MAX_MCP_OUTPUT_TOKENS` bump (D2 alt)**. Точно НЕ хочешь подкрутить дефолт через `~/.claude/settings.json` env? Например `200000` для конкретно этого проекта через project-level settings? Это решило бы проблему №1 в одну строчку, но влияет на ВСЕ MCP-серверы (риск раздуть контекст другими серверами).

4. **`.claude/settings.json` — чекать в git?** Project-level settings.json — часть контракта repo (хук нужен всем, кто клонирует), значит чекать. Но global instruction говорит «NEVER edit `.env` or environment variable files — only the user». Settings.json не env, но хочу убедиться. Альтернатива — `settings.local.json` (gitignored), тогда каждый разработчик сам ставит. Какой путь?

5. **Cap `COPILOT_AUTOLOAD_MAX_CHARS=80000`** (D6) — норм оценка? 80K chars compact ≈ 80K байт ≈ 25K токенов compact-формата ≈ 600 строк ≈ 60 минут разговора при нормальном темпе. Если встречи короче — можно поднять до 200K. Если копилот часто гоняется на 4-часовые — снижать или ставить summarize.

6. **HTTP side-route на channel server (D10)** — ок ли что endpoint без auth? Channel server на 127.0.0.1, локально — но прецедент «просто HTTP на 8767» создаёт. Альтернатива — UNIX-socket. Решил в плане в пользу HTTP (проще), но open для возражений.

7. **Auto-load на `meeting_started` (D5)** — правда ли надо? Транскрипт пуст в этот момент (~0–3 сегмента), доступен через `get_recent_transcript`. Может полностью дропнуть кейс B и оставить только SessionStart? Аргумент за: симметрия (любая транзиция «копилот без контекста» → «копилот с контекстом» обработана). Аргумент против: noise.

8. **Тестируемость SessionStart** — как ты обычно отлаживаешь hook'и (вручную через `claude --debug`?)? Если есть налаженный паттерн — Task 7.1 может быть короче.

## Связь с существующими планами и спеками

- **Parent OpenSpec**: `openspec/changes/archive/2026-04-27-ktalk-claude-copilot-chat/` — этот план продолжает task 4.x (manual chat tracer) и 5.x (transcript snapshot tool) с новой capability «полный транскрипт + bootstrap».
- **ADR-0019** (`docs/adr/0019-per-speaker-stream-refine.md`) — refined-сегменты учитываются в compact-форматтере (D7).
- **`docs/plans/2026-04-22-per-speaker-asr-pipeline.md`** — образец decisions-table формата (D1–D10).
- **`docs/plans/completed/2026-04-26-ktalk-copilot-channel-scaffold.md`** — образец per-task structure для bridge handlers + channel server tools.
- **Следующий шаг (не в этом плане)**: создать `openspec/changes/copilot-full-transcript-bootstrap/` с `proposal.md` + `design.md` + `tasks.md` после ручного smoke (Task 8).

## Rollback

- **Hook**: убрать SessionStart из `.claude/settings.json` или удалить файл.
- **Tool/resource**: убрать tool registrations в `recog/copilot_channel.py._build_mcp` — channel server рестартует, инвентарь обновлён без рестарта Claude Code.
- **HTTP side-route**: удалить custom Starlette route, рестарт channel server.
- **Bridge handler**: ветка `get_full_transcript_request` в `_process_message` не аффектит остальной dispatcher — удаление чистое.

## Smoke result (заполняется после прогона)

```
Дата прогона: YYYY-MM-DD
Длительность встречи: NN мин (или synthetic JSONL N сегментов)
Тип сценария: A (claude старт во время встречи) / B (long meeting truncation) / C (no meeting)

Acceptance checks:
- PASS/FAIL: SessionStart hook fires (видно в `claude --debug` или logs)
- PASS/FAIL: при активной встрече блок `[copilot autoload]` появляется в первом контексте
- PASS/FAIL: при no-meeting hook silent (нет ошибок старта claude)
- PASS/FAIL: при > cap встрече транскрипт truncate'нут, маркер `[... truncated ...]` присутствует
- PASS/FAIL: `get_full_transcript` MCP tool отвечает compact-текстом
- PASS/FAIL: MCP resource `copilot://meetings/active/transcript` читается через ReadMcpResourceTool
- PASS/FAIL: regression — `get_recent_transcript`, `send_copilot_message` работают как раньше

Follow-ups:
- …
```
