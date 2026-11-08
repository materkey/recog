//! The conference bridge of recog — the port of `recog/ktalk_bridge.py`
//! (docs/plans/2026-11-07-rust-migration.md, "Target layout").
//!
//! One WebSocket server (`ws://localhost:8766`) for every role: the Chrome
//! extension's conference tabs, the webapp's `BridgeSubscriber`, the Copilot
//! supervisor and the Copilot channel. Every socket starts as an extension and moves
//! to its role on its hello. Browser pages must pass the Origin allowlist of
//! `recog_core::ws_origin`; the Python daemons send no Origin and pass.
//!
//! The boundary toward the live process (`recog-live`, not ported yet) is in
//! [`hooks`]: the callbacks Python's `LiveTranscriber` registers become traits, and
//! the calls it makes the other way are methods of [`Bridge`].
//!
//! # Frames
//!
//! Received (`type`, who sends it):
//!
//! | type | from | what the bridge does |
//! |---|---|---|
//! | `subscriber_hello` | webapp | moves the socket to the subscribers |
//! | `copilot_channel_hello` | channel | moves it to the channel set, sends `copilot_policy_updated`, announces `channel_connected` |
//! | `supervisor_hello` | supervisor | moves it to the supervisors, re-arms a reported manual session, answers `meeting_state` |
//! | `speaker_update` | extension | speaker history, segments, `speaker_changed`/`segment_completed` hooks |
//! | `participants_update` | extension | participants and `localMicMuted` (owner's tab only) |
//! | `pcm_transport_stats` | extension | kept for the debug file |
//! | `connection_status` | extension | conference connected/disconnected |
//! | `meeting_join` | extension | opens/reopens the meeting, owner, `meeting_started`, `transcript_history` |
//! | `meeting_leave` | extension | ends the meeting, `meeting_ended`, `new_meeting` |
//! | `meeting_title_update` | extension | retitles the meeting |
//! | `request_transcript` | extension | `transcript_history` to the asking tab |
//! | `ping` | extension | nothing (heartbeat) |
//! | `clock_sync` | extension | browser-to-bridge clock offset |
//! | `pcm_chunk` | extension | checked and handed to the `pcm_chunk` hook |
//! | `copilot_chat_message` | extension, webapp | `copilot_user_message` to the channel, `copilot_chat_echo` to the webapp |
//! | `copilot_channel_reply` | channel | `copilot_reply` (or a terminal error for a stale request) |
//! | `copilot_channel_status` | channel | `copilot_status` per request |
//! | `get_recent_transcript_request` | channel | `get_recent_transcript_response` |
//! | `claude_sessions` | channel | the session count of the agent-state merge |
//! | `copilot_spawn_request` | webapp | relayed to the supervisor, arms the manual mirror |
//! | `copilot_stop_request` | webapp | relayed to the supervisor, clears the mirror |
//! | `copilot_agent_status` | supervisor | the screen state of the agent-state merge |
//! | `user_profile_refresh` | webapp | re-reads the identity: `user_profile_applied`/`_error`, `user_profile_updated` |
//! | `ai_settings_refresh` | webapp | re-reads the policy: `ai_settings_applied`/`_error`, `copilot_settings_changed`, `copilot_policy_updated` |
//!
//! Sent: `transcript`, `transcript_refine`, `transcript_history`, `new_meeting`,
//! `meeting_join_rejected` (`ktalk_only`, `another_tab_active`,
//! `capture_unavailable`), `meeting_started`, `meeting_ended`, `meeting_state`,
//! `transcript_tick`, `copilot_user_message`, `copilot_chat_echo`, `copilot_reply`,
//! `copilot_status` (`channel_connected`, `channel_disconnected`, `agent_state`,
//! `error`, `conference_active`, `supervisor_disconnected`, `manual_preempted` and the
//! channel's per-request statuses), `get_recent_transcript_response`,
//! `copilot_policy_updated`, `user_profile_updated`, `user_profile_applied`,
//! `user_profile_error`, `ai_settings_applied`, `ai_settings_error`,
//! `copilot_settings_changed`, `copilot_channel_unavailable`, and the relayed
//! `copilot_spawn_request`/`copilot_stop_request`.
//!
//! # Known differences from Python
//!
//! Beyond those listed in [`speakers`] and [`meeting`]:
//! - frames are compared by value: Python's `json.dumps` and this port produce the
//!   same text (`pyjson`), but JSON that only Python parses (integers beyond i128)
//!   is refused as invalid here;
//! - a `meta.json` that is not an object counts as "no conference" (Python raises
//!   from inside the check);
//! - an exception escaping the disconnect cleanup is printed as `Connection error:`
//!   (Python's websockets logs `connection handler failed` with a traceback);
//! - the server binds when `start()` returns, instead of Python's fixed 0.5 s wait;
//! - frames sent from the live threads (`send_transcript`, `send_refine`) are queued
//!   on the bridge's runtime one task each, as `run_coroutine_threadsafe` does.
#![forbid(unsafe_code)]

pub mod ai;
pub mod bridge;
pub mod client;
pub mod copilot;
mod debug;
pub mod hooks;
pub mod introspect;
pub mod json;
pub mod meeting;
mod messages;
mod relay;
mod server;
pub mod speakers;
pub mod testing;

pub use bridge::{Bridge, BridgeConfig, Logger, RefineUpdate, TranscriptLine};
pub use client::{Client, ClientId, ClientSink, Connection, Inbound, SendError};
pub use introspect::Role;
