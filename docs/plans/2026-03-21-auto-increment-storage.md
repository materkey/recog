# Auto Increment Storage

## Overview
- Auto-generate "My increment" (AI task summary) on each `ktalk_leave` event
- Store versioned outputs in `meetings/<id>/increments/v1.md`, `v2.md` with YAML frontmatter (RAG-ready)
- Delta-generation: vN uses only segments after v(N-1), no duplication
- UI: "My increment" button shows stored versions; falls back to SSE generation if none exist
- AI provider: Codex 5.4 xhigh (`CodexCliProvider`)

## Context (from discovery)
- Files/components involved:
  - `recog/increment.py` (new) — IncrementGenerator module
  - `ktalk_bridge.py:1161-1175,1374-1395` — two leave trigger points
  - `recog/live.py` — wiring IncrementGenerator into session startup
  - `frontend/server.py` — REST API for increments
  - `frontend/static/app.js:1362-1416` — UI for stored versions
  - `frontend/static/index.html:110-130` — modal markup
  - `frontend/ai_provider.py:33-56` — INCREMENT_PROMPT, CodexCliProvider
- Related patterns: `meeting_storage.py` stores per-meeting data in `meetings/<id>/`; `on_ktalk_leave()` ends meeting and creates new orphan; KTalkBridge uses callbacks (`_on_meeting_change`, `_on_pre_meeting_switch`)
- Dependencies: `CodexCliProvider` from `frontend/ai_provider.py`; `MeetingStorage` for transcript/meta access

## Development Approach
- **Testing approach**: TDD — tests first for core module, then integration
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- Run tests after each change
- Maintain backward compatibility

## Testing Strategy
- **Unit tests**: `tests/test_increment.py` — version numbering, delta ranges, frontmatter, error handling
- **Existing tests**: `tests/test_ktalk_bridge.py` — must continue to pass after bridge changes
- **Smoke test**: `python mp4_notes.py --help` — no import errors
- **Manual E2E**: join KTalk → leave → check files → rejoin → leave → check v2

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope

## Implementation Steps

### Task 1: IncrementGenerator core module
- [x] create `recog/increment.py` with `IncrementGenerator` class: `__init__(meetings_base: Path)`, `set_callback()`, `generate()`, `list_increments()`, `get_increment()`
- [x] implement `_next_version()`: scan `increments/v*_meta.json`, return `(next_version, last_segment_id)`
- [x] implement `generate()`: determine delta range, skip if 0 segments, call `CodexCliProvider().complete()`, write `vN.md` with YAML frontmatter + `vN_meta.json`, update `meta.json`
- [x] implement `_build_frontmatter()`: YAML header with meeting title, meeting_id, date, participants, version, segment_range
- [x] implement error handling: on Codex failure write `vN_meta.json` with `status: "failed"`, no `vN.md`
- [x] implement `list_increments()`: scan `increments/v*_meta.json`, return sorted by version
- [x] implement `get_increment()`: return `{meta, content}` for a specific version
- [x] write test: `test_generate_v1_creates_increment_files` — mock CodexCliProvider, verify v1.md frontmatter + v1_meta.json fields
- [x] write test: `test_generate_v2_uses_delta_segments` — pre-create v1 with segment_range [1,5], verify v2 prompt contains only segments 6+
- [x] write test: `test_skip_generation_when_zero_delta` — v1 covers all segments, verify no v2 created and CodexCliProvider not called
- [x] write test: `test_generate_handles_codex_failure` — mock exception, verify status=failed meta, no .md file
- [x] write test: `test_list_increments` — two versions exist, verify sorted list
- [x] write test: `test_get_increment` — verify returns meta + content
- [x] write test: `test_get_increment_returns_none_for_missing`
- [x] run tests: `./venv-gigaam/bin/python -m pytest tests/test_increment.py -v` — all must pass

### Task 2: Integrate IncrementGenerator into KTalkBridge
- [x] add `self._increment_generator` field in `KTalkBridge.__init__()` (after `self._on_pre_meeting_switch`, line ~303)
- [x] add `set_increment_generator(generator)` method (after `set_meeting_storage()`, line ~369)
- [x] modify `meeting_leave` handler (line ~1374): capture `prev_meeting_id` and `prev_meeting_path` BEFORE `on_ktalk_leave()`, then `asyncio.create_task(self._increment_generator.generate(...))`
- [x] modify client disconnect handler (line ~1161): same pattern — capture prev meeting before leave, trigger async generation
- [x] run existing tests: `./venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py -v` — all must pass

### Task 3: Wire IncrementGenerator in live.py startup
- [ ] find where `bridge.set_meeting_storage(storage)` is called in `recog/live.py`
- [ ] add `from recog.increment import IncrementGenerator` and create instance with `storage.base_path`
- [ ] call `bridge.set_increment_generator(increment_gen)`
- [ ] add `on_increment_ready` callback that logs to stderr
- [ ] smoke test: `./venv-gigaam/bin/python mp4_notes.py --help` — no import errors
- [ ] run all tests: `./venv-gigaam/bin/python -m pytest tests/ -v` — all must pass

### Task 4: REST API endpoints for increments
- [ ] add `GET /api/meetings/{meeting_id}/increments` endpoint in `frontend/server.py` — returns list of increment meta dicts
- [ ] add `GET /api/meetings/{meeting_id}/increments/{version}` endpoint — returns `{meta, content}` or 404
- [ ] run: `./venv-gigaam/bin/python -m compileall frontend/server.py` — no syntax errors
- [ ] run all tests: `./venv-gigaam/bin/python -m pytest tests/ -v` — all must pass

### Task 5: Frontend UI — show stored increments
- [ ] refactor `analyzeIncrement()` in `app.js`: fetch `/api/meetings/{id}/increments` first; if versions exist call `showStoredIncrements()`, else fall back to SSE generation
- [ ] implement `showStoredIncrements(meetingId, increments)`: render version tabs in modal with timestamps, load latest version content
- [ ] implement `switchIncrementVersion(meetingId, version)`: switch active tab and load content
- [ ] implement `loadIncrementContent(meetingId, version)`: fetch from REST API, strip YAML frontmatter, render markdown; handle `generating` status with polling, `failed` status with error message
- [ ] implement `regenerateIncrement(meetingId)`: trigger SSE generation (existing flow)
- [ ] extract existing SSE logic into `generateIncrementSSE(meetingId)` so both paths (fresh generate and regenerate) reuse it
- [ ] run: `./venv-gigaam/bin/python -m compileall recog frontend` — no syntax errors

### Task 6: Verify acceptance criteria
- [ ] verify: ktalk_leave triggers async increment generation (check stderr logs with `./recog-service.sh logs`)
- [ ] verify: increment files created in `meetings/<id>/increments/v1.md` + `v1_meta.json`
- [ ] verify: v1.md contains YAML frontmatter with meeting title, date, participants
- [ ] verify: reconnect + second leave creates v2 with delta segments only
- [ ] verify: zero-delta (join + immediate leave) skips generation
- [ ] verify: UI button shows stored versions with tabs
- [ ] verify: Regenerate button works via SSE fallback
- [ ] run full test suite: `./venv-gigaam/bin/python -m pytest tests/ -v`
- [ ] run linter: `./venv-gigaam/bin/python -m compileall recog frontend`

## Technical Details

### Storage layout
```
meetings/2026-03-21_143025_abc12def/
  meta.json              ← adds "increments" summary field
  transcript.jsonl
  increments/            ← NEW
    v1.md                ← YAML frontmatter + Codex output
    v1_meta.json         ← version, segment_range, status, timing
    v2.md                ← delta after reconnect
    v2_meta.json
```

### vN.md format
```markdown
---
meeting: Daily sync
meeting_id: 2026-03-21_143025_abc12def
date: "2026-03-21"
participants: [Вася, Петя]
version: 1
segment_range: [1, 41]
---

## Задачи
- ...
```

### vN_meta.json format
```json
{
  "version": 1,
  "generated_at": "2026-03-21T14:45:00+00:00",
  "provider": "codex",
  "model": "gpt-5.4",
  "reasoning_effort": "xhigh",
  "segment_range": [1, 41],
  "segment_count": 41,
  "status": "completed",
  "duration_seconds": 18.3,
  "error": null
}
```

### Delta-generation logic
- v1: full transcript (all segments)
- vN (N>1): read previous `v(N-1)_meta.json` → `start = prev.segment_range[1] + 1` → filter segments with `id > start`
- If delta = 0 segments → skip, don't create version

### Integration points in ktalk_bridge.py
- `meeting_leave` message handler (~line 1390): capture `prev_meeting_id`/`prev_meeting_path` BEFORE `on_ktalk_leave()`, then `asyncio.create_task(generator.generate(...))`
- Client disconnect (~line 1171): same pattern

## Post-Completion

**Manual verification:**
- Join KTalk meeting, talk, leave → check v1.md appears
- Rejoin same meeting, talk more, leave → check v2.md with delta only
- Open UI → click "My increment" → verify version tabs and content display
- Test Codex offline: stop codex → leave meeting → verify `status: "failed"` in meta

**Future enhancements (not in scope):**
- Structured JSON output (`v1.json`) alongside markdown
- Global index across all meetings for RAG
- Embedding storage (`vN.embedding`)
- WebSocket push for `increment_ready` (currently REST polling + log)
