# ktalk-copilot-chat Specification

## Purpose
TBD - created by archiving change ktalk-claude-copilot-chat. Update Purpose after archive.
## Requirements
### Requirement: Copilot tab in KTalk DOM
The system SHALL provide a `Copilot` tab inside the existing KTalk extension UI, alongside the existing transcript surface, without removing or regressing that surface.

#### Scenario: Copilot tab is available during a meeting
- **WHEN** the extension successfully injects its meeting UI into a supported KTalk/Jitsi page
- **THEN** the UI includes a `Copilot` tab that can be opened by the user
- **AND** the existing transcript tab remains available

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
- **WHEN** a local channel server connects to `ktalk_bridge.py` and sends `copilot_channel_hello`
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

### Requirement: Copilot broadcast fan-out to subscriber clients
The bridge SHALL send every broadcast Copilot frame (`copilot_reply` and the broadcast `copilot_status` variants — the `agent_state` chip, `channel_connected`, and the `channel_disconnected` broadcast on last channel-client departure) to registered subscriber clients in addition to extension clients, without widening the fan-out of any non-Copilot frame. Per-request rejection echoes (`conference_active`, `supervisor_disconnected`, and the no-channel `channel_disconnected` chat echo) stay targeted at the originating socket; `manual_preempted` goes to subscriber clients only.

#### Scenario: Subscriber receives Copilot frames
- **WHEN** the bridge broadcasts a `copilot_reply` or a broadcast `copilot_status` frame
- **THEN** registered subscriber clients receive the frame
- **AND** connected extension clients still receive shared-audience frames unchanged; subscriber-only `manual_preempted` is not widened

#### Scenario: Non-Copilot frames keep their original audience
- **WHEN** the bridge broadcasts a non-Copilot frame intended for extension clients
- **THEN** subscriber clients do not receive it through the Copilot fan-out

### Requirement: Manual Copilot session lifecycle
The system SHALL let a user spawn and stop a Copilot session manually via `copilot_spawn_request` / `copilot_stop_request` frames carrying a client-generated `request_id`, relayed by the bridge to supervisor clients; the supervisor maps them onto the constant synthetic lifecycle id `manual`.

#### Scenario: Manual spawn request
- **WHEN** the bridge receives `copilot_spawn_request` from a subscriber client while no conference meeting is active and a supervisor client is connected
- **THEN** the bridge forwards the frame to supervisor clients, and arms its manual-session flag only after successful delivery with no conference having started mid-relay
- **AND** the supervisor drives `meeting_started("manual")` → spawn, treating a repeated spawn with a live manual session as an idempotent same-id flap (an explicit spawn also resets the respawn backoff budget when no retry is pending, reviving a session parked after exhausted respawn attempts)

#### Scenario: Manual stop request
- **WHEN** the bridge receives `copilot_stop_request` while a manual session is live
- **THEN** the bridge clears its manual-session flag and forwards the frame to supervisor clients
- **AND** the supervisor treats it as `meeting_ended("manual")`, killing the session after the grace period

#### Scenario: Stop during a conference session is inert
- **WHEN** `copilot_stop_request` arrives while the live session is bound to a conference meeting
- **THEN** the bridge still clears its manual-session flag and relays the frame
- **AND** the supervisor's `meeting_ended("manual")` no-ops on the mismatched lifecycle id — a webapp Stop can never kill a conference-bound session

#### Scenario: Supervisor unavailable
- **WHEN** a `copilot_spawn_request` or `copilot_stop_request` arrives with no supervisor client connected
- **THEN** the bridge replies `copilot_status {status: "supervisor_disconnected", request_id}` to the originating socket
- **AND** a stop request still clears the manual-session flag

#### Scenario: Autospawn flag does not gate manual frames
- **WHEN** `RECOG_COPILOT_AUTOSPAWN=0` is set
- **THEN** meeting-driven frames remain inert for the supervisor
- **AND** manual spawn/stop and crash-respawn of a manual session still work

### Requirement: Conference precedence over manual sessions
Conference meetings SHALL take routing precedence over manual Copilot sessions: a manual spawn during an active conference is rejected, and, while meeting-driven autospawn is enabled, a conference start preempts a live manual session with a user-visible notice.

#### Scenario: Spawn rejected during an active conference
- **WHEN** `copilot_spawn_request` arrives while a conference meeting is already active
- **THEN** the bridge does not forward the frame to the supervisor and leaves the manual-session flag untouched
- **AND** replies `copilot_status {status: "conference_active", request_id}` to the originating socket
- **AND** if a conference starts mid-relay instead, the frame may already have been delivered, but the manual-session flag is not armed and the originator still receives `conference_active`

#### Scenario: Conference start preempts a manual session
- **WHEN** a conference `meeting_started` is emitted while the manual-session flag is armed and meeting-driven autospawn is enabled
- **THEN** the bridge clears the manual-session flag
- **AND** broadcasts `copilot_status {status: "manual_preempted"}` to subscriber clients

#### Scenario: Conference starts while autospawn is disabled
- **WHEN** a conference `meeting_started` is emitted while the manual-session flag is armed and `RECOG_COPILOT_AUTOSPAWN=0`
- **THEN** the bridge clears the manual-session flag and broadcasts `copilot_status {status: "manual_preempted"}` to subscriber clients
- **AND** the supervisor ignores the meeting frame, so the existing `manual` tmux session may linger until an explicit Stop

### Requirement: Manual session recovery via supervisor hello
`supervisor_hello` SHALL carry a `current_meeting_id` field — the id of a session the supervisor intends to keep alive (SPAWNING/RUNNING); GRACE and IDLE report null — so a restarted bridge can re-learn a live manual session without a reconnect inside the grace window defeating Stop.

#### Scenario: Bridge restart re-arms the manual flag
- **WHEN** the bridge receives `supervisor_hello` with `current_meeting_id == "manual"` and no conference meeting is active
- **THEN** the bridge re-arms its manual-session flag before computing `meeting_state`
- **AND** replies `meeting_state {active: true, meeting_id: "manual"}` so the supervisor reconcile adopts the live session instead of killing it

### Requirement: Webapp Copilot proxy over the live stream
The frontend server SHALL relay Copilot traffic both ways over `/api/live/stream`: inbound frames restricted to a whitelist of `copilot_chat_message`, `copilot_spawn_request`, and `copilot_stop_request` sent to the bridge via the subscriber's best-effort send, and bridge Copilot frames (`copilot_reply` / `copilot_status`, alongside `transcript_refine`) fanned out to all connected live-stream clients.

#### Scenario: Whitelisted inbound frame is relayed
- **WHEN** a browser sends one of the three whitelisted Copilot frame types over `/api/live/stream`
- **THEN** the server relays the frame to the bridge through the subscriber

#### Scenario: Non-whitelisted inbound frame is dropped
- **WHEN** a client sends a non-whitelisted frame type over `/api/live/stream`
- **THEN** the server drops the frame with a log

#### Scenario: Bridge link is down
- **WHEN** a whitelisted inbound frame arrives while the bridge subscriber is disconnected or the send fails
- **THEN** the server replies `copilot_status {status: "bridge_disconnected", request_id}` to the originating client only

#### Scenario: Agent-state primer on connect
- **WHEN** a new client connects to `/api/live/stream` and a last `agent_state` Copilot status has been cached
- **THEN** the server delivers the cached `copilot_status {status: "agent_state", agent_state: "spawning"|"online"|"working"|"blocked"|"offline"}` frame right after `init`
