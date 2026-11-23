//! Copilot on Codex (docs/plans/completed/2026-11-23-codex-copilot.md): the harness that runs
//! `codex app-server` in the Copilot's tmux session when Codex is the Copilot provider.

pub mod app_server;
pub mod events;
pub mod harness;
pub mod instructions;
pub mod session;
