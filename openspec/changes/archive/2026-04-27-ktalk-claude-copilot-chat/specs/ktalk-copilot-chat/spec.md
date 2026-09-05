## ADDED Requirements

### Requirement: Copilot tab in KTalk DOM
The system SHALL provide a `Copilot` tab inside the existing KTalk extension UI, alongside the existing transcript/context surfaces, without removing or regressing those surfaces.

#### Scenario: Copilot tab is available during a meeting
- **WHEN** the extension successfully injects its meeting UI into a supported KTalk/Jitsi page
- **THEN** the UI includes a `Copilot` tab that can be opened by the user
- **AND** the existing transcript and context tabs remain available

#### Scenario: Copilot tab shows connection state
- **WHEN** the bridge WebSocket or Claude channel is unavailable
- **THEN** the Copilot tab shows a visible non-blocking disconnected/error state
- **AND** transcript capture and rendering continue to work

### Requirement: Manual Copilot chat
The system SHALL allow the user to send a free-form chat message from the Copilot tab to a running Claude Code session and receive the assistant reply in the same tab.

#### Scenario: User sends a Copilot message
- **WHEN** the user submits text in the Copilot input
- **THEN** the extension appends the user message to the Copilot chat
- **AND** sends a `copilot_chat_message` frame to the bridge with a unique `request_id`
- **AND** renders a pending assistant/status state for that request

#### Scenario: Claude replies to a manual message
- **WHEN** Claude sends a reply for a known `request_id`
- **THEN** the extension appends or updates the assistant message in the Copilot chat
- **AND** clears the pending state for that request

#### Scenario: Manual message fails
- **WHEN** the bridge or channel reports an error for a `request_id`
- **THEN** the Copilot chat shows the error on that request
- **AND** offers a way to retry or leave the failed message visible for context

### Requirement: Bridge routing for Copilot frames
The bridge SHALL route Copilot messages between the KTalk extension and the local Claude Code channel server without treating the channel server as a meeting participant or extension lifecycle client.

#### Scenario: Channel server connects
- **WHEN** a local channel server connects to `recog/ktalk_bridge.py` and sends `copilot_channel_hello`
- **THEN** the bridge registers it as a Copilot channel client
- **AND** does not reset speaker clock state, sequence counters, or meeting lifecycle state for extension clients

#### Scenario: Extension message is forwarded to Claude
- **WHEN** the bridge receives a valid `copilot_chat_message` from the extension and at least one channel client is connected
- **THEN** the bridge forwards a corresponding `copilot_user_message` event to the channel client
- **AND** includes the message text, `request_id`, meeting identity, and available recent transcript context

#### Scenario: Claude reply is forwarded to extension clients
- **WHEN** the bridge receives a Copilot reply/status frame from the channel server
- **THEN** the bridge broadcasts the corresponding `copilot_reply` or `copilot_status` frame to connected extension clients

### Requirement: Claude Code channel integration
The system SHALL provide a local Claude Code Channel server that can deliver Copilot events into a running Claude Code session and expose tools for Claude to send responses back to the KTalk Copilot chat.

#### Scenario: User message reaches Claude Code
- **WHEN** the bridge forwards a `copilot_user_message` to the channel server
- **THEN** the channel server sends a Claude Channel notification containing the user message and meeting metadata
- **AND** Claude Code can process it in the running repository session

#### Scenario: Claude sends a Copilot message
- **WHEN** Claude calls `send_copilot_message` with text and routing metadata
- **THEN** the channel server forwards that message to the bridge
- **AND** the bridge relays it to the KTalk Copilot chat

#### Scenario: Claude sends status
- **WHEN** Claude calls `set_copilot_status` for a pending request
- **THEN** the status is rendered in the Copilot chat for that request

### Requirement: Recent transcript context
The system SHALL provide Claude with access to recent live meeting transcript context for manual and proactive Copilot behaviour.

#### Scenario: Manual message includes immediate transcript context
- **WHEN** the user submits a Copilot chat message during a meeting
- **THEN** the extension or bridge includes a recent transcript window in the event delivered to Claude

#### Scenario: Claude requests a fresh transcript snapshot
- **WHEN** Claude calls `get_recent_transcript` with a time window or `since_segment_id`
- **THEN** the system returns recent transcript segments for the active meeting
- **AND** includes speaker names, segment ids, text, timestamps where available, and refined state where available

#### Scenario: Refined transcript updates snapshot text
- **WHEN** a `transcript_refine` update replaces text for a segment in the recent buffer
- **THEN** subsequent transcript snapshots use the refined text for that segment

### Requirement: Claude-first proactive transcript ticks
The system SHALL send throttled transcript tick events to Claude while a meeting is active so Claude can decide whether to post proactive Copilot messages.

#### Scenario: Transcript tick is emitted
- **WHEN** new transcript segments arrive during an active meeting and the tick interval has elapsed
- **THEN** the bridge sends a `transcript_tick` event to connected channel clients
- **AND** the event includes new segments plus short recent context

#### Scenario: Claude stays silent
- **WHEN** Claude receives a `transcript_tick` and does not call `send_copilot_message`
- **THEN** no new message is rendered in the Copilot chat

#### Scenario: Claude posts a proactive message
- **WHEN** Claude calls `send_copilot_message` with `kind="proactive"` after a transcript tick
- **THEN** the extension renders the message as an assistant proactive message in the Copilot chat

### Requirement: Proactive noise controls
The system SHALL provide minimal user-facing controls to reduce proactive Copilot noise.

#### Scenario: Proactive message is dismissed
- **WHEN** the user dismisses a proactive Copilot message
- **THEN** the message is hidden or marked dismissed in the current UI session

#### Scenario: Proactive mode is muted
- **WHEN** the user selects `mute proactive 10m`
- **THEN** the extension suppresses rendering new proactive Copilot messages for that local session until the mute expires

#### Scenario: Proactive messages are rate-limited
- **WHEN** Claude sends multiple proactive messages faster than the configured UI cooldown
- **THEN** the extension or bridge suppresses or queues excess proactive messages rather than rendering all of them immediately

### Requirement: Meeting lifecycle handling
The system SHALL associate Copilot messages, replies, transcript snapshots, and proactive ticks with the active meeting to avoid stale cross-meeting output.

#### Scenario: New meeting starts
- **WHEN** the bridge receives a meeting join event
- **THEN** Copilot routing uses the new meeting id for subsequent manual messages, transcript snapshots, and proactive ticks

#### Scenario: Meeting ends
- **WHEN** the bridge receives a meeting leave event
- **THEN** proactive transcript ticks stop for that meeting
- **AND** pending Copilot requests for that meeting are marked stale or ignored when late replies arrive

#### Scenario: Stale reply arrives
- **WHEN** a Copilot reply references a meeting id that is no longer active
- **THEN** the extension or bridge does not render it into the current meeting chat

### Requirement: Feature isolation
The system SHALL isolate Copilot failures from the existing transcription, refine, context, and meeting-storage paths.

#### Scenario: Channel server unavailable
- **WHEN** no Copilot channel client is connected
- **THEN** manual Copilot messages fail with a visible Copilot status/error
- **AND** live transcription, refine updates, and context requests continue to work

#### Scenario: Invalid Copilot frame is received
- **WHEN** the bridge receives a malformed Copilot frame
- **THEN** the bridge rejects or ignores that frame
- **AND** does not close valid extension or subscriber connections because of that malformed frame

#### Scenario: Copilot disabled
- **WHEN** the Copilot feature flag is disabled or the channel server is not running
- **THEN** existing KTalk transcription functionality behaves as before this change
