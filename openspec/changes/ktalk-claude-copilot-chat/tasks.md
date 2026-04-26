## 1. Channel Spike and Runtime Setup

- [ ] 1.1 Verify local Claude Code Channels availability with the official fakechat-style flow on this machine # requires human smoke
- [x] 1.2 Decide and document the channel server runtime (`bun`/Node TypeScript vs Python) based on the spike
- [x] 1.3 Add minimal channel server scaffold with MCP Streamable HTTP transport and a bridge WebSocket client
- [x] 1.4 Add developer instructions for starting the channel server and registering its HTTP MCP endpoint with Claude Code (`claude mcp add --transport http`)

## 2. Bridge Copilot Protocol

- [x] 2.1 Add bridge-side parsing for `copilot_channel_hello` and store Copilot channel clients separately from extension clients
- [x] 2.2 Add `copilot_chat_message` validation and request routing from extension clients to channel clients
- [x] 2.3 Add `copilot_channel_reply` and `copilot_channel_status` handling from channel clients
- [x] 2.4 Broadcast `copilot_reply` and `copilot_status` frames to extension clients
- [x] 2.5 Return visible `channel_disconnected` status when a user message arrives and no channel client is connected

## 3. KTalk DOM Copilot UI

- [x] 3.1 Add a `Copilot` tab next to the existing `Transcript` and `Context` tabs
- [x] 3.2 Add Copilot state fields for chat messages, pending requests, proactive mute, and connection/status metadata
- [x] 3.3 Render the Copilot chat message list with user, assistant, proactive, status, and error message styles
- [x] 3.4 Add Copilot input handling with enter-to-send and disabled/loading states
- [x] 3.5 Add message actions for copy, dismiss, and `mute proactive 10m`
- [x] 3.6 Route `copilot_reply` and `copilot_status` frames from `src/ws.js` into the Copilot renderer

## 4. Manual Chat Tracer Bullet

- [x] 4.1 Generate stable `request_id` values for extension-originated Copilot messages
- [x] 4.2 Include meeting title, current meeting id if available, and recent transcript window in `copilot_chat_message`
- [x] 4.3 Forward manual user messages from bridge to channel server as `copilot_user_message`
- [x] 4.4 Send Claude channel notifications for `copilot_user_message`
- [x] 4.5 Implement `send_copilot_message` tool in the channel server and relay replies back to bridge
- [ ] 4.6 Verify end-to-end flow: KTalk DOM input → bridge → Claude Code → bridge → KTalk DOM reply # requires human smoke

## 5. Transcript Snapshot Tool

- [x] 5.1 Add bridge-side recent transcript buffer keyed by segment id
- [x] 5.2 Update the recent transcript buffer from live `transcript` frames emitted by the bridge
- [x] 5.3 Apply `transcript_refine` updates to the recent transcript buffer by segment id
- [x] 5.4 Implement bridge request/response path for `get_recent_transcript`
- [x] 5.5 Implement `get_recent_transcript` tool in the channel server
- [ ] 5.6 Verify Claude can refresh transcript context before replying after a slow repo/tool lookup # requires human smoke

## 6. Claude-First Proactive Mode

- [x] 6.1 Add bridge-side transcript tick scheduler gated on active meeting and new transcript segments
- [x] 6.2 Send `transcript_tick` events to connected Copilot channel clients with new segments and short context
- [x] 6.3 Add channel instructions that tell Claude to default to silence and only post high-value proactive messages
- [x] 6.4 Render `send_copilot_message(kind="proactive")` as a proactive assistant message in Copilot chat
- [x] 6.5 Enforce proactive UI cooldown and `mute proactive 10m`
- [ ] 6.6 Verify a real or scripted "question to me" transcript can produce a proactive Copilot message # requires human smoke

## 7. Meeting Lifecycle and Stale Reply Handling

- [x] 7.1 Forward `meeting_started` and `meeting_ended` channel events from bridge to channel clients
- [x] 7.2 Include meeting id in manual messages, transcript ticks, replies, statuses, and transcript snapshots where available
- [x] 7.3 Stop proactive ticks on meeting leave
- [x] 7.4 Drop or mark stale late replies for a meeting that is no longer active
- [x] 7.5 Show non-blocking Copilot system messages for channel connect/disconnect and meeting transitions

## 8. Tests and Validation

- [x] 8.1 Add bridge unit tests for Copilot frame validation and channel-client registration
- [x] 8.2 Add bridge tests for disconnected-channel status and stale meeting reply handling
- [x] 8.3 Add transcript-buffer tests for append, refine update, and snapshot window selection
- [x] 8.4 Add extension tests for Copilot message rendering and WS frame handling
- [x] 8.5 Add a scripted channel-server smoke test with a fake Claude tool call response
- [x] 8.6 Run `python -m pytest tests/ -v`
- [x] 8.7 Run extension tests with `npm test` in `kontur-diarize/ktalk-speaker-extension`
- [x] 8.8 Run `npm run build` in `kontur-diarize/ktalk-speaker-extension`
- [ ] 8.9 Run manual live smoke: KTalk meeting, Copilot manual chat, Claude repo lookup, proactive transcript tick # requires human smoke
