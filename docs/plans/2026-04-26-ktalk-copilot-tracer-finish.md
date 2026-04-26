# KTalk Copilot Tracer — Finish

## Overview
- Continue the `ktalk-claude-copilot-chat` tracer bullet by adding the KTalk DOM Copilot UI, transcript snapshot tool, proactive transcript ticks, meeting lifecycle events, and finishing the test pass. Ends with all `tasks.md` items in `openspec/changes/ktalk-claude-copilot-chat/` either checked off or explicitly marked as deferred-to-human-smoke.

## Context (from current branch)
- Branch: `ktalk-copilot-channel-scaffold` (forked off `main`).
- Backend scaffold landed in commits `213da74..799bb47` on this branch:
  - `recog/copilot_channel.py` — MCP Streamable HTTP server + bridge WS client + `send_copilot_message`/`set_copilot_status` tools, with `request_id` auto-generation when caller omits it.
  - `ktalk_bridge.py` — `_copilot_channel_clients` set, frame handlers for `copilot_channel_hello` / `copilot_chat_message` / `copilot_channel_reply` / `copilot_channel_status`, `channel_disconnected` status fallback, meeting-end-recovery race fix.
  - `requirements.txt` — `mcp` SDK added.
  - `tests/test_ktalk_bridge.py` — channel-scaffold tests.
- Spec: `openspec/changes/ktalk-claude-copilot-chat/{proposal,design,tasks,specs/...}.md` (committed on `main` as `c976216`).
- Extension files (read-only context for this plan):
  - `kontur-diarize/ktalk-speaker-extension/src/ui.js:773` — `addTranscriptTab()`; `:805` — `addContextTab()`. New `addCopilotTab()` mirrors these.
  - `src/ws.js:72-88` — incoming-frame dispatcher; `copilot_reply` / `copilot_status` / `meeting_started` / `meeting_ended` need new branches.
  - `src/state.js:15` — shared `state` object; copilot fields go here.
  - `src/ws_api.js` — outbound frame helpers; new `sendCopilotChatMessage()` goes here.
- Test model: `tests/test_ktalk_bridge.py` already extended with copilot scaffold tests; reuse `pytest-asyncio` + `KTalkBridge()` direct-call pattern for new lifecycle / transcript-buffer tests.

## Development Approach
- **Testing approach**: Regular — write a unit test alongside each new bridge handler.
- Order: UI first (closes the manual-chat loop end-to-end), then lifecycle, then transcript snapshot, then proactive ticks, then close-out (tasks.md updates + tests).
- Smoke-only items in `tasks.md` (1.1, 4.6, 5.6, 6.6, 8.9) cannot be automated; mark them as deferred-to-human in the plan close-out instead of checking them off.
- Skip `npm test` if `package.json` has no `test` script — verify before running.
- Keep edits minimal per the project's editing principles. No drive-by refactors.

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: KTalk DOM Copilot UI scaffold (`tasks.md` 3.1–3.6)
- [x] In `kontur-diarize/ktalk-speaker-extension/src/state.js`, extend the exported `state` object with: `copilotMessages: []`, `copilotPending: new Map()`, `copilotProactiveMutedUntil: 0`, `copilotChannelStatus: "unknown"`, `copilotLastProactiveAt: 0`. Match existing field-style and indentation.
- [x] In `kontur-diarize/ktalk-speaker-extension/src/ui.js`, add `addCopilotTab()` modelled on `addTranscriptTab()` (line 773) and `addContextTab()` (line 805). Reuse existing tab CSS/classes; do not introduce a CSS framework. Wire the tab insertion into the same flow that calls `addTranscriptTab` / `addContextTab`.
- [x] Inside the Copilot panel: scrollable message list `<div>`, a multi-line input (`<textarea>` or `<input>`), a send button. Enter (without Shift) sends; Shift+Enter inserts a newline. Disable input while a request is pending.
- [x] Add a `renderCopilotMessage(msg)` helper supporting `kind ∈ {"user","assistant","proactive","status","error"}` with distinct CSS classes (`copilot-msg-user`, `copilot-msg-assistant`, `copilot-msg-proactive`, `copilot-msg-status`, `copilot-msg-error`). Define minimal CSS where missing.
- [x] Per-message actions for `assistant` and `proactive`: `copy` button (writes text via `navigator.clipboard.writeText`), `dismiss` button (filters the entry out of `state.copilotMessages` and re-renders). Add a global "mute proactive 10m" toggle in the panel header that sets `state.copilotProactiveMutedUntil = Date.now() + 10 * 60 * 1000`.
- [x] In `kontur-diarize/ktalk-speaker-extension/src/ws_api.js`, add `sendCopilotChatMessage({ text, meetingId, meetingTitle, transcriptWindow })`. Generates `request_id = "copilot_" + crypto.randomUUID()`, sends `{"type":"copilot_chat_message", request_id, text, meeting_id, meeting_title, transcript_window, created_at: Date.now()}`. Returns `request_id`.
- [x] Wire input submission through `sendCopilotChatMessage`: append a user message + a pending assistant placeholder (`status="working"`) to `state.copilotMessages` keyed on `request_id`; record in `state.copilotPending`.
- [x] In `kontur-diarize/ktalk-speaker-extension/src/ws.js` `onmessage` dispatcher (lines 72-88), add branches for:
  - `copilot_reply` — resolve the pending entry by `request_id`, replace placeholder with the assistant message; if `kind === "proactive"`, run through the proactive gate (Task 4).
  - `copilot_status` — update the matching pending entry's status; if `status === "channel_disconnected"`, render an error message and clear the pending state.
- [x] Run `npm run build` in `kontur-diarize/ktalk-speaker-extension` — verify it succeeds and `injected.js` is updated. (Covers Task 8.8.)

### Task 2: Meeting lifecycle events from bridge to channel (`tasks.md` 7.1, 7.2, 7.3, 7.5)
- [x] In `ktalk_bridge.py`, find the existing `meeting_join` and `meeting_leave` branches in `_process_message`. After the bridge's existing meeting-state update, also broadcast a frame to every websocket in `self._copilot_channel_clients`:
  - join: `{"type": "meeting_started", "meeting_id", "meeting_title", "created_at"}`
  - leave: `{"type": "meeting_ended", "meeting_id", "created_at"}`
- [x] Add `self._copilot_pending_requests: dict[str, str]` in `__init__` (request_id → meeting_id). When `_process_message` forwards a `copilot_chat_message` (existing handler from previous ralphex), record the mapping. On `meeting_leave`, mark all entries for the leaving meeting as stale (e.g. prefix the value with `"stale:"`).
- [x] When a `copilot_channel_reply` or `copilot_channel_status` arrives for a stale `request_id`, log at INFO and drop instead of broadcasting to extension clients.
- [x] In `kontur-diarize/ktalk-speaker-extension/src/ws.js`, add branches for `meeting_started` / `meeting_ended` — push a system-style entry (`kind="status"`) into `state.copilotMessages` (e.g. "встреча началась" / "встреча закончилась").
- [x] Add unit tests in `tests/test_ktalk_bridge.py`: `meeting_started`/`meeting_ended` frames sent to channel clients; stale reply dropped after `meeting_leave`; non-stale reply still relayed.

### Task 3: Recent transcript buffer + `get_recent_transcript` MCP tool (`tasks.md` 5.1–5.5)
- [x] In `ktalk_bridge.py` `__init__`, add `self._copilot_transcript_buffer: dict[int, dict] = {}` plus `self._copilot_transcript_order: collections.deque = collections.deque(maxlen=300)` (segment_id order). Each entry: `{"segment_id", "speaker", "text", "start_ms", "end_ms", "refined": False, "meeting_id"}`.
- [x] Find where the bridge currently emits `{"type": "transcript", ...}` (search `transcript` broadcast site). After the broadcast, append/replace the buffer entry by `segment_id` (drop oldest if deque is full — also `del self._copilot_transcript_buffer[old_id]`).
- [x] On `transcript_refine`, find the entry by `segment_id` and update `text` + `refined=True`. If the segment is not in the buffer, ignore silently.
- [x] Add `_process_message` branch for `msg_type == "get_recent_transcript_request"` from a channel client. Args: `request_id`, `window_seconds?` (default 180), `since_segment_id?`, `include_refined?` (default True). Reply with `{"type": "get_recent_transcript_response", "request_id", "segments": [...]}` filtered to the current `meeting_id` only.
- [x] In `recog/copilot_channel.py`, add an MCP tool `get_recent_transcript(window_seconds: int = 180, since_segment_id: int | None = None, include_refined: bool = True) -> list[dict]`. Implementation: send `get_recent_transcript_request` over the bridge WS connection with a generated `request_id`, await the matching `get_recent_transcript_response` via an `asyncio.Future` correlator dictionary keyed by `request_id`. Add a 5-second timeout to avoid hanging Claude.
- [x] Add unit tests in `tests/test_ktalk_bridge.py`: append, refine update by segment_id, snapshot returns segments within window, snapshot filters by `meeting_id`.

### Task 4: Proactive transcript ticks (`tasks.md` 6.1, 6.2, 6.4, 6.5, 7.4)
- [x] In `ktalk_bridge.py`, add `self._copilot_tick_state: dict | None = None`. On `meeting_join`, initialize `{"meeting_id", "last_seg_count": 0, "last_emit_ts": 0.0}`. On `meeting_leave`, set to `None` and cancel any scheduled task.
- [x] After every `transcript` broadcast, if `_copilot_tick_state is not None` AND `_copilot_channel_clients` is non-empty, check the trigger: at least 5 new segments since last emit OR at least 15 seconds elapsed. If triggered, broadcast `{"type": "transcript_tick", "meeting_id", "meeting_title", "new_segments": [...], "recent_context": "<last ~10 segments as text>", "local_user_name": <MIC_SPEAKER_NAME>}` to channel clients. Update `last_seg_count` / `last_emit_ts`.
- [x] In `kontur-diarize/ktalk-speaker-extension/src/ws.js`, when a `copilot_reply` has `kind === "proactive"`:
  - if `state.copilotProactiveMutedUntil > Date.now()`, drop silently.
  - otherwise enforce a 60 s UI cooldown: if `Date.now() - state.copilotLastProactiveAt < 60_000`, drop the new proactive (do not queue). Otherwise render via `renderCopilotMessage` with `kind="proactive"` and update `copilotLastProactiveAt`.
- [x] Make sure proactive messages render with `copilot-msg-proactive` CSS class (already added in Task 1) so they stand out visually.
- [x] Add unit tests: tick fires after N new segments; tick fires on time-based trigger; tick suppressed without active meeting; tick suppressed without channel clients.

### Task 5: Channel-server smoke (`tasks.md` 8.5)
- [x] Add `tests/test_copilot_channel.py`: spin up an in-memory fake bridge WS server (using `websockets.serve` on a random port), boot the channel-server module against it (call its `main` coroutine with overridden host/port args), and assert:
  - on connect, the server sends `copilot_channel_hello`.
  - calling the `send_copilot_message` tool produces a `copilot_channel_reply` frame on the bridge side with the correct fields.
  - calling the `get_recent_transcript` tool sends `get_recent_transcript_request` and resolves on `get_recent_transcript_response`.
- [x] Use `pytest-asyncio`. Do NOT spawn a real subprocess. If the channel server's MCP HTTP loop interferes, gate it behind a flag and only run the WS-client loop in the test.

### Task 6: Update `tasks.md` and run automated test sweep (`tasks.md` 8.6, 8.7, 8.8)
- [ ] Edit `openspec/changes/ktalk-claude-copilot-chat/tasks.md`: change `- [ ]` to `- [x]` for every task whose work is complete on this branch. Specifically check off:
  - 1.2, 1.3, 1.4 (1.1 is a human spike — leave unchecked, append `# requires human smoke`).
  - 2.1, 2.2, 2.3, 2.4, 2.5.
  - 3.1, 3.2, 3.3, 3.4, 3.5, 3.6.
  - 4.1, 4.2, 4.3, 4.4, 4.5 (4.6 is a manual smoke — leave unchecked).
  - 5.1, 5.2, 5.3, 5.4, 5.5 (5.6 is a manual smoke).
  - 6.1, 6.2, 6.3 (channel-server prompt instructions can be a one-liner doc — see below), 6.4, 6.5 (6.6 is a manual smoke).
  - 7.1, 7.2, 7.3, 7.4, 7.5.
  - 8.1, 8.2, 8.3, 8.4 (extension tests — only if a JS test runner is present), 8.5, 8.6, 8.8.
- [ ] For task 6.3 (channel instructions), add a short `## System prompt for Claude` section to `recog/copilot_channel.py` module docstring (or a sibling `.md`) describing: default to silence, post only on direct questions to the local user, missing owner/deadline, technical risks, etc. This is what Claude Code's MCP server description carries.
- [ ] Run `./venv-gigaam/bin/python -m pytest tests/ -v` — must pass.
- [ ] Run `./venv-gigaam/bin/python -m compileall recog frontend`.
- [ ] In `kontur-diarize/ktalk-speaker-extension`: check `package.json` for a `test` script. If present, run `npm test`. If absent, skip and note "Task 8.7 deferred — no JS test runner configured" in plan progress.
- [ ] Run `npm run build` in `kontur-diarize/ktalk-speaker-extension` — must succeed.

### Task 7: Commit and push
- [ ] `git status` — confirm only intended files changed.
- [ ] Atomic commits (collapse where it would split a single coherent change):
  - `feat(extension): add KTalk DOM Copilot tab and chat UI`
  - `feat(extension): wire copilot_reply, copilot_status, and meeting lifecycle frames`
  - `feat(bridge): emit meeting_started/meeting_ended to channel clients`
  - `feat(bridge): drop late copilot replies for stale meetings`
  - `feat(bridge): recent transcript buffer keyed by segment_id`
  - `feat(copilot_channel): add get_recent_transcript MCP tool`
  - `feat(bridge): emit transcript_tick events while a meeting is active`
  - `feat(extension): render proactive copilot messages with cooldown and mute`
  - `test(copilot): scripted channel-server smoke and bridge transcript-buffer tests`
  - `docs(openspec): mark ktalk-copilot-chat tasks complete`
- [ ] Push to current branch `ktalk-copilot-channel-scaffold`. Do not force-push. If push is rejected, surface the error and stop.

## Post-Completion
- Manual smoke (cannot be automated by ralphex):
  - 1.1: verify Claude Code Channels availability with the official fakechat reference flow.
  - 4.6: KTalk meeting → manual Copilot input → bridge → Claude reply → DOM render.
  - 5.6: slow Claude lookup; verify it can re-call `get_recent_transcript` and pull the latest segments before answering.
  - 6.6: scripted transcript with a question to the local user; verify a proactive Copilot message appears and `mute proactive 10m` suppresses subsequent ones.
  - 8.9: end-to-end live smoke covering all of the above.
- Once smokes pass, file a PR from `ktalk-copilot-channel-scaffold` → `main` referencing the OpenSpec change.
- Follow-up plan candidates (if needed): tighten anti-noise heuristics on `transcript_tick`, persist Copilot history into `meetings/<id>/`, Side Panel UI, security/egress hardening.
