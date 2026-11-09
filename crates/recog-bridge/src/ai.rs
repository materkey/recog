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
            // `resolve_task` always sets it for the Copilot.
            enabled: Value::Bool(task.enabled.unwrap_or(true)),
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
