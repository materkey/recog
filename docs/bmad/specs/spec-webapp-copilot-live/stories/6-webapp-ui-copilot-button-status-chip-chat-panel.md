---
title: 'Webapp UI — Copilot button, status chip, chat panel'
type: 'feature'
created: '2026-07-25'
status: 'done'
baseline_revision: 'BASE_REVISION'
final_revision: '9ddc533'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/docs/plans/20260724-webapp-copilot-live.md'
  - '{project-root}/_bmad-output/specs/spec-webapp-copilot-live/SPEC.md'
warnings: [oversized]
---

<intent-contract>

## Intent

**Problem:** The whole copilot pipeline now reaches the webapp (stories 1–5: bridge fan-out, manual spawn/stop relay, supervisor lifecycle, bidirectional subscriber, server proxy + `agent_state` primer), but `#/live` renders none of it: `handleMessage` (`recog/web/static/app.js:675`) silently drops `copilot_reply` / `copilot_status`, and the browser never sends a copilot frame.

**Approach:** CAP-6 — add a "Recog Copilot" button with an agent-state chip and a floating chat panel to the live view (`recog/web/static/` only: `app.js`, `index.html`, `style.css`). Behavior mirrors the extension panel (`browser-extension/src/ui-copilot-panel.test.js` is the behavioral spec; implementation reference `src/ui.js:1332-1997`), with the plan's webapp-specific deltas: spawn/stop controls, `renderMarkdown` replies, ≤15-segment `transcript_window`, and renderings for `bridge_disconnected` / `supervisor_disconnected` / `conference_active` / `manual_preempted`.

## Boundaries & Constraints

**Always:**
- Copilot chat state (message list, pending map, chip state, panel-open flag) lives at module level, NOT inside the state that `views.live.disconnect()` clears — chat history must survive view switches within one page load (the session itself survives tab close per D2). F5 loses history; that matches the SPA's persistence model.
- All frames go over the existing `views.live.ws`; guard every send on `ws && ws.readyState === WebSocket.OPEN`. A send attempted while the WS is not OPEN appends an error bubble ("Bridge offline: сообщение не отправлено.") without clearing the input and without creating a pending entry (extension `submitCopilotInput` behavior).
- `request_id` = `'copilot_' + crypto.randomUUID()`; outbound chat frame shape exactly as `ws_api.js:63-82`: `{type: 'copilot_chat_message', request_id, text, meeting_id, meeting_title, transcript_window, created_at: Date.now()}`.
- `transcript_window`: last ≤15 rows of `views.live.segments`, event rows (`event_type` present) excluded, each mapped to `{speaker: s.speaker || '', text: s.text || '', timestamp: s.ts_display || '', segment_id: s.id || 0}` (the actual `ws_api.js:73` wire shape — `timestamp`/`segment_id`, not the plan prose's `ts`). `meeting_id` = `currentMeeting?.id || null`, `meeting_title` = `currentMeeting?.title || null`.
- Assistant/proactive reply text renders through the existing `renderMarkdown` (`app.js:1869`, escape-first — safe for `innerHTML`); user/error/info bubbles stay plain-escaped text.
- Replies resolve their pending placeholder **in place** (no reorder); message insertion is append-only with scroll-to-bottom.
- Per-request `copilot_status` frames never touch the agent chip; `agent_state` frames never touch per-request placeholders; unknown `agent_state` values leave the chip unchanged (extension guard).
- Existing live-view behavior stays byte-identical: transcript rendering, refine handling, anchor overlay (incl. its Escape/Alt+J bindings), follow mode, copy/download. The copilot panel closes only via its own controls — do not extend the global Escape handler.
- Style additions follow `style.css` conventions: flat kebab-case classes, `:root` tokens, reuse `.ai-answer` typography inside assistant bubbles and the `.live-status`/`.status-dot` chip grammar.

**Block If:**
- The UI turns out to require any change outside `recog/web/static/` (a server/bridge/extension edit would violate the story boundary — stories 1–5 claim those surfaces are complete).

**Never:**
- No JS test harness, no npm/bundler setup in `recog/web/` (SPEC non-goal); no server-side (`recog/web/server.py`), bridge, supervisor, or extension changes.
- No proactive mute button / 60 s cooldown machinery in the webapp panel (extension-only `copilot.test.js` surface; not in the named behavioral spec) — proactive replies simply render with the "Claude · proactive" label.
- No queueing of frames while the WS is down; no reconnect logic changes.
- No persistence of chat history or manual-session state in the browser (D5: the frontend holds no manual-session truth — button/panel state is derived from `agent_state` frames and per-click echoes only).

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Chip lifecycle | `copilot_status {status:"agent_state", agent_state: X}` for online/working/spawning/blocked/offline | Chip renders 🟢 online / 🟡 working / 🟡 spawning / 🔴 blocked / ⚫ offline (default ⚫) | Unknown value: chip unchanged |
| Spawn click | Chip ⚫; click "Recog Copilot" | Panel opens; exactly one `copilot_spawn_request {request_id}` sent; spawn-pending → "запускается…", input disabled | WS not OPEN → error info line, no frame |
| Spawn resolves | Spawn-pending; chip leaves `offline` (spawning/online) | Spawn-pending cleared; input enabled once chip ≠ spawning | No error expected |
| Spawn rejected | Spawn-pending; `copilot_status {status:"conference_active", request_id}` echo | Info line "занято конференцией"; spawn-pending cleared, button back to idle | No error expected |
| Spawn echo error | Spawn-pending; `bridge_disconnected`/`supervisor_disconnected` echo with that request_id | Error info line; spawn-pending cleared, button clickable again (retry) | No error expected |
| Open while live | Chip not ⚫; click button | Panel toggles open/closed; NO spawn frame sent | No error expected |
| Stop click | Panel open, session live; click Stop | One `copilot_stop_request {request_id}`; stop-pending → "останавливается…" persists until chip goes ⚫ (only then considered stopped) | No error expected |
| Stop echo error | Stop-pending; `bridge_disconnected`/`supervisor_disconnected` echo | Error info line; stop-pending cleared; Stop stays enabled (retry affordance) | No error expected |
| Chat send | Text entered; Enter or Send; WS OPEN | User bubble + assistant placeholder "Жду ответа Claude…" (pending, same request_id); frame sent; input cleared | No error expected |
| Reply resolves | Pending request_id; `copilot_reply {request_id, text}` | Placeholder mutated in place to markdown-rendered assistant bubble; pending entry removed | No error expected |
| Unknown reply | `copilot_reply` with unmatched request_id (e.g. extension-authored) | Fresh assistant bubble appended — never dropped | No error expected |
| Working status | Pending id; `copilot_status {status:"working", request_id}` | Placeholder text → "Claude думает…" (still pending) | No error expected |
| Other status | Pending id; `queued` or unknown per-request status | Placeholder text → `Status: <status>`; no bubble created when no pending match | No error expected |
| Terminal done | Pending id; `copilot_status {status:"done", request_id}` | Placeholder → info/status kind, `data.message \|\| "Запрос завершён без ответа."`; pending removed (no-op if reply already landed) | No error expected |
| Terminal error | Pending id; `copilot_status {status:"error", request_id}` | Placeholder → error kind, `data.message \|\| "Claude не смог ответить."`; pending removed | Without pending match: standalone error bubble |
| Channel down | `channel_disconnected` with matching id / no id but pendings exist / no pendings | Matching placeholder → error "Channel offline: Claude не подключён." / ALL pendings flipped + cleared / one standalone error bubble | No error expected |
| Bridge down chat | Chat pending; `bridge_disconnected` echo with its request_id | That placeholder → error kind; pending removed | No error expected |
| Preemption | Broadcast `copilot_status {status:"manual_preempted"}` (no matching pending) | Info line "сессия переключена на конференцию" in the panel | No error expected |
| Unread | Reply/proactive arrives while panel closed | Unread dot on the "Recog Copilot" button; opening the panel clears it | No error expected |
| View switch | Navigate away from `#/live` and back | Chat history + chip survive (module state); WS reconnects; server re-primes chip after `init` | No error expected |

</intent-contract>

## Code Map

- `recog/web/static/app.js:610-673` -- `views.live` (state, `connect`/`disconnect`); `:675` `handleMessage` switch — add `copilot_reply`/`copilot_status` cases; `:1869` `renderMarkdown`; `:1071` `escapeHtml`; `:1955` `isTextInputActive` hotkey guard; `:1988-2130` anchor overlay (in-flight/disable pattern to mirror; do not touch)
- `recog/web/static/index.html:82-97` -- `#view-live` markup: button + chip slot into `.view-header` (`:85-89`); panel markup as a sibling after `#anchor-overlay` (`:147`), mirroring the overlay pattern
- `recog/web/static/style.css` -- tokens `:7-36`; chip grammar `.live-status`/`.status-dot` `:824-865`; panel references `.anchor-card` `:1247`, `.ai-answer*` typography `:1159-1211`, `.btn`/`.btn-danger` `:135`/`:1598`
- `browser-extension/src/ui-copilot-panel.test.js` -- behavioral spec (pending map, in-place resolve, unknown-id append, chip mapping, unread dot)
- `browser-extension/src/ui.js:1332-1997` -- reference implementation: state model, `submitCopilotInput` `:1708`, `handleCopilotReply` `:1778`, `handleCopilotStatus` `:1827`, chip map `:1484-1500`, channel-status branches `:1858-1903`
- `browser-extension/src/ws_api.js:63-82` -- outbound frame shape + request-id generation to replicate
- `recog/web/server.py:464-552` -- (read-only) inbound whitelist, `bridge_disconnected` echo, `agent_state` primer after `init` — what the browser can rely on

## Tasks & Acceptance

**Execution:**
- `recog/web/static/index.html` -- add the "Recog Copilot" button + chip span to the live `.view-header`, and the copilot panel skeleton (header with chip/Stop/close, messages region, textarea + Send) as a hidden top-level element -- static DOM per SPA convention
- `recog/web/static/app.js` -- add module-level `copilotState` (messages, pending Map, agentState, panelOpen, unread, spawn/stop-pending ids) + panel render/append/update helpers mirroring `ui.js` (append-only DOM keyed by `data-copilot-id`, in-place placeholder mutation, markdown for assistant kinds) -- state survives `views.live.disconnect()`
- `recog/web/static/app.js` -- send path: `sendCopilotFrame` (OPEN-guarded), `submitCopilotChat` (Enter/Shift+Enter, user bubble + placeholder + pending), spawn/stop click handlers implementing the button/Stop matrix rows -- one send helper so every frame gets the same guard
- `recog/web/static/app.js` -- `handleMessage` cases for `copilot_reply` / `copilot_status` dispatching to the helpers (chip vs per-request vs broadcast branches per the matrix) -- server already delivers these frames verbatim
- `recog/web/static/style.css` -- `.copilot-*` styles: button chip states (online/working/blocked/offline), floating panel (anchor-card grammar), bubble kinds (user/assistant/proactive/status/error left-border split), unread dot, pending italics -- reuse existing tokens

**Acceptance Criteria:**
- Given the built page, when every I/O Matrix row is walked through the browser (or by code inspection where no session is available), then each renders as specified — `ui-copilot-panel.test.js` behaviors (in-place resolve, unknown-id append, chip mapping incl. `spawning`, unread dot semantics) hold on the webapp panel.
- Given a page with no copilot session and no frames received, when `#/live` loads, then the chip shows ⚫ offline and the live view is visually and functionally unchanged from baseline apart from the new button.
- Given the full pytest suite and compile checks, when they run, then they are green — no server-side file changed in this story (`git diff --name-only` shows only the three `recog/web/static/` files).

## Spec Change Log

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 9: (high 0, medium 3, low 6)
- defer: 0
- reject: 16: (high 0, medium 6, low 10)
- addressed_findings:
  - `[medium]` `[patch]` No WS-close cleanup existed: a `copilot_reply` broadcast while this tab's WS was closed (view switch, connection loss) is permanently lost, stranding "Жду ответа Claude…" placeholders forever — the extension's own `handleCopilotBridgeDisconnect` (ui.js:1976) covers exactly this and had no webapp analogue. Added idempotent `copilotHandleWsClosed()` (flips all pendings to error "Bridge offline: соединение прервано.", clears the map and spawn/stop pending ids, refreshes controls), called from both `ws.onclose` and `views.live.disconnect()`.
  - `[medium]` `[patch]` The body-level fixed panel stayed open on non-live views, where Send fails with a false "Bridge offline" (the client itself closed the socket) and no button exists to reopen it. The router's leave-live branch now also calls `closeCopilotPanel()`; history and chip state survive in `copilotState`.
  - `[medium]` `[patch]` `channel_disconnected` mass-flip triggered on any unmatched request_id, so another tab's per-request echo (fanned out via the shared subscriber socket) wrongly flipped this tab's unrelated pendings to error. Mass-flip is now gated on the frame carrying NO request_id; a non-empty unmatched id is dropped silently.
  - `[low]` `[patch]` `error`/`channel_disconnected` per-request frames for a spawn/stop request_id never cleared `spawnPendingId`/`stopPendingId`, leaving the input bricked (defensive — no current emitter). Both branches now give spawn/stop ids the same clear-flag + error-line treatment as `bridge_disconnected`/`supervisor_disconnected`.
  - `[low]` `[patch]` Enter-to-send fired mid-IME-composition; `if (e.isComposing) return;` added.
  - `[low]` `[patch]` `stopBtn.disabled` contained an unreachable `&& !stopping` clause inviting a semantics-changing "fix"; simplified with an intent-stating comment.
  - `[low]` `[patch]` The open panel (z-index 1050) buried `.notification` toasts (1000, same corner); toasts raised to 1200 — in style.css AND in `showNotification`'s inline cssText, since the inline style overrides the stylesheet.
  - `[low]` `[patch]` An interim `queued` status stripped the pending styling from a placeholder; `setCopilotMsgText` now keeps pending italics for `working` and `queued`.
  - `[low]` `[patch]` A standalone `channel_disconnected` bubble rendered as an error during spawn, contradicting the plan's "session starting" semantics; the no-id/no-pendings branch now returns silently while spawning.

Rejected on the authority of the intent itself. The verification-mode cluster (jest never executes `app.js`; error-branch renderings have no executable coverage; `transcript_window` content observed by no verifier) proposes what the invocation explicitly forecloses — "No JS test harness — verify with python -m compileall recog frontend and the full pytest suite"; the SPA-wide harness ban is a SPEC non-goal and story 7 owns the acceptance smoke. Stale-chip findings (spawn at a running session after a primer gap; stale-online blocking spawn) are self-healing by the epic's own design: the bridge primes new clients, the server re-primes after `init`, a redundant spawn is an idempotent same-id flap (story 3), and sessions-zero merge flips a false 🟢 to ⚫ (story 2). Timeout proposals for spawn/stop pendings contradict the spec's "the UI must not time it out" (90 s GRACE); a chat-pending timeout has no extension precedent (parity authority — the named behavioral spec has none). Remaining rejects were extension-parity by the intent's own anchor (blank-reply bubble, always-scroll-to-bottom, unread dot only for assistant/proactive, unbounded history, pending `createdAt`/`data-copilot-request-id` mirroring the extension's state shape), unreachable-in-production (proactive reply carrying a pending chat id — no emitter), codebase-consistent omissions (accessibility attributes absent across the whole SPA and the extension panel alike; no input-size caps anywhere in either client), a matrix-specified behavior (standalone error bubble for unmatched terminal `error` status), and the absent channel-connectivity indicator (extension-specific chrome the plan's Task 6 checklist never lists for the webapp).

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 6: (high 0, medium 4, low 2)
- defer: 0
- reject: 17: (high 0, medium 6, low 11)
- addressed_findings:
  - `[medium]` `[patch]` A delayed `onclose` from the previous live WebSocket could clear pending work on a replacement socket. Each callback now verifies socket identity, and `disconnect()` detaches the current socket before closing it.
  - `[medium]` `[patch]` Stop was enabled for conference-owned Copilot sessions even though the backend treats manual Stop as inert there. The control now derives conference ownership from the active meeting source and stays disabled for KTalk, Jitsi, and Matrix meetings.
  - `[medium]` `[patch]` Request-scoped `conference_active`, `bridge_disconnected`, and `supervisor_disconnected` frames from another browser tab produced misleading local bubbles. Unknown nonlocal request ids are now ignored while requestless broadcasts still render.
  - `[medium]` `[patch]` The panel header and input could overflow on narrow or short viewports. Header wrapping and a compact viewport rule now keep the controls and message area reachable.
  - `[low]` `[patch]` WebSocket closure silently cleared spawn/stop pending ids. The history now records that the lifecycle outcome is unknown before the ids are reset.
  - `[low]` `[patch]` An unknown nonterminal per-request status stayed in the pending map but lost pending styling. Any string status other than terminal `done`/`error` now keeps the assistant placeholder visibly pending.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 5: (high 0, medium 3, low 2)
- defer: 0
- reject: 21: (high 0, medium 12, low 9)
- addressed_findings:
  - `[medium]` `[patch]` In-place orphan→KTalk promotion updated only `refineEnabled`, leaving `currentMeeting.source` stale and the conference-owned Stop control enabled. The `refine_state` handler now synchronizes the source to `ktalk` and refreshes Copilot controls without clearing the transcript.
  - `[medium]` `[patch]` The live header could overflow before the existing 768 px breakpoint after adding the wide Copilot control. Live-view headers now wrap whenever their controls no longer fit, without changing other views.
  - `[medium]` `[patch]` Router navigation forcibly closed the panel even though the intent requires panel-open state to survive view switches and the panel to close only through its own controls. The leave-live branch now preserves the open panel and its module-level state.
  - `[low]` `[patch]` `generateCopilotRequestId()` added an unspecified timestamp/random fallback. It now uses the exact required `'copilot_' + crypto.randomUUID()` construction.
  - `[low]` `[patch]` A requestless `channel_disconnected` with no pending chat was suppressed while spawning, contradicting the matrix's standalone-error branch. The suppression was removed.

Rejected findings either contradicted explicit intent (adding a JS harness, spawn/stop/chat timeouts, proactive mute/cooldown, different unread or scroll semantics), proposed broader UX policy absent from the named extension parity surface (history eviction, input-size limits, SPA-wide accessibility conventions), or were disproved by the existing lifecycle contract (meeting-leave emits a new orphan `meeting_change`; reconnect/init re-primes agent state; requestless channel disconnect handles preempted pending work). The unmatched terminal-error bubble and send-while-offline affordance are matrix-specified behaviors. Descriptive strict-vs-narrow lifecycle observations that would reintroduce permanently stranded placeholders were retained as the implemented narrow reading.

## Design Notes

**Spawn/stop success has no ack** — the supervisor answers only through the `agent_state` chip (story 3 rejected request-id replies). Hence: spawn-pending clears when the chip leaves `offline` OR an error echo arrives; stop-pending clears only when the chip reaches `offline` OR an error echo arrives. The 90 s GRACE means "останавливается…" legitimately shows for minutes; the UI must not time it out.

**Webapp deltas vs the extension panel** (each anchored in plan Task 6): Stop/spawn controls exist (extension is auto-spawn only); assistant replies use `renderMarkdown` (extension escapes only); window is ≤15 segments (extension 20); input is disabled during `spawning` (extension never disables); `conference_active`/`manual_preempted`/`supervisor_disconnected` renderings are new (extension never sees these frames).

**Panel is a fixed floating element** (bottom-right, `.anchor-card` visual grammar) rather than docked inside `#view-live`, so opening it never reflows the transcript; it stays in the DOM (hidden) across view switches, which is what lets chat history survive `disconnect()`.

## Verification

**Commands:**
- `node --check recog/web/static/app.js` -- expected: exit 0 (syntax)
- `venv-gigaam/bin/python -m compileall recog frontend` -- expected: no errors
- `venv-gigaam/bin/python -m pytest tests/ -v` -- expected: full suite green (regression only — no Python touched)
- `git diff --name-only` -- expected: only `recog/web/static/app.js`, `recog/web/static/index.html`, `recog/web/static/style.css`

**Manual checks (if no CLI):**
- Open `http://127.0.0.1:9080/#/live` after `./recog-service.sh restart`: button + ⚫ chip present, transcript/anchor/follow behavior unchanged; the full press-button→🟢→chat→Stop smoke is story 7's acceptance pass.

## Auto Run Result

Status: done

Summary: Completed a fresh four-lens follow-up review of `BASE_REVISION..HEAD` and hardened the webapp Copilot UI against stale conference ownership, live-header overflow, and three literal contract deviations.

Files changed:
- `recog/web/static/app.js` — preserves panel-open state across view switches, synchronizes KTalk promotion state, uses the exact request-id construction, and restores the matrix-specified disconnect error.
- `recog/web/static/index.html` — contains the reviewed Copilot button, chip, and floating chat-panel markup; no new patch was required in this pass.
- `recog/web/static/style.css` — wraps the live-view header when its expanded controls no longer fit.

Review findings breakdown:
- patches applied: 5 (high 0, medium 3, low 2)
- items deferred: 0
- items rejected: 21 (high 0, medium 12, low 9)

Follow-up review recommendation: true — patched findings were high 0, medium 3, low 2; score = `3 × 3 + 1 × 2 = 11`.

Verification performed:
- `node --check recog/web/static/app.js` — passed.
- `venv-gigaam/bin/python -m compileall recog frontend` — passed.
- `venv-gigaam/bin/python -m pytest tests/ -v` — 680 passed, 2 deselected.
- `git diff --check` — passed.
- `git diff --name-only BASE_REVISION..HEAD` — exactly `recog/web/static/app.js`, `recog/web/static/index.html`, and `recog/web/static/style.css`.
- `git status --porcelain` — clean.

Residual risks:
- The full press-button → online → chat reply → Stop browser smoke remains story 7's acceptance surface.
- The intent explicitly forbids adding a JavaScript test harness, so the webapp-only state machine remains covered by code inspection, syntax checking, Python regression tests, and the later browser smoke.
