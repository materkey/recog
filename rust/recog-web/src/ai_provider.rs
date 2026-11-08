//! The configured AI providers and prompt rendering of the web routes — the port of
//! `recog/web/ai_provider.py`.

use std::path::PathBuf;
use std::sync::Arc;

use recog_ai::AiError;
use recog_ai::install::LegacyEnv;
use recog_ai::provider::{AiProvider, ProviderFactory};
use recog_ai::settings::{AiSettingsStore, Fallback, ResolvedTask, render_prompt};
use recog_ai::template::variables;

/// What the AI routes need: the settings of the data directory and a way to make
/// providers (the CLIs in production, fakes in tests).
#[derive(Clone)]
pub struct AiContext {
    pub data_dir: PathBuf,
    pub prompts_dir: PathBuf,
    pub legacy_env: LegacyEnv,
    pub factory: Arc<dyn ProviderFactory>,
}

impl AiContext {
    /// `AiSettingsStore(get_data_dir())`.
    #[must_use]
    pub fn store(&self) -> AiSettingsStore {
        AiSettingsStore::new(&self.data_dir, Some(self.prompts_dir.clone())).with_legacy_env(self.legacy_env.clone())
    }

    /// `get_task_settings(task)`: `analysis` and `increment` are the increment scenario.
    ///
    /// # Errors
    /// Unreadable settings or an unknown scenario.
    pub fn task_settings(&self, task: &str) -> Result<ResolvedTask, AiError> {
        let task_id = match task {
            "analysis" | "increment" => "analysis.increment",
            other => other,
        };
        self.store().resolve_task(task_id, None, Fallback::default())
    }

    /// `get_provider(task, settings)`: the scenario's selected provider.
    ///
    /// # Errors
    /// The CLI is not installed or the provider unknown.
    pub fn provider(&self, task: &str, settings: &ResolvedTask) -> Result<Box<dyn AiProvider>, AiError> {
        self.factory.create(settings, task, None)
    }

    /// `get_all_providers(task, settings)`: every selected provider; a missing CLI is
    /// skipped unless the scenario was customized.
    ///
    /// # Errors
    /// A missing CLI of a customized scenario, or another provider error.
    pub fn all_providers(&self, task: &str, settings: &ResolvedTask) -> Result<Vec<Box<dyn AiProvider>>, AiError> {
        let mut providers = Vec::new();
        for key in &settings.providers {
            match self.factory.create(settings, task, Some(key)) {
                Ok(provider) => providers.push(provider),
                Err(AiError::NotFound(_)) if !settings.customized => {}
                Err(error) => return Err(error),
            }
        }
        Ok(providers)
    }
}

/// `build_prompt(question, transcript, user_name, settings, participants,
/// meeting_title)`.
///
/// # Errors
/// A template with a placeholder that cannot be filled.
pub fn build_prompt(
    settings: &ResolvedTask,
    transcript: &str,
    user_name: &str,
    participants: &str,
    meeting_title: &str,
) -> Result<String, AiError> {
    render_prompt(
        &settings.prompt,
        &variables([
            ("transcript", transcript),
            ("user_name", user_name),
            ("participants", participants),
            ("meeting_title", meeting_title),
        ]),
    )
}
