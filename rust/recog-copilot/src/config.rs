//! The settings the supervisor reads, resolved once at startup.
//!
//! Stand-in until `recog-core` (being written in parallel) provides the installation
//! configuration (`recog/install_config.py`) and the data directory
//! (`recog/config.py`); then this module becomes a call into it. Until then it keeps
//! the Python precedence for the few values the supervisor needs: the process
//! environment, then the repository's `.env` (never overriding the environment), then
//! the `[copilot]` table of `config.toml`, then the defaults. Only `[copilot]` of that
//! file is read and checked; Python also refuses a malformed table elsewhere in it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::ai_settings::LegacyEnv;
use crate::log;

pub const DEFAULT_BRIDGE_URL: &str = "ws://127.0.0.1:8766";
pub const DEFAULT_MCP_URL: &str = "http://127.0.0.1:8767/mcp";
pub const DEFAULT_GRACE_S: f64 = 90.0;

/// Environment the transient unit needs but would not inherit (`systemd-run --user`
/// starts it from the user manager's environment, not the supervisor's).
pub const SESSION_ENV_KEYS: [&str; 5] = ["PATH", "HOME", "TERM", "SHELL", "LANG"];

/// The environment as Python sees it after `load_dotenv()`.
#[derive(Clone, Debug, Default)]
pub struct Env {
    vars: BTreeMap<String, String>,
    /// What `.env` added: Python puts it into `os.environ`, so every child process
    /// (tmux, and through it Claude) inherits it; the runner passes these on.
    pub from_dotenv: Vec<(String, String)>,
}

impl Env {
    pub fn from_process() -> Self {
        let vars = std::env::vars_os()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
            .collect();
        Env { vars, from_dotenv: Vec::new() }
    }

    pub fn from_pairs<K: Into<String>, V: Into<String>>(pairs: impl IntoIterator<Item = (K, V)>) -> Self {
        Env { vars: pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect(), from_dotenv: Vec::new() }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    /// `load_dotenv`: `KEY=value` lines, quotes stripped, never overriding a variable
    /// that is already set. Python reads it in text mode, so `\r` ends a line too.
    pub fn load_dotenv(&mut self, path: &Path) -> Result<(), String> {
        let text = match std::fs::read(path) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        for line in text.replace("\r\n", "\n").split(['\n', '\r']) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !key.is_empty() && !self.vars.contains_key(key) {
                self.vars.insert(key.to_owned(), value.to_owned());
                self.from_dotenv.push((key.to_owned(), value.to_owned()));
            }
        }
        Ok(())
    }

    /// A setting's raw value: unset when absent or blank, as `_environment_value`.
    fn setting(&self, key: &str) -> Option<&str> {
        self.get(key).map(str::trim).filter(|v| !v.is_empty())
    }

    /// `Path("~/x").expanduser()`.
    fn expand_user(&self, path: &str) -> PathBuf {
        match (path.strip_prefix('~'), self.get("HOME")) {
            (Some(""), Some(home)) => PathBuf::from(home),
            (Some(rest), Some(home)) if rest.starts_with('/') => PathBuf::from(format!("{home}{rest}")),
            _ => PathBuf::from(path),
        }
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
    pub from_dotenv: Vec<(String, String)>,
}

/// The installation's settings for a checkout at `repo_root`.
pub fn load(repo_root: &Path, mut env: Env) -> Result<Settings, String> {
    env.load_dotenv(&repo_root.join(".env"))?;
    let file = copilot_table(&env, repo_root)?;

    let autospawn = match env.setting("RECOG_COPILOT_AUTOSPAWN") {
        Some(raw) => env_flag(raw, "RECOG_COPILOT_AUTOSPAWN")?,
        None => file.autospawn.unwrap_or(true),
    };
    let grace_s = match env.setting("RECOG_COPILOT_GRACE_S") {
        Some(raw) => raw.parse::<f64>().map_err(|_| {
            format!("RECOG_COPILOT_GRACE_S: could not convert string to float: {}", crate::py::str_repr(raw))
        })?,
        None => file.grace_s.unwrap_or(DEFAULT_GRACE_S),
    };
    let mcp_url = match env.setting("RECOG_COPILOT_MCP_URL") {
        Some(raw) => raw.to_owned(),
        None => file.mcp_url.clone().unwrap_or_else(|| DEFAULT_MCP_URL.to_owned()),
    };
    // install_config reports a variable that silently replaces a different file value.
    let overridden = [
        ("RECOG_COPILOT_AUTOSPAWN", "copilot.autospawn", file.autospawn.is_some_and(|v| v != autospawn)),
        ("RECOG_COPILOT_GRACE_S", "copilot.grace_s", file.grace_s.is_some_and(|v| v != grace_s)),
        ("RECOG_COPILOT_MCP_URL", "copilot.mcp_url", file.mcp_url.as_ref().is_some_and(|v| *v != mcp_url)),
    ];
    for (variable, key, differs) in overridden {
        if differs && env.setting(variable).is_some() {
            log::line(&format!(
                "recog: {variable} from the environment or .env overrides {key} of config.toml; keep only one of them"
            ));
        }
    }

    let legacy = LegacyEnv {
        claude_model: env.get("CLAUDE_MODEL").unwrap_or("").to_owned(),
        codex_model: env.get("CODEX_MODEL").unwrap_or("").to_owned(),
        codex_reasoning_effort: env.get("CODEX_REASONING_EFFORT").unwrap_or("").to_owned(),
    };
    let session_env = SESSION_ENV_KEYS
        .iter()
        .filter_map(|key| env.get(key).filter(|v| !v.is_empty()).map(|v| ((*key).to_owned(), v.to_owned())))
        .collect();
    Ok(Settings {
        autospawn,
        grace_s,
        mcp_url,
        data_dir: data_dir(&env, repo_root)?,
        legacy,
        session_env,
        from_dotenv: env.from_dotenv.clone(),
    })
}

/// `_env_flag`.
fn env_flag(raw: &str, name: &str) -> Result<bool, String> {
    match raw.to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{name}: {name}={} is not one of 1/0, true/false, yes/no, on/off", crate::py::str_repr(raw))),
    }
}

#[derive(Debug, Default)]
struct CopilotTable {
    autospawn: Option<bool>,
    grace_s: Option<f64>,
    mcp_url: Option<String>,
}

/// `config_path()`: `RECOG_CONFIG_FILE` (relative to the checkout), else
/// `$XDG_CONFIG_HOME/recog/config.toml`, else `~/.config/recog/config.toml`.
fn config_path(env: &Env, repo_root: &Path) -> PathBuf {
    if let Some(configured) = env.get("RECOG_CONFIG_FILE").filter(|v| !v.is_empty()) {
        let path = env.expand_user(configured);
        return if path.is_absolute() { path } else { repo_root.join(path) };
    }
    let base = env.get("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).unwrap_or("~/.config");
    env.expand_user(base).join("recog").join("config.toml")
}

/// The `[copilot]` table, checked as `_file_section` checks it.
fn copilot_table(env: &Env, repo_root: &Path) -> Result<CopilotTable, String> {
    let path = config_path(env, repo_root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(CopilotTable::default()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let fail = |message: String| format!("{}: {message}", path.display());
    let document: toml::Table = text.parse().map_err(|e: toml::de::Error| fail(e.message().to_owned()))?;
    let Some(table) = document.get("copilot") else { return Ok(CopilotTable::default()) };
    let table = table
        .as_table()
        .filter(|t| t.keys().all(|k| ["autospawn", "grace_s", "mcp_url"].contains(&k.as_str())))
        .ok_or_else(|| fail("[copilot] must be a table with only autospawn, grace_s, mcp_url".to_owned()))?;
    let wrong = |key: &str| fail(format!("copilot.{key} has the wrong type"));
    let mut out = CopilotTable::default();
    if let Some(value) = table.get("autospawn") {
        out.autospawn = Some(value.as_bool().ok_or_else(|| wrong("autospawn"))?);
    }
    if let Some(value) = table.get("grace_s") {
        let number = match value {
            toml::Value::Integer(i) => *i as f64,
            toml::Value::Float(f) => *f,
            _ => return Err(wrong("grace_s")),
        };
        out.grace_s = Some(number);
    }
    if let Some(value) = table.get("mcp_url") {
        out.mcp_url = Some(value.as_str().ok_or_else(|| wrong("mcp_url"))?.to_owned());
    }
    Ok(out)
}

/// `get_data_dir()`: `RECOG_DATA_DIR`, else `~/.recog`, except that an installation
/// whose data still lives in the checkout keeps using it (with the same warning), and
/// data in both places is refused.
fn data_dir(env: &Env, repo_root: &Path) -> Result<PathBuf, String> {
    let configured = env.get("RECOG_DATA_DIR").map(str::trim).filter(|v| !v.is_empty());
    let target = match configured {
        Some(value) => env.expand_user(value),
        None => env.expand_user("~").join(".recog"),
    };
    if !target.is_absolute() {
        return Err("RECOG_DATA_DIR must be an absolute path (or start with ~/)".to_owned());
    }
    if configured.is_some() {
        return Ok(target);
    }
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if has_data(repo_root) && !same(repo_root, &target) {
        if has_data(&target) {
            return Err(format!(
                "Recog data exists in both {} and {}; resolve the conflict before starting services \
                 (scripts/migrate-data.py).",
                repo_root.display(),
                target.display()
            ));
        }
        log::line(&format!(
            "Recog: using legacy data in {}; run scripts/migrate-data.py to move it outside the checkout.",
            repo_root.display()
        ));
        return Ok(repo_root.to_path_buf());
    }
    Ok(target)
}

/// `data_entries(root)` is not empty.
fn has_data(root: &Path) -> bool {
    let fixed = ["meetings", "logs", "user-profile.json", "ai-settings.json", "privacy.json"];
    if fixed.iter().any(|name| root.join(name).symlink_metadata().is_ok()) {
        return true;
    }
    std::fs::read_dir(root).into_iter().flatten().flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with("speakers") && name.ends_with(".json")
    })
}
