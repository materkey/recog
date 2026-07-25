# Recog Copilot on webapp `#/live`: manual session spawn + chat panel

## Overview

Add a "Recog Copilot" control to the webapp live page (`http://127.0.0.1:9080/#/live`):
a button that spawns the Claude copilot session on demand and opens a chat panel
wired to the existing copilot pipeline (bridge → `recog/copilot_channel.py` MCP →
Claude session in tmux). Today the copilot chat exists only inside the Chrome
extension's floating panel, and the session is spawned only automatically per
conference meeting by `recog/copilot_supervisor.py`. This plan gives the webapp
feature parity for manual use: mic-only meetings, post-meeting questions, or any
ad-hoc session without a conference tab open.

Problem it solves: no way to talk to the copilot without a live KTalk/Jitsi/Matrix
conference tab; no manual session start at all.

## Context (from discovery)

- Chat protocol already works end-to-end for the extension:
  `copilot_chat_message {request_id, text, meeting_title, transcript_window, created_at}`
  → bridge (`ktalk_bridge.py:2626`) → `copilot_user_message` to channel clients →
  Claude replies → bridge re-emits `copilot_reply` / `copilot_status`.
- **Gap 1**: `copilot_reply`/`copilot_status` go out via `_broadcast_message`
  (`ktalk_bridge.py:1075`) which targets only `_connected_clients` (extension) —
  `_subscriber_clients` (frontend) never receive them. Inbound frames from
  subscribers ARE dispatched through `_process_message` (call site
  `ktalk_bridge.py:1786`, def `:1947`), so sending from the frontend needs no
  bridge change.
- **Gap 2**: no manual spawn path. Supervisor spawns only on `meeting_started`
  emitted by the bridge for conference sources (`_emit_lifecycle`,
  `ktalk_bridge.py:1109`). `_compute_meeting_state` (`ktalk_bridge.py:1163`)
  reports `active` only for conference meetings with a live extension client —
  a supervisor reconnect during a manual session would GRACE-kill it unless the
  bridge knows about manual sessions.
- Frontend already holds a reconnecting WS client to the bridge:
  `frontend/bridge_subscriber.py` (receive-only, filters to `transcript_refine`).
- Webapp live WS endpoint `/api/live/stream` (`frontend/server.py:1371`) discards
  everything the browser sends (`receive_text()` loop).
- Live view SPA: `frontend/static/app.js` — `views.live`, WS message switch at
  `handleMessage` (line 675), markdown renderer `renderMarkdown` (line 1869).
- Reference implementation of the chat UI behavior: extension's
  `ui-copilot-panel` with tests (`kontur-diarize/ktalk-speaker-extension/src/ui-copilot-panel.test.js`),
  outbound frame shape in `src/ws_api.js:73`.
- Existing pytest files to extend: `tests/test_ktalk_bridge.py`,
  `tests/test_copilot_supervisor.py`, `tests/test_bridge_subscriber.py`.

## Decisions

- **D1 Transport**: proxy through the frontend server. Browser talks only to
  `/api/live/stream`; `BridgeSubscriber` becomes bidirectional and relays copilot
  frames both ways. Rejected: direct browser→bridge WS (needs a new client class
  in the bridge; a page in `_connected_clients` would break last-extension-gone
  meeting-end detection; duplicates reconnect logic in JS).
- **D2 Lifecycle**: manual session lives until the user presses Stop; closing the
  tab does NOT kill it. A real conference `meeting_started(B)` preempts the manual
  session (kill + respawn under the meeting) — already the supervisor state
  machine's standard behavior for a different meeting id.
- **D3 Autospawn flag**: `RECOG_COPILOT_AUTOSPAWN=0` gates only meeting-driven
  auto-spawn. The manual button works regardless — an explicit user click. The
  early-return gate in `handle_frame` (`copilot_supervisor.py:483`) moves from
  "all frames" down to the `meeting_started`/`meeting_state` branches. The
  `on_session_died` gate (`copilot_supervisor.py:605`) is removed entirely: it
  only fires when a session should be alive, so crash-respawn must work for
  manual sessions too, independent of the flag.
- **D5 Manual-state ownership & recovery** (rewritten after arch review):
  manual-session truth has ONE runtime owner — the supervisor, which physically
  holds the tmux session (`lifecycle.meeting_id == "manual"`). `supervisor_hello`
  gains a `current_meeting_id` field (live session id or null); when the bridge
  receives a hello with `current_meeting_id == "manual"` and no conference
  meeting is active, it re-arms `_manual_copilot_active` BEFORE computing
  `meeting_state`, so the reconcile reply is `{active: true, meeting_id: "manual"}`
  and `_reconcile` adopts instead of killing. This covers both restart
  permutations (bridge-only, and bridge+frontend via `./recog-service.sh restart`)
  without persistence and without any frontend-side flag — the frontend keeps
  NO manual state. Defense-in-depth guard: the bridge REJECTS
  `copilot_spawn_request` while a conference meeting is active
  (`_is_meeting_active()` true) — not forwarded, flag untouched, reply
  `copilot_status {status: "conference_active", request_id}` to the originator —
  so no stale or misbehaving client can ever reach the supervisor as
  `meeting_started("manual")` and kill a conference-bound session (protects the
  D2 blast-radius bar "extension flow untouched"). On conference preemption of
  a manual session the bridge broadcasts an informational
  `copilot_status {status: "manual_preempted"}` to subscriber clients for UI
  display.
- **D4 v1 limitation**: proactive ticks (`_copilot_tick_state`) stay seeded only
  by conference `meeting_started` — a manual session gets manual chat only.
  Meeting context still works: the transcript buffer fills from the live pipeline
  regardless of the extension, and Claude pulls it via `get_recent_transcript`.

## New protocol frames

- `copilot_spawn_request {request_id}` — webapp → server → bridge → supervisor
  clients. `request_id` is client-generated (`crypto.randomUUID()`), echoed in
  error replies (`bridge_disconnected` / `supervisor_disconnected` /
  `conference_active`) so the UI can bind the error to the click.
- `copilot_stop_request {request_id}` — same route; supervisor treats as manual
  meeting end.
- Bridge tracks `_manual_copilot_active: bool` (set on spawn_request relay,
  cleared on stop_request relay AND on conference `meeting_started` preemption;
  re-armed from `supervisor_hello.current_meeting_id == "manual"` — D5) and,
  while set with no conference meeting active, reports
  `meeting_state {active: true, meeting_id: "manual"}` so supervisor reconcile
  adopts instead of kills. No extra `manual_active` field — `_reconcile` keys
  only off `active` + `meeting_id` (`supervisor_hello` gains
  `current_meeting_id` instead).
- Supervisor uses synthetic lifecycle id `"manual"` (constant — a flap/reconnect
  must map to the same id so GRACE revival works).

## Development Approach

- **testing approach**: TDD (tests first) — write the failing test, then the code
- test naming follows the project's given–when–then convention (CLAUDE.md;
  existing `tests/test_copilot_supervisor.py` uses given/when/then docstrings)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- maintain backward compatibility (extension flow must be untouched)

## Testing Strategy

- **unit tests**: pytest per task in the existing test files (see tasks)
- **UI (app.js)**: the frontend SPA has no JS test infrastructure (no npm setup in
  `frontend/`) — syntax is checked with `node --check`, and protocol logic stays
  server-side under pytest. Browser behavior remains a post-completion manual
  smoke; do not introduce a JS test harness in this plan.
- **e2e**: `tests/e2e` exists but covers diarization, not the webapp — out of scope

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Solution Overview

```
Браузер (#/live: кнопка + чат)
   │  WS /api/live/stream (9080): copilot_chat_message / copilot_spawn_request / copilot_stop_request
   ▼
frontend/server.py ── BridgeSubscriber (двунаправленный) ──► ktalk_bridge.py
   ▲                                                            │ spawn/stop → supervisor
   │  copilot_reply / copilot_status (incl. agent_state chip)   │ chat → copilot_channel → Claude (tmux)
   └────────────────────────────────────────────────────────────┘
```

Status chip on the button reuses the effective agent state the bridge already
computes and broadcasts as `copilot_status {status: "agent_state", agent_state}`
(🟢 online / 🟡 working / 🔴 blocked / ⚫ offline) — same semantics as the
extension chip.

## Technical Details

- `request_id`: `crypto.randomUUID()` in app.js; pending map keyed by it,
  mirroring the extension panel (one reply per request by protocol).
- `transcript_window`: last N (≤15) segments already held by `views.live`,
  `[{speaker, text, ts}]` — same shape as `ws_api.js:73`.
- Frontend server keeps a set of connected live-stream websockets (it already
  does for refine forwarding); copilot frames from the bridge are fanned out to
  all of them, browser-side filtering by `request_id` handles pending state.
- Inbound whitelist on `/api/live/stream`:
  `{copilot_chat_message, copilot_spawn_request, copilot_stop_request}` — anything
  else is dropped with a log. If the bridge link is down, the server replies to
  the originating browser socket with
  `copilot_status {status: "bridge_disconnected", request_id}` so the UI can
  show an error bound to the originating click/message.
- Bridge already answers `channel_disconnected` to the originator socket when no
  channel client is registered (`ktalk_bridge.py:2664`) — since the originator is
  the shared BridgeSubscriber socket, the server relays that status to webapp
  clients; UI treats it as "session starting" while the chip is `spawning`.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code + tests in this repo
- **Post-Completion** (no checkboxes): service restarts, manual smoke on a real meeting

## Implementation Steps

### Task 1: Bridge broadcasts copilot frames to subscriber clients

**Files:**
- Modify: `ktalk_bridge.py`
- Modify: `tests/test_ktalk_bridge.py`

- [x] write failing tests: a registered subscriber client receives `copilot_reply`
      (from `copilot_channel_reply`) and broadcast `copilot_status` variants
      (`agent_state`, `channel_connected`, and the last-channel
      `channel_disconnected`); extension clients still receive shared-audience
      frames, while per-request rejection echoes stay originator-only
- [x] add a `_broadcast_copilot_frame` helper (fan-out to `_connected_clients` +
      `_subscriber_clients`, snapshot under `self._lock` as `send_refine` does,
      `ktalk_bridge.py:1009`) and route shared-audience Copilot broadcast sites
      through it: `copilot_reply` ~`ktalk_bridge.py:2780`, agent-state chip
      ~`:1264`, `channel_connected` ~`:2619`, `channel_disconnected` on last
      channel-client departure ~`:1847` (a hand-patched subset would leave the
      webapp blind to channel-daemon death mid-session)
- [x] verify no other `_broadcast_message` call sites leak unrelated frames to
      subscribers (only copilot frames get the wider fan-out)
- [x] run tests - must pass before task 2

### Task 2: Bridge relays manual spawn/stop and tracks manual session state

**Files:**
- Modify: `ktalk_bridge.py`
- Modify: `tests/test_ktalk_bridge.py`

- [x] write failing tests: `copilot_spawn_request` from a subscriber socket is
      forwarded to supervisor clients and sets the manual flag;
      `copilot_spawn_request` while a conference meeting is active
      (`_is_meeting_active()` true) is NOT forwarded, does NOT set the flag,
      and replies `copilot_status {status: "conference_active", request_id}`
      to the originator; `copilot_stop_request` forwards and clears the flag;
      `copilot_stop_request` with no supervisor connected → `copilot_status
      {status: "supervisor_disconnected", request_id}` (flag still cleared —
      there is no session to keep alive for); `_compute_meeting_state` reports
      `active=True, meeting_id="manual"` while the flag is set and no
      conference meeting is active; conference `meeting_started` clears the
      flag AND broadcasts `copilot_status {status: "manual_preempted"}` to
      subscriber clients; `supervisor_hello` with
      `current_meeting_id == "manual"` and no conference meeting active re-arms
      the flag before `meeting_state` is computed (reply `active=True` — D5);
      spawn_request with no supervisor connected → `copilot_status
      {status: "supervisor_disconnected", request_id}` to the originator socket
- [x] add `_manual_copilot_active` state + handlers for the two new frame types in
      `_process_message` (forward to `_supervisor_clients` like `_emit_lifecycle`
      does, `ktalk_bridge.py:1122`); reject spawn with `conference_active` while
      `_is_meeting_active()` is true — never forward it when the conference is
      already active; if it starts mid-relay, do not arm the manual flag and
      return `conference_active` to the originator
- [x] extend `_compute_meeting_state` (`ktalk_bridge.py:1163`) with the manual
      branch (conference meeting takes precedence over manual); re-arm the flag
      from `supervisor_hello.current_meeting_id` in the `supervisor_hello`
      handler BEFORE `_compute_meeting_state` runs (D5)
- [x] clear the manual flag inside the conference `meeting_started` emit path
      (preemption) and on `copilot_stop_request`; on preemption broadcast
      `copilot_status {status: "manual_preempted"}` to subscriber clients
- [x] extend the sessions-zero → offline merge eligibility to manual sessions:
      add helper `_copilot_should_be_alive() -> bool` returning
      `_is_meeting_active() or _manual_copilot_active`, use it in
      `_update_sessions_zero_anchor` (`ktalk_bridge.py:1216`) and
      `_effective_agent_state` (`ktalk_bridge.py:1244`) — do NOT change
      `_is_meeting_active` itself (used elsewhere, e.g. `_compute_meeting_state`).
      Test: manual flag set, supervisor `online`, `claude_sessions {count: 0}`
      past the debounce → effective state `offline` (chip must not show 🟢 for
      a manual copilot whose Claude session never registered with
      copilot_channel)
- [x] manual chat must not inherit conference staleness semantics: in
      `copilot_chat_message` (`ktalk_bridge.py:2650`), capture `meeting_id` as
      None when `_is_meeting_active()` is False (mic-only/orphan context) — the
      pending entry then records `""` (existing no-meeting escape hatch,
      `ktalk_bridge.py:2711-2714`) and the reply-side staleness checks
      (`:2745-2776`) cannot drop a manual reply when `_check_silence_split`
      rotates the orphan meeting mid-flight. Conference requests unaffected.
      Test: chat sent with no conference active, current meeting rotated before
      the reply arrives → reply is still broadcast (not dropped as stale)
- [x] run tests - must pass before task 3

### Task 3: Supervisor handles manual spawn/stop; autospawn gate narrowed

**Files:**
- Modify: `recog/copilot_supervisor.py`
- Modify: `tests/test_copilot_supervisor.py`

- [x] write failing tests: `copilot_spawn_request` → `lifecycle.meeting_started("manual")`
      → Spawn; repeated `copilot_spawn_request` with a live manual session is a
      no-op flap (idempotent same-id spawn — double-click safe);
      `copilot_stop_request` → `meeting_ended` →
      GRACE → timer → Kill; conference `meeting_started(B)` during a manual
      session → Kill + Spawn(B); `meeting_state {active: true, meeting_id: "manual"}`
      on reconnect adopts the live session (no kill); with `autospawn=False`
      manual frames still spawn but `meeting_started`/`meeting_state` remain
      inert; `session_died` respawns a manual session even with `autospawn=False`
- [x] update the existing `test_autospawn_off_is_noop`
      (`tests/test_copilot_supervisor.py:219`) — it asserts ALL frames are no-ops
      under `autospawn=False`, which D3 changes; split it into
      meeting-frames-are-inert vs manual-frames-still-work
- [x] handle the two new frame types in `handle_frame` (`copilot_supervisor.py:480`)
- [x] move the `if not self.autospawn: return` gate from the top of `handle_frame`
      into the `meeting_started` / `meeting_state` / `meeting_ended` branches (D3);
      manual stop must also work when autospawn is off
- [x] remove the `autospawn` early-return from `on_session_died`
      (`copilot_supervisor.py:605`) so a crashed manual session respawns (D3)
- [x] include `current_meeting_id` (the id of a session the supervisor intends
      to keep alive, or null in GRACE/IDLE) in `supervisor_hello` (D5); test:
      supervisor reconnect during a live manual session → bridge re-arms its
      flag → `meeting_state {active: true, meeting_id: "manual"}` → adopt, no
      kill (covers a bridge-only restart wiping the bridge mid-manual-session)
- [x] run tests - must pass before task 4

### Task 4: BridgeSubscriber becomes bidirectional

**Files:**
- Modify: `frontend/bridge_subscriber.py`
- Modify: `tests/test_bridge_subscriber.py`

- [x] write failing tests: forwards `copilot_reply` and `copilot_status` (in
      addition to `transcript_refine`), still drops other types; `send(raw)`
      delivers to the connected bridge socket; `send` while disconnected returns
      False (or raises a typed error) without crashing the run loop
- [x] replace the single-type filter (`bridge_subscriber.py:167`) with a
      `FORWARD_TYPES` frozenset `{transcript_refine, copilot_reply, copilot_status}`
- [x] hold the current ws in an attribute; add `async def send(self, raw: str) -> bool`
      (best-effort: no queueing of stale chat messages across reconnects — a chat
      message typed while the bridge is down fails fast)
- [x] run tests - must pass before task 5

### Task 5: Frontend server — inbound copilot frames over `/api/live/stream`

**Files:**
- Modify: `frontend/server.py`
- Create: `tests/test_frontend_copilot_proxy.py`

- [x] write failing tests (pure helpers + fake subscriber): inbound whitelist
      accepts the three copilot types and rejects others; accepted frames are
      passed to `bridge_subscriber.send`; send failure (bridge link down)
      produces a `copilot_status {status: "bridge_disconnected", request_id}`
      reply to the originating client (echoing the frame's `request_id`, incl.
      for spawn/stop); last `agent_state` is cached and delivered to a newly
      connected live-stream client. The server holds NO manual-session state —
      it is a pure relay (D5: recovery is supervisor_hello-driven, bridge-side)
- [x] extract inbound handling into a testable helper
      `handle_client_frame(raw, subscriber, reply_cb)` and call it from the
      `/api/live/stream` receive loop (`frontend/server.py:1420`)
- [x] note: NO `_forward_bridge_message` change needed — it already broadcasts
      every forwarded frame unconditionally (`frontend/server.py:453`); widening
      `FORWARD_TYPES` in Task 4 is sufficient for bridge→webapp fan-out
- [x] agent-state primer: cache the last `copilot_status {status: "agent_state"}`
      seen from the bridge and send it right after `init` on each new
      `/api/live/stream` connection (a browser opening `#/live` mid-session must
      not show ⚫ until the next state change)
- [x] run tests + `python -m compileall recog frontend` - must pass before task 6

### Task 6: Webapp UI — Copilot button, status chip, chat panel

**Files:**
- Modify: `frontend/static/app.js`
- Modify: `frontend/static/index.html`
- Modify: `frontend/static/style.css` (or the stylesheet used by the live view)

- [x] add a "Recog Copilot" button with status chip to the live view; chip driven
      by `copilot_status {status: "agent_state"}` frames (⚫ offline default)
- [x] click on ⚫/idle → send `copilot_spawn_request {request_id}` and open the
      chat panel; Stop control sends `copilot_stop_request {request_id}` and
      shows "останавливается…" — the session dies only after the supervisor's
      90 s GRACE, so the UI must not claim "stopped" until the chip goes ⚫; a
      failed stop (`bridge_disconnected`/`supervisor_disconnected` echo) shows
      the error with a retry affordance; while `spawning` show "запускается…"
      and disable the input
- [x] chat panel: message list, input, per-`request_id` pending state, statuses
      (`queued/working/error/done/channel_disconnected/bridge_disconnected/
      supervisor_disconnected/conference_active/manual_preempted` — the last
      two render as info lines: "занято конференцией" / "сессия переключена на
      конференцию"), assistant replies rendered via existing
      `renderMarkdown` (`app.js:1869`) — behavior mirrors the extension panel
      (`ui-copilot-panel.test.js` is the behavioral spec)
- [x] build `transcript_window` from the last ≤15 segments in `views.live` state,
      shape as `ws_api.js:73`; include `meeting_title` when a meeting is active
- [x] new `case` branches in `handleMessage` (`app.js:675`) for `copilot_reply` /
      `copilot_status`; unknown `request_id` replies (e.g. authored from the
      extension) are appended, not dropped
- [x] verify: `python -m compileall recog frontend` and
      `node --check frontend/static/app.js` pass; full pytest suite passes

### Task 7: Verify acceptance criteria

- [x] all Overview requirements implemented (manual spawn, chat, Stop, chip)
- [x] automated edge cases handled: no supervisor / no channel client / bridge down;
      conference preemption of a manual session; `copilot_spawn_request` during
      an active conference is rejected (`conference_active`) and can never kill
      the conference-bound session; supervisor reconnect adopts a manual
      session; a bridge-only restart while the supervisor stays alive re-arms via
      `supervisor_hello.current_meeting_id`; F5 of the tab does not kill the
      session; manual chat reply survives an orphan-meeting silence-split
      rotation mid-flight
- [x] run full test suite: `python -m pytest tests/ -v`
- [x] `python -m compileall recog frontend`
- [x] `node --check frontend/static/app.js`
- ⚠️ smoke (see Post-Completion) — post-completion manual step, NOT attempted here: launchd restart, real-meeting preemption check, and restart-resilience check remain for a human

### Task 8: [Final] Update documentation

- [x] update `CLAUDE.md`: extend the copilot supervisor section with manual
      spawn frames + webapp chat, note D3/D4 semantics
- [x] update `openspec/specs/ktalk-copilot-chat/spec.md` if the team keeps specs
      in sync (new frames + subscriber fan-out)
- [x] move this plan to `docs/plans/completed/`

## Post-Completion

**Manual smoke** (requires launchd services):
- `./recog-service.sh restart` + `./recog-service.sh restart-supervisor`
- open `http://127.0.0.1:9080/#/live`, press "Recog Copilot", wait for 🟢
  (~6–10 s cold start), send a question, receive an answer; press Stop, verify
  the tmux session (`tmux -L recog-copilot ls`) is gone after grace
- with a real conference: start a KTalk/Jitsi meeting during a manual session,
  verify preemption (session respawns bound to the meeting), verify the webapp
  shows "сессия переключена на конференцию", and that the extension panel still
  works unchanged
- restart resilience (known gap): `./recog-service.sh restart` also restarts the
  supervisor, so its fresh lifecycle cannot report the surviving manual binding
  and the tmux session enters GRACE. Re-run this acceptance case after the
  deferred lifecycle-id recovery is implemented.

**Known v1 limitations** (documented, not tasks):
- no proactive ticks in manual sessions (D4)
- one copilot per machine (inherited from the supervisor design)
- replies broadcast to both the extension and webapp when both are open —
  pending state is per-author via `request_id`, duplicates are cosmetic
- `AUTOSPAWN=0` + conference preemption desync: the bridge clears
  `_manual_copilot_active` on conference `meeting_started`, but a supervisor
  with autospawn off ignores meeting frames — the `"manual"` tmux session
  lingers untracked by the bridge and is only killable via explicit Stop
