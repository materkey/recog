//! The Copilot processes of recog (`docs/plans/2026-11-07-rust-migration.md`, step 7).
//!
//! Two binaries: `recog-copilot-supervisor`, the port of `recog/copilot_supervisor.py`,
//! and `recog-copilot-channel`, the port of the MCP channel `recog/copilot_channel.py`.
//! Not wired into the services yet: the Python processes stay the ones that run.
#![forbid(unsafe_code)]

pub mod ai_settings;
pub mod channel;
pub mod config;
pub mod log;
pub mod py;
pub mod supervisor;
