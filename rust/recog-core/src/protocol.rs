//! The words of the bridge's WebSocket protocol (port 8766) that more than one side
//! writes down: the frame `type` strings, the `status` words of `copilot_status`, the
//! agent states, the manual session id and the debug state file.
//!
//! Only what at least two crates, or a crate and the JavaScript of the extension or
//! the web UI, have to agree on lives here; a word one crate alone uses stays in that
//! crate. The frames themselves stay JSON objects read by key: these are the strings,
//! not a schema. `tests/protocol_js.rs` checks that the JavaScript still spells every
//! word it shares with Rust the same way.

// Frame types between the extension and the bridge.
pub const MEETING_JOIN: &str = "meeting_join";
pub const MEETING_JOIN_REJECTED: &str = "meeting_join_rejected";
pub const MEETING_LEAVE: &str = "meeting_leave";
pub const PARTICIPANTS_UPDATE: &str = "participants_update";
pub const SPEAKER_UPDATE: &str = "speaker_update";

// Frame types between the web server (and its tabs) and the bridge.
pub const SUBSCRIBER_HELLO: &str = "subscriber_hello";
pub const TRANSCRIPT_REFINE: &str = "transcript_refine";
pub const USER_PROFILE_REFRESH: &str = "user_profile_refresh";
pub const USER_PROFILE_APPLIED: &str = "user_profile_applied";
pub const USER_PROFILE_ERROR: &str = "user_profile_error";
pub const AI_SETTINGS_REFRESH: &str = "ai_settings_refresh";
pub const AI_SETTINGS_APPLIED: &str = "ai_settings_applied";
pub const AI_SETTINGS_ERROR: &str = "ai_settings_error";
pub const COPILOT_CHAT_MESSAGE: &str = "copilot_chat_message";
pub const COPILOT_CHAT_ECHO: &str = "copilot_chat_echo";
pub const COPILOT_REPLY: &str = "copilot_reply";
pub const COPILOT_STATUS: &str = "copilot_status";
pub const COPILOT_SPAWN_REQUEST: &str = "copilot_spawn_request";
pub const COPILOT_STOP_REQUEST: &str = "copilot_stop_request";

// Frame types between the Copilot supervisor and the bridge.
pub const SUPERVISOR_HELLO: &str = "supervisor_hello";
pub const MEETING_STATE: &str = "meeting_state";
pub const MEETING_STARTED: &str = "meeting_started";
pub const MEETING_ENDED: &str = "meeting_ended";
pub const COPILOT_AGENT_STATUS: &str = "copilot_agent_status";
pub const COPILOT_SETTINGS_CHANGED: &str = "copilot_settings_changed";
pub const COPILOT_CHANNEL_UNAVAILABLE: &str = "copilot_channel_unavailable";

// Frame types between the Copilot MCP channel and the bridge.
pub const COPILOT_CHANNEL_HELLO: &str = "copilot_channel_hello";
pub const COPILOT_CHANNEL_REPLY: &str = "copilot_channel_reply";
pub const COPILOT_CHANNEL_STATUS: &str = "copilot_channel_status";
pub const CLAUDE_SESSIONS: &str = "claude_sessions";
pub const COPILOT_USER_MESSAGE: &str = "copilot_user_message";
pub const TRANSCRIPT_TICK: &str = "transcript_tick";
pub const USER_PROFILE_UPDATED: &str = "user_profile_updated";
pub const COPILOT_POLICY_UPDATED: &str = "copilot_policy_updated";
pub const GET_RECENT_TRANSCRIPT_REQUEST: &str = "get_recent_transcript_request";
pub const GET_RECENT_TRANSCRIPT_RESPONSE: &str = "get_recent_transcript_response";

// `status` words of `copilot_status` that the extension and the web UI act on.
/// The frame carries the effective agent state in `agent_state`.
pub const STATUS_AGENT_STATE: &str = "agent_state";
/// A conference took over the manual session.
pub const STATUS_MANUAL_PREEMPTED: &str = "manual_preempted";
/// A manual spawn refused while a conference is on.
pub const STATUS_CONFERENCE_ACTIVE: &str = "conference_active";
/// A spawn or stop with no supervisor connected to the bridge.
pub const STATUS_SUPERVISOR_DISCONNECTED: &str = "supervisor_disconnected";
/// The web server's link to the bridge is down.
pub const STATUS_BRIDGE_DISCONNECTED: &str = "bridge_disconnected";

// Agent states: `copilot_agent_status.status` and `copilot_status.agent_state`.
pub const AGENT_SPAWNING: &str = "spawning";
pub const AGENT_ONLINE: &str = "online";
pub const AGENT_WORKING: &str = "working";
pub const AGENT_BLOCKED: &str = "blocked";
pub const AGENT_OFFLINE: &str = "offline";

/// The synthetic meeting id of a Copilot session started from the web UI.
pub const MANUAL_MEETING_ID: &str = "manual";

/// The bridge's debug state file, which the web server serves.
pub const KTALK_DEBUG_FILE: &str = "/tmp/ktalk_debug.json";
