//! The AI side of recog in Rust (docs/plans/2026-11-07-rust-migration.md, "Target
//! layout"), in place of `recog/ai_provider.py`, `ai_settings.py`, `ai_defaults.py`,
//! `prompt_registry.py`, `increment.py`, `title_generator.py` and `meeting_chat.py`.
//!
//! The files it shared with Python (`ai-settings.json`, `increments/`, `chat.jsonl`)
//! are written byte for byte as Python wrote them, and the CLIs are run with the same
//! argv, stdin and environment, so an installation moved from the Python modules keeps
//! its files. The tests hold the answers the Python modules gave, kept since the Python
//! side and the differential tests/parity.rs were deleted.

#![forbid(unsafe_code)]

pub mod defaults;
pub mod error;
pub mod increment;
pub mod install;
pub mod json;
pub mod meeting_chat;
pub mod prompt_registry;
pub mod provider;
pub mod pytext;
pub mod settings;
pub mod template;
pub mod timestamp;
pub mod title;

pub use error::{AiError, Result};
