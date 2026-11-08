//! The settings the supervisor reads, resolved once at startup through `recog-core`:
//! the installation configuration (`recog/install_config.py`: the process environment,
//! then the repository's `.env` never overriding it, then `config.toml`, then the
//! defaults; a malformed value or file stops startup) and the data directory
//! (`recog/config.py`).

use std::path::Path;
use std::path::PathBuf;

use recog_core::{data_dir, install_config};

use crate::ai_settings::LegacyEnv;
use crate::log;

pub const DEFAULT_BRIDGE_URL: &str = "ws://127.0.0.1:8766";
pub const DEFAULT_MCP_URL: &str = "http://127.0.0.1:8767/mcp";
pub const DEFAULT_GRACE_S: f64 = 90.0;

/// Environment the transient unit needs but would not inherit (`systemd-run --user`
/// starts it from the user manager's environment, not the supervisor's).
pub const SESSION_ENV_KEYS: [&str; 5] = ["PATH", "HOME", "TERM", "SHELL", "LANG"];

/// The process environment the settings are read from.
#[derive(Clone, Debug, Default)]
pub struct Env {
    vars: install_config::Environ,
}

impl Env {
    pub fn from_process() -> Self {
        let vars = std::env::vars_os()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
            .collect();
        Env { vars }
    }

    pub fn from_pairs<K: Into<String>, V: Into<String>>(pairs: impl IntoIterator<Item = (K, V)>) -> Self {
        Env { vars: pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect() }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }
}

/// What the supervisor runs with.
#[derive(Clone, Debug)]
pub struct Settings {
    pub autospawn: bool,
    pub grace_s: f64,
    pub mcp_url: String,
    pub data_dir: PathBuf,
    pub legacy: LegacyEnv,
    /// `SESSION_ENV_KEYS` with a nonempty value, for `systemd-run --setenv`.
    pub session_env: Vec<(String, String)>,
    /// What `.env` added: Python puts it into `os.environ`, so every child process
    /// (tmux, and through it Claude) inherits it; the runner passes these on.
    pub from_dotenv: Vec<(String, String)>,
}

/// The installation's settings for a checkout at `repo_root`.
///
/// # Errors
/// A malformed setting, `config.toml` or data directory, as Python's import raises it.
pub fn load(repo_root: &Path, env: Env) -> Result<Settings, String> {
    let mut environ = env.vars.clone();
    let mut warn = |notice: String| log::line(&notice);
    let config = install_config::load(repo_root, &mut environ, &mut warn)?;
    let data_dir = data_dir::get_data_dir(repo_root, &environ, &mut warn).map_err(|e| e.to_string())?;
    let mut from_dotenv: Vec<(String, String)> =
        environ.iter().filter(|(k, _)| !env.vars.contains_key(*k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    from_dotenv.sort();
    let get = |key: &str| environ.get(key).map_or("", String::as_str).to_owned();
    let legacy = LegacyEnv {
        claude_model: get("CLAUDE_MODEL"),
        codex_model: get("CODEX_MODEL"),
        codex_reasoning_effort: get("CODEX_REASONING_EFFORT"),
    };
    let session_env = SESSION_ENV_KEYS
        .iter()
        .filter_map(|key| environ.get(*key).filter(|v| !v.is_empty()).map(|v| ((*key).to_owned(), v.clone())))
        .collect();
    Ok(Settings {
        autospawn: config.copilot.autospawn,
        grace_s: config.copilot.grace_s,
        mcp_url: config.copilot.mcp_url,
        data_dir,
        legacy,
        session_env,
        from_dotenv,
    })
}
