//! The Copilot processes of recog (`docs/plans/2026-11-07-rust-migration.md`, step 7).
//!
//! The first binary is `recog-copilot-supervisor`, the port of
//! `recog/copilot_supervisor.py`; the MCP channel (`recog/copilot_channel.py`) is still
//! Python. Not wired into the services yet: the Python supervisor stays the one that runs.
#![forbid(unsafe_code)]

pub mod ai_settings;
pub mod config;
pub mod log;
pub mod py;
pub mod supervisor;
