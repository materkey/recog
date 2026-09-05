---
id: SPEC-webapp-copilot-live
companions:
  - ../../../docs/plans/20260724-webapp-copilot-live.md
sources: []
---

> **Canonical contract.** This SPEC and the files in `companions:` are the complete, preservation-validated contract for what to build, test, and validate. Source documents listed in frontmatter are for traceability — consult them only if you need narrative rationale or prose color this contract intentionally omits.

# Recog Copilot on webapp `#/live`: manual session spawn + chat panel

## Why

A pain to solve: the copilot chat exists only inside the Chrome extension's floating panel, and the Claude session is spawned only automatically per conference meeting by `recog/copilot_supervisor.py`. There is no way to talk to the copilot without a live KTalk/Jitsi/Matrix conference tab, and no manual session start at all. The webapp live page (`http://127.0.0.1:9080/#/live`) gets feature parity for manual use: mic-only meetings, post-meeting questions, ad-hoc sessions. The adopted companion plan (`docs/plans/20260724-webapp-copilot-live.md`) carries the implementation anchors (file:line references, frame shapes, task steps) and is part of this contract.

## Capabilities

- **CAP-1**
  - **intent:** Webapp subscriber clients receive every copilot broadcast frame (`copilot_reply` and all `copilot_status` variants — per-request, `agent_state` chip, `channel_connected`, `channel_disconnected`) that today reaches only extension clients.
  - **success:** Test: a registered subscriber client receives `copilot_reply` and each `copilot_status` variant; extension clients still receive them; no non-copilot frame gains the wider fan-out.
- **CAP-2**
  - **intent:** User can manually spawn and stop a copilot session from the webapp via `copilot_spawn_request` / `copilot_stop_request` frames (webapp → server → bridge → supervisor), under the constant synthetic lifecycle id `"manual"`.
  - **success:** Test: spawn_request drives `lifecycle.meeting_started("manual")` → Spawn (repeated spawn is an idempotent flap); stop_request drives `meeting_ended` → GRACE → timer → Kill; disconnected supervisor/bridge yields a `copilot_status` error echoing `request_id`.
- **CAP-3**
  - **intent:** A live manual session survives infrastructure restarts and crashes: supervisor reconnect adopts it, a bridge restart re-arms via `supervisor_hello.current_meeting_id`, and a died session respawns — all regardless of `RECOG_COPILOT_AUTOSPAWN`.
  - **success:** Test: `meeting_state {active: true, meeting_id: "manual"}` on reconnect adopts (no kill); `supervisor_hello` with `current_meeting_id == "manual"` and no active conference re-arms the bridge flag before `meeting_state` is computed; `session_died` respawns a manual session with `autospawn=False`.
- **CAP-4**
  - **intent:** Conference meetings take precedence: a spawn attempt during an active conference is rejected, and a conference start preempts a live manual session with user-visible notice.
  - **success:** Test: `copilot_spawn_request` while `_is_meeting_active()` is not forwarded, does not set the manual flag, and replies `copilot_status {status: "conference_active", request_id}`; conference `meeting_started` clears the manual flag and broadcasts `copilot_status {status: "manual_preempted"}` to subscribers.
- **CAP-5**
  - **intent:** The frontend server relays copilot traffic both ways over `/api/live/stream`: inbound whitelist of the three copilot frame types, error replies bound to the originating click, and an agent-state primer for clients connecting mid-session.
  - **success:** Test: whitelist accepts `{copilot_chat_message, copilot_spawn_request, copilot_stop_request}` and rejects others; a send failure produces `copilot_status {status: "bridge_disconnected", request_id}` to the originating client; the last cached `agent_state` is delivered right after `init` on a new connection.
- **CAP-6**
  - **intent:** The `#/live` view offers a "Recog Copilot" button with an agent-state chip (🟢/🟡/🔴/⚫) and a chat panel: per-`request_id` pending state, `transcript_window` of the last ≤15 segments, markdown-rendered replies, and rendering for every status (including `conference_active` / `manual_preempted` info lines).
  - **success:** Acceptance smoke: press the button → chip 🟢 in ~6–10 s → question answered in the panel; Stop shows "останавливается…" until the chip goes ⚫; extension panel (`ui-copilot-panel.test.js`) is the behavioral spec.

## Constraints

- Browser talks only to `/api/live/stream`; all copilot traffic is proxied through the frontend server via a bidirectional `BridgeSubscriber` (D1). No direct browser→bridge WebSocket.
- Extension flow must be untouched — backward-compatibility bar for every bridge/supervisor change.
- The frontend server is a pure relay and holds NO manual-session state; manual-session truth has one runtime owner — the supervisor (D5).
- `RECOG_COPILOT_AUTOSPAWN=0` gates only meeting-driven auto-spawn; manual spawn/stop and crash-respawn work regardless (D3).
- `BridgeSubscriber.send` is best-effort: no queueing across reconnects — a chat message typed while the bridge is down fails fast.
- Manual chat must not inherit conference staleness semantics: `copilot_chat_message` captures `meeting_id` as None when no conference is active, so an orphan-meeting silence-split rotation cannot drop the reply.
- TDD: failing test first per task; given–when–then test naming; full pytest suite green before the next task.
- No JS test harness for the frontend SPA — protocol logic stays server-side under pytest; UI behavior is covered by the acceptance smoke.

## Non-goals

- Proactive ticks in manual sessions — manual chat only; transcript context still flows via `get_recent_transcript` (D4).
- More than one copilot per machine (inherited supervisor design).
- JS test infrastructure for `recog/web/` or e2e webapp tests.
- Fixing the `AUTOSPAWN=0` + conference-preemption desync (documented v1 limitation: the lingering `"manual"` tmux session is only killable via explicit Stop).

## Success signal

On `http://127.0.0.1:9080/#/live` a user presses "Recog Copilot", the chip goes 🟢 within ~10 s, a typed question receives a copilot answer in the panel, and Stop removes the tmux session (`tmux -L recog-copilot ls`) after the 90 s grace. Starting a real conference during a manual session preempts it with "сессия переключена на конференцию" shown in the webapp, while the extension panel keeps working unchanged.
