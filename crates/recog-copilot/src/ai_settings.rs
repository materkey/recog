//! The Copilot scenario of `ai-settings.json`: its switch, model and Claude effort.
//!
//! Read through `recog-ai`'s store, which owns the file's rules and which the web UI
//! writes through, so the supervisor accepts exactly the files the settings page saves.
//! A file the store refuses is an error here, and the supervisor treats it as "Copilot
//! off" (`read_enabled` fails closed).

use std::path::PathBuf;

use recog_ai::AiError;
use recog_ai::defaults::DEFAULT_CLAUDE_MODEL;
use recog_ai::install::LegacyEnv;
use recog_ai::settings::{AiSettingsStore, Fallback};

/// What a Copilot session is started with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopilotTask {
    pub enabled: bool,
    pub model: String,
    pub claude_effort: String,
}

/// Where the supervisor learns the Copilot task, read afresh on every call as Python
/// reads the file on every check.
pub trait CopilotSettings {
    fn resolve(&self) -> Result<CopilotTask, String>;
}

/// `<data dir>/ai-settings.json`.
#[derive(Clone, Debug)]
pub struct AiSettingsFile {
    store: AiSettingsStore,
}

impl AiSettingsFile {
    /// The legacy environment (`CLAUDE_MODEL`, `CODEX_MODEL`, `CODEX_REASONING_EFFORT`)
    /// is captured once: the process environment does not change.
    pub fn new(data_dir: impl Into<PathBuf>, legacy: LegacyEnv) -> Self {
        // The Copilot scenario has no prompt file, so the prompts directory is never read.
        AiSettingsFile { store: AiSettingsStore::new(data_dir.into(), None).with_legacy_env(legacy) }
    }
}

impl CopilotSettings for AiSettingsFile {
    /// `resolve_task("copilot")`, reduced to what the supervisor uses.
    fn resolve(&self) -> Result<CopilotTask, String> {
        let task = self.store.resolve_task("copilot", None, Fallback::default()).map_err(|error| match error {
            // The supervisor logs it once; without the path it would not say which file.
            AiError::Io(e) => format!("{}: {e}", self.store.path.display()),
            other => other.to_string(),
        })?;
        Ok(CopilotTask {
            enabled: task.enabled.unwrap_or(true),
            model: task.models.get("claude").cloned().unwrap_or_else(|| DEFAULT_CLAUDE_MODEL.to_owned()),
            claude_effort: task.claude_effort,
        })
    }
}
