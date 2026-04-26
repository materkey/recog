## Context

The current KTalk extension is a vanilla JS/webpack MV3 content-script bundle. It injects transcript and context tabs directly into the KTalk/Jitsi page DOM and talks to `ktalk_bridge.py` over the existing `ws://localhost:8766` WebSocket. The bridge already receives meeting lifecycle events, participant metadata, per-speaker PCM chunks, live transcript rows, and refine updates. It also already handles request/response AI flows (`summarize_request`, `anchor_request`) from the extension.

The new Copilot capability should therefore reuse the existing DOM injection and bridge transport. The tracer bullet is not a new browser-extension architecture; it is a new live chat route from KTalk DOM to a running Claude Code session, with the bridge acting as the local router and transcript source.

Claude Code Channels are treated as the agent transport for this change. The channel server is a long-running local MCP server exposed over **Streamable HTTP**; Claude Code connects to it as a configured MCP server (`claude mcp add --transport http ...`) rather than launching it as a stdio subprocess. In parallel, the channel server connects back to `ktalk_bridge.py` as a WebSocket client so it can receive Copilot events and return tool results.

## Goals / Non-Goals

**Goals:**
- Add a KTalk DOM `Copilot` tab with manual chat.
- Route user Copilot messages to a running Claude Code session.
- Return assistant replies and statuses back into the KTalk DOM chat.
- Give Claude recent meeting transcript context for both manual and proactive answers.
- Add minimal Claude-first proactive behaviour via throttled transcript ticks.
- Preserve the existing `Transcript`, `Context`, PCM transport, refine, frontend, and meeting-storage flows.

**Non-Goals:**
- Browser Side Panel, React, WXT/Plasmo, or `assistant-ui`.
- Strong local meeting understanding before Claude.
- Token-by-token streaming.
- Permission relay.
- Security hardening against prompt injection / lethal-trifecta risks.
- Autonomous write actions to external systems.
- Production-ready multi-session management.

## Decisions

### D1. UI container: KTalk DOM tab, not Chrome Side Panel

**Choice:** add `Copilot` as a third injected tab in the same KTalk side area that already hosts `Transcript` and `Context`.

**Why:** the existing extension already has DOM hooks, CSS, tab switching, transcript state, current participants, meeting title, and the bridge WebSocket. Side Panel is a better long-term privacy/isolation container, but it would force new extension permissions, a service-worker/message-passing path, and a second UI runtime before the tracer bullet proves value.

**Alternative — Chrome Side Panel:** deferred. It becomes relevant once private repo/Jira answers must be isolated from the KTalk page DOM.

**Alternative — replace `Context` tab:** rejected. `Context` is already useful and has a different interaction model: one-shot catch-up. `Copilot` is a persistent chat.

### D2. Frontend stack: vanilla extension UI

**Choice:** implement the first chat UI in `src/ui.js` / `src/state.js` using the existing webpack build.

**Why:** the UI surface is small: message list, input, pending/error states, copy/dismiss/mute buttons. Adding React/WXT/Plasmo/assistant-ui now would turn the tracer bullet into a framework migration.

**Alternative — `assistant-ui` / AI SDK Elements:** deferred until there is a need for richer tool-call rendering, threads, attachments, or streaming.

### D3. Browser-to-bridge protocol: extend existing WS:8766

**Choice:** reuse `ws://localhost:8766` for Copilot frames.

Extension → bridge:
```json
{
  "type": "copilot_chat_message",
  "request_id": "copilot_...",
  "meeting_id": "current",
  "meeting_title": "...",
  "text": "что мне ответить про capacity?",
  "transcript_window": "[14:20] Андрей: ...",
  "created_at": 1777200000000
}
```

Bridge → extension:
```json
{
  "type": "copilot_reply",
  "request_id": "copilot_...",
  "kind": "manual|proactive",
  "text": "...",
  "created_at": 1777200000000
}
```

Status frames:
```json
{
  "type": "copilot_status",
  "request_id": "copilot_...",
  "status": "working|error|channel_disconnected",
  "message": "Claude смотрит repo..."
}
```

**Why:** the bridge already has client tracking, broadcast helpers, and extension reconnect behaviour. A second localhost server would duplicate connection health and lifecycle handling.

**Alternative — extension talks directly to channel server:** rejected for MVP. It requires host permissions/token discovery and bypasses bridge-owned meeting context.

### D4. Claude channel topology: channel server as bridge WS client, MCP Streamable HTTP toward Claude Code

**Choice:** the Claude Code channel server is a long-running local process. It exposes an MCP **Streamable HTTP** endpoint that Claude Code connects to (registered once via `claude mcp add --transport http copilot http://127.0.0.1:8767/mcp`). In parallel, the channel server connects to `ktalk_bridge.py` as a special WebSocket client and sends a `copilot_channel_hello` frame. The bridge stores these connections separately from extension clients, similar in spirit to `subscriber_hello`, so they do not affect meeting lifecycle or speaker-clock state.

Flow:
```
KTalk DOM extension
    │ copilot_chat_message
    ▼
ktalk_bridge.py
    │ copilot_user_message / transcript_tick
    ▼
Claude channel server  ── HTTP MCP ── Claude Code
    ▲                                  │
    │ copilot_channel_reply/status      │ tool call
    └──────────────────────────────────┘
```

**Why:** Streamable HTTP keeps the channel server independent of Claude Code's process lifecycle (survives `/exit`, supports multiple sessions, can run under launchd alongside other recog services) and matches the long-running posture of `ktalk_bridge.py`. Streamable HTTP is bidirectional (SSE-style), so server-pushed channel notifications (`copilot_user_message`, `transcript_tick`, ...) work the same as they would over stdio. Having the channel server dial into the existing bridge as a WS client (rather than the bridge calling out) keeps all meeting routing in one process and reuses the existing reconnect/lifecycle code.

**Alternative — bridge launches Claude/CLI directly:** rejected. Existing `_call_claude_cli` and `_call_codex_cli` are good for one-shot prompts, but they do not preserve the user's running Claude Code session, repo context, tools, or skills.

**Alternative — bridge hosts an HTTP API for the channel server:** acceptable but less aligned with the existing architecture. Use only if MCP/channel SDK integration makes WS awkward.

### D5. Agent context: transcript snapshot as data, not a local semantic model

**Choice:** for manual chat, the extension sends a recent transcript window immediately. The bridge also maintains a recent segment buffer and exposes it to Claude via a channel tool:

```json
get_recent_transcript({
  "window_seconds": 180,
  "since_segment_id": 1234,
  "include_refined": true
})
```

**Why:** the first answer should work even before the bridge-side snapshot tool is polished. The tool then handles longer Claude runs: Claude can re-read the latest transcript before finalizing an answer.

**Refine handling:** when `transcript_refine` updates a segment, the bridge updates the buffered segment text by `segment_id`. Snapshots prefer refined text where available.

**Alternative — full meeting layer with topic/action/decision state:** deferred. It is useful later, but this change intentionally lets Claude be the primary meeting reasoner.

### D6. Proactive mode: Claude-first transcript ticks

**Choice:** while a meeting is active, the bridge sends throttled `transcript_tick` channel events containing new segments plus a short rolling context. Claude decides whether to call `send_copilot_message` or stay silent.

Tick shape:
```json
{
  "kind": "transcript_tick",
  "meeting_id": "...",
  "meeting_title": "...",
  "new_segments": [
    {"id": 1842, "speaker": "Андрей", "text": "Слава, CI это выдержит?", "refined": false}
  ],
  "recent_context": "[14:20] ...",
  "local_user_name": "Слава"
}
```

Channel instruction:
- Default to silence.
- Post only if the event looks immediately useful: question to the user, task for the user, topic in the user's responsibility area, missing owner/deadline, or technical risk worth surfacing.
- If code context matters, use Claude Code tools before replying.
- If a tool lookup takes time, call `get_recent_transcript` again before final answer.

**Why:** this tests the product thesis directly: Claude as a live meeting copilot, not just a Q&A endpoint.

**Anti-noise controls:** even in YOLO mode, the bridge/UI enforce product-level throttles:
- ticks no more often than every 10-20 seconds;
- proactive UI messages no more often than once per minute by default;
- `dismiss` hides a message;
- `mute proactive 10m` suppresses proactive renders locally.

**Alternative — local detectors first:** safer and cheaper, but not what this tracer bullet is trying to validate.

### D7. Channel tools

The channel server exposes three MVP tools to Claude:

1. `send_copilot_message`
   - Arguments: `request_id?`, `meeting_id?`, `kind`, `text`, `actions?`.
   - Sends a `copilot_channel_reply` frame to the bridge.

2. `set_copilot_status`
   - Arguments: `request_id`, `status`, `message`.
   - Sends a `copilot_channel_status` frame to the bridge.

3. `get_recent_transcript`
   - Arguments: `window_seconds?`, `since_segment_id?`, `include_refined?`.
   - Requests a snapshot from the bridge and returns transcript text plus segment metadata.

**Why:** this is the smallest tool surface that supports manual chat, proactive messages, and freshness checks.

### D8. Meeting lifecycle and routing

**Choice:** every Copilot event carries `meeting_id` and `request_id` where applicable.

- On `meeting_join`, the bridge emits a `meeting_started` channel event and the UI may show a small system message.
- On `meeting_leave`, the bridge emits `meeting_ended`, clears pending proactive timers, and marks outstanding requests as stale.
- Manual chat history is kept in extension memory for the current page/session only.
- Proactive ticks stop when no active meeting exists.

**Why:** existing services can switch meetings during reconnects. Explicit IDs prevent a late Claude reply from being rendered into the wrong meeting without requiring durable storage.

### D9. Failure handling

**Choice:** failure is visible but non-blocking.

- If the Claude channel is disconnected, manual messages render an error/status in Copilot; transcript and context tabs keep working.
- If Claude does not answer within a timeout, the UI leaves the user message visible and shows retry.
- If the bridge restarts, extension reconnect uses existing WS reconnect; Copilot shows disconnected status until channel returns.
- If the channel server restarts, it reconnects with `copilot_channel_hello`; bridge resumes forwarding new requests/ticks.

**Why:** this is an optional assistant surface. It must not destabilize live transcription.

### D10. YOLO mode boundary

**Choice:** no new permission sandbox is introduced in this change. Claude Code uses the permissions, MCP servers, skills, and workspace access of the running session.

**Why:** the goal is to validate the experience and integration path, not solve secure agent delegation. The user explicitly chose YOLO mode and DOM embedding for this proposal.

**Follow-up boundary:** any future version that posts to external systems, auto-approves tools, or displays private repo answers outside the extension page needs a separate security design.

## Risks / Trade-offs

- **[Claude proactive messages are noisy]** → throttle ticks, throttle proactive renders, require "default silence" channel instruction, add `mute proactive 10m`.
- **[Claude Code Channels unavailable or unstable]** → start with fakechat/manual spike; surface disconnected status; keep existing AI `anchor_request` path untouched.
- **[A reply arrives after meeting switch]** → include `meeting_id`; bridge/UI drop stale replies.
- **[Transcript context is stale during long repo lookup]** → instruct Claude to call `get_recent_transcript` before final answer; expose snapshot tool.
- **[KTalk DOM layout changes break the Copilot tab]** → reuse existing tab insertion patterns and tests around DOM selectors; failure only hides Copilot.
- **[Bridge Copilot bugs affect transcription]** → isolate Copilot state and channel clients from extension client lifecycle; unknown Copilot errors log and return status, not exceptions through PCM/transcript paths.
- **[YOLO mode can leak or misuse private data]** → accepted for this tracer bullet; documented as out of scope and future hardening work.

## Migration Plan

1. Add bridge-side Copilot state and frame handlers behind a runtime flag, default off while developing.
2. Add the DOM `Copilot` tab and manual chat UI; route messages to bridge and render bridge replies/status.
3. Add the local Claude Code channel server and validate the manual request/reply loop with a running Claude session.
4. Add recent transcript snapshot support and refine-aware buffer updates.
5. Add proactive transcript ticks behind a separate toggle; tune tick/proactive intervals on real meetings.
6. Flip the feature on locally once tracer bullet is stable.

**Rollback:** disable the Copilot flag or run without the channel server. Existing transcript/refine/context features are unaffected because all protocol additions are additive and unknown frame types are ignored.

## Open Questions

1. Should the first channel server be Python to stay close to `ktalk_bridge.py`, or TypeScript/Bun to match Claude fakechat examples? Default recommendation: **Python** — Streamable HTTP is supported by the Python `mcp` SDK and removes the main reason fakechat was Bun-shaped (stdio + Node ergonomics). Reach for Bun only if the Python MCP HTTP server proves limiting.
2. Should manual chat include transcript text from the extension immediately, or should it only send request metadata and force Claude to call `get_recent_transcript`? Default recommendation: do both for the tracer bullet, then simplify after observing latency/context size.
3. Should Copilot chat history persist in `meetings/<id>/`? Default recommendation: no for this change; keep it ephemeral until the UX proves useful.
4. What is the local user's canonical name for proactive detection prompts? Default recommendation: use extension participants/local transcript speaker if available, otherwise `MIC_SPEAKER_NAME`.
