//! The AI side of recog in Rust (docs/plans/2026-11-07-rust-migration.md, "Target
//! layout"), in place of `recog/ai_provider.py`, `ai_settings.py`, `ai_defaults.py`,
//! `prompt_registry.py`, `increment.py`, `title_generator.py` and `meeting_chat.py`.
//!
//! The files it shares with Python (`ai-settings.json`, `increments/`, `chat.jsonl`)
//! are written byte for byte as Python writes them, and the CLIs are run with the same
//! argv, stdin and environment; tests/parity.rs runs both implementations on the same
//! input and compares. Nothing is wired yet: Python stays the default.

#![forbid(unsafe_code)]

pub mod defaults;
pub mod error;
pub mod install;
pub mod json;
pub mod prompt_registry;
pub mod pytext;
pub mod settings;
pub mod template;

pub use error::{AiError, Result};
