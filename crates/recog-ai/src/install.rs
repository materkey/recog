//! The data directory and the few installation settings this crate reads.
//!
//! `recog-core` (being written in parallel, `recog/install_config.py` and
//! `recog/config.py`) will provide these; until it does, this module resolves them the
//! way the Python modules do, so that nothing else in the crate reads the environment.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::{AiError, Result};
use crate::pytext;

/// `recog.config.data_dir_target()`: `RECOG_DATA_DIR` (absolute or `~/…`), else
/// `~/.recog`. The legacy fallback of `get_data_dir()` to data kept in the checkout is
/// not reproduced: it belongs to `recog-core` and its migration.
pub fn data_dir() -> Result<PathBuf> {
    data_dir_from(std::env::var_os("RECOG_DATA_DIR"), home_dir())
}

/// [`data_dir`] from explicit values, for tests.
pub fn data_dir_from(value: Option<OsString>, home: Option<PathBuf>) -> Result<PathBuf> {
    let home = home.unwrap_or_default();
    let value = value.map(|value| value.to_string_lossy().into_owned()).unwrap_or_default();
    let value = pytext::strip(&value);
    let path = if value.is_empty() { home.join(".recog") } else { expand_user(value, &home) };
    if !path.is_absolute() {
        return Err(AiError::Value("RECOG_DATA_DIR must be an absolute path (or start with ~/)".to_owned()));
    }
    Ok(path)
}

/// `os.path.expanduser` for `~` and `~/…` (not `~user`).
pub fn expand_user(path: &str, home: &std::path::Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

/// `$HOME`, which `os.path.expanduser` and `Path.home()` read first.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from).or_else(std::env::home_dir)
}

/// The shipped prompts, `crates/recog-ai/prompts/` of the checkout this crate was built from (the
/// default `prompts_dir` of `AiSettingsStore` and `IncrementGenerator`).
pub fn default_prompts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("prompts")
}

/// The legacy model and effort variables the AI settings still honour below the saved
/// values (`.env` or the environment; `recog-core` loads `.env`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyEnv {
    pub claude_model: Option<String>,
    pub codex_model: Option<String>,
    pub codex_reasoning_effort: Option<String>,
}

impl LegacyEnv {
    /// The variables of this process, read at the moment of the call as Python does.
    pub fn from_process() -> LegacyEnv {
        LegacyEnv::from_vars(std::env::vars_os())
    }

    pub fn from_vars<K: Into<OsString>, V: Into<OsString>>(vars: impl IntoIterator<Item = (K, V)>) -> LegacyEnv {
        let mut env = LegacyEnv::default();
        for (key, value) in vars {
            let (key, value) = (key.into(), value.into());
            let value = Some(value.to_string_lossy().into_owned());
            match key.to_str() {
                Some("CLAUDE_MODEL") => env.claude_model = value,
                Some("CODEX_MODEL") => env.codex_model = value,
                Some("CODEX_REASONING_EFFORT") => env.codex_reasoning_effort = value,
                _ => {}
            }
        }
        env
    }

    /// `os.getenv(f"{provider.upper()}_MODEL", "")`.
    pub fn model(&self, provider: &str) -> &str {
        let value = match provider {
            "claude" => &self.claude_model,
            "codex" => &self.codex_model,
            _ => &None,
        };
        value.as_deref().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_override_given_home_when_resolved_then_dot_recog() {
        let dir = data_dir_from(None, Some(PathBuf::from("/home/u"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.recog"));
    }

    #[test]
    fn tilde_override_given_home_when_resolved_then_expanded() {
        let dir = data_dir_from(Some(" ~/data ".into()), Some(PathBuf::from("/home/u"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/data"));
    }

    #[test]
    fn relative_override_when_resolved_then_value_error() {
        let error = data_dir_from(Some("data".into()), Some(PathBuf::from("/home/u"))).unwrap_err();
        assert!(error.is_value_error());
        assert_eq!(error.to_string(), "RECOG_DATA_DIR must be an absolute path (or start with ~/)");
    }
}
