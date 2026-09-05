## Why

The live KTalk transcription pipeline already produces speaker-attributed, refined meeting text, but during a call it is still mostly a passive transcript. The next useful step is a chat-first live copilot: a small Copilot tab inside the KTalk DOM where the user can ask a running Claude Code session questions that combine the freshest meeting transcript with the local repository, existing skills, Jira/TeamCity tooling, and other configured developer context.

This proposal intentionally starts with a tracer bullet rather than a polished product: prove that KTalk DOM chat, `recog/ktalk_bridge.py`, a Claude Code Channel, the live transcript buffer, and Claude Code's existing agent/tool environment can work together end-to-end. Proactive behaviour is included in a minimal Claude-first form: transcript ticks wake Claude, and Claude decides whether to post a message or stay silent.

## What Changes

- Add a `Copilot` tab to the existing KTalk extension UI, rendered directly inside the KTalk DOM alongside the current `Transcript` and `Context` tabs.
- Add a minimal chat surface in that tab: message list, input, pending/error states, and assistant replies.
- Extend the existing extension-to-bridge WebSocket protocol with `copilot_chat_message`, `copilot_status`, and `copilot_reply` frames.
- Add a local Claude Code Channel bridge that forwards Copilot chat messages into a running Claude Code session and exposes a reply tool that sends assistant responses back to the KTalk DOM chat.
- Provide Claude with current meeting context by including a recent transcript window in the first tracer-bullet request, then formalise a `get_recent_transcript` / meeting snapshot tool for longer-running answers and proactive ticks.
- Add Claude-first proactive mode: `recog/ktalk_bridge.py` sends throttled `transcript_tick` events to the Claude Code Channel; Claude posts to Copilot only when it judges the event worth interrupting the user for.
- Keep local meeting-layer logic deliberately thin for this change: buffering, batching, throttling, and transport only. Meeting understanding lives primarily in Claude for the MVP.
- Run the first version in YOLO mode: Claude Code can use the user's existing repo/tools/skills according to the running Claude Code session's permissions. Security hardening, egress gating, and Side Panel isolation are explicit follow-ups, not part of this tracer bullet.

## Capabilities

### New Capabilities

- `ktalk-copilot-chat`: chat-first live meeting copilot embedded in the KTalk extension UI. Covers manual Copilot chat, bridge-to-Claude Code Channel transport, recent transcript context, assistant replies back into the DOM, and minimal Claude-first proactive transcript ticks.

### Modified Capabilities

None. This is additive and does not change the existing per-speaker transcription/refine requirements.

## Impact

**Code:**
- `browser-extension/src/ui.js` — add Copilot tab/content, chat DOM, input handling, render helpers, and copy/dismiss affordances.
- `browser-extension/src/ws.js` — route `copilot_status` and `copilot_reply` frames to the Copilot UI.
- `browser-extension/src/state.js` — store Copilot messages, pending request metadata, and proactive mute/cooldown state.
- `recog/ktalk_bridge.py` — handle `copilot_chat_message`, maintain a recent transcript buffer, emit `transcript_tick` events, and relay replies/status frames.
- New local channel bridge module/script — MCP/Claude Code Channel server that receives bridge events, pushes `notifications/claude/channel`, exposes reply/status/snapshot tools, and talks back to `recog/ktalk_bridge.py`.
- Tests for extension message formatting, bridge protocol handling, transcript tick throttling, and channel bridge request/reply routing.

**APIs / Protocols:**
- WS `8766` gains additive Copilot frame types:
  - `copilot_chat_message` from extension to bridge.
  - `copilot_status` from bridge to extension.
  - `copilot_reply` from bridge to extension.
  - `copilot_proactive_message` may be folded into `copilot_reply` with `kind="proactive"` or kept separate if clearer during implementation.
- Claude Code Channel events:
  - `copilot_user_message`
  - `transcript_tick`
  - `meeting_started`
  - `meeting_ended`
- Claude Code Channel tools:
  - `send_copilot_message`
  - `set_copilot_status`
  - `get_recent_transcript`

**Dependencies:**
- Claude Code with Channels support enabled.
- A local MCP/Channel server runtime exposed over **Streamable HTTP** (registered with `claude mcp add --transport http`); default Python to match the existing bridge stack, with Node/Bun as a fallback if Python MCP HTTP support is limiting.
- No new frontend framework for the tracer bullet. The existing webpack/vanilla DOM extension stack remains in place.

**Operational:**
- The user starts or attaches a Claude Code session in the repository, with the Copilot channel enabled.
- KTalk extension connects to `recog/ktalk_bridge.py` over the existing `ws://localhost:8766` transport.
- Proactive ticks are throttled by default, e.g. every 10-20 seconds and only while a meeting is active.
- UI anti-noise controls are included as product ergonomics: `dismiss`, `mute proactive 10m`, and a minimum interval between proactive messages.

**Non-goals:**
- Chrome Side Panel UI.
- Rewriting the extension to WXT, Plasmo, React, `assistant-ui`, or AI SDK Elements.
- Token-streaming responses into the UI.
- Permission relay / remote approval UX.
- Secure egress gating, DOM isolation, prompt-injection hardening, or a full lethal-trifecta mitigation design.
- Autonomous writes to Jira, Slack, GitHub, meeting chat, or files.
- High-quality local decision/action-item detectors. For this change, Claude is the primary proactive decision maker.
