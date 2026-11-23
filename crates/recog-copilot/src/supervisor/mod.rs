//! The supervisor daemon, in place of `recog/copilot_supervisor.py`: a fresh headless
//! Claude Code Copilot per conference meeting (and per webapp click), in a detached
//! tmux, torn down after the meeting. See
//! `docs/plans/completed/2026-07-02-auto-copilot-session.md` and
//! `docs/plans/completed/20260724-webapp-copilot-live.md`.

pub mod bridge;
pub mod glue;
pub mod lifecycle;
pub mod screen;
pub mod tmux;
pub mod watchdog;

pub use glue::{Supervisor, Tunables};
pub use lifecycle::{Command, MANUAL_MEETING_ID, SessionLifecycle, State, reconcile, reconcile_without_autostart};
pub use screen::{AgentState, classify_screen, trust_dialog_keys};
pub use tmux::{CommandRunner, Error, ProcessRunner, Runner, TMUX_SESSION, TMUX_SESSION_MANUAL, TmuxRunner};
pub use watchdog::{Watchdog, WatchdogHost};
