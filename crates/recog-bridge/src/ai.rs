//! The Copilot policy and the increment generator from `recog-ai`: what Python's
//! bridge gets from `AiSettingsStore(get_data_dir())` and `IncrementGenerator`.

use std::path::PathBuf;
use std::sync::Arc;

use recog_ai::settings::{AiSettingsStore, Fallback};
use recog_core::pyjson::Value;

use crate::hooks::{BoxFuture, CopilotPolicy, IncrementGenerator, PolicySource};

/// `resolve_task("copilot")` of the AI settings, read afresh on every call.
pub struct AiSettingsPolicy(pub AiSettingsStore);

impl AiSettingsPolicy {
    /// The settings under the data directory, the shipped prompts.
    #[must_use]
    pub fn new(data_dir: impl AsRef<std::path::Path>) -> Self {
        Self(AiSettingsStore::new(data_dir, None))
    }
}

impl PolicySource for AiSettingsPolicy {
    fn read_policy(&self) -> Result<CopilotPolicy, String> {
        let task =
            self.0.resolve_task("copilot", None, Fallback { prompt: None, label: None }).map_err(|e| e.to_string())?;
        Ok(CopilotPolicy {
            instructions: Value::from(task.prompt),
            revision: Value::from(task.revision),
            // `resolve_task` always sets the switch for the Copilot; the AI functions
            // level must allow a click too.
            enabled: Value::Bool(task.enabled.unwrap_or(true) && task.access.allows_manual()),
        })
    }
}

/// `IncrementGenerator.generate_all` after a conference meeting ends.
pub struct AiIncrements(pub Arc<recog_ai::increment::IncrementGenerator>);

impl IncrementGenerator for AiIncrements {
    fn generate_all(&self, meeting_id: String, meeting_path: PathBuf) -> BoxFuture<Result<(), String>> {
        let generator = self.0.clone();
        Box::pin(async move { generator.generate_all(&meeting_id, &meeting_path).await.map_err(|e| e.to_string()) })
    }
}

#[cfg(test)]
mod tests {
    use recog_ai::install::LegacyEnv;

    use super::*;

    #[test]
    fn settings_access_off_when_policy_read_then_disabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ai-settings.json"),
            r#"{"version": 1, "revision": "0f8fad5b-d9cb-469f-a165-70867728950e", "models": {}, "tasks": {}, "access": "off"}"#,
        )
        .unwrap();
        let policy = AiSettingsPolicy(AiSettingsStore::new(dir.path(), None).with_legacy_env(LegacyEnv::default()));
        assert_eq!(policy.read_policy().unwrap().enabled, Value::Bool(false));
    }
}
