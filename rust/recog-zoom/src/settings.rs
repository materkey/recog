//! The one installation setting this source reads: the signing identity of the helper.
//!
//! Resolved as `recog/install_config.py` resolves `[macos] codesign_identity`: the
//! process environment, then the checkout's `.env` (which never overrides it), then
//! `config.toml`; empty signs ad hoc. Only that key of `config.toml` is read here: the
//! whole file is validated by the live process, which is what runs this source today.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const CODESIGN_IDENTITY_VARIABLE: &str = "RECOG_CODESIGN_IDENTITY";
/// `codesign --sign -`: an ad-hoc signature, the hash of the bundle.
pub const AD_HOC: &str = "-";

/// The pairs `load_dotenv` of `recog/install_config.py` takes from a `.env` file: the
/// first value of each key, quotes stripped.
pub fn read_dotenv(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = crate::py::strip(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = crate::py::strip(key);
        let value = crate::py::strip(value).trim_matches('"').trim_matches('\'');
        if !key.is_empty() {
            values.entry(key.to_owned()).or_insert_with(|| value.to_owned());
        }
    }
    values
}

fn expand_user(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), std::env::home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

/// `config_path()`: `RECOG_CONFIG_FILE` (relative to the checkout), else
/// `$XDG_CONFIG_HOME/recog/config.toml`, else `~/.config/recog/config.toml`.
pub fn config_path(repo_root: &Path, environment: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(configured) = environment("RECOG_CONFIG_FILE").filter(|value| !value.is_empty()) {
        let path = expand_user(&configured);
        return if path.is_absolute() { path } else { repo_root.join(path) };
    }
    let base = environment("XDG_CONFIG_HOME").filter(|value| !value.is_empty()).unwrap_or_else(|| "~/.config".into());
    expand_user(&base).join("recog").join("config.toml")
}

/// The identity to sign the helper with, `"-"` for ad hoc.
pub fn codesign_identity(repo_root: &Path, environment: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let from_environment = match environment(CODESIGN_IDENTITY_VARIABLE) {
        Some(value) => Some(value),
        None => std::fs::read_to_string(repo_root.join(".env"))
            .ok()
            .and_then(|text| read_dotenv(&text).remove(CODESIGN_IDENTITY_VARIABLE)),
    };
    if let Some(value) = from_environment.as_deref().map(crate::py::strip).filter(|value| !value.is_empty()) {
        return Ok(value.to_owned());
    }
    let path = config_path(repo_root, environment);
    let identity = match std::fs::read_to_string(&path) {
        Err(_) => String::new(),
        Ok(text) => {
            let document: toml::Table = text.parse().map_err(|error| format!("{}: {error}", path.display()))?;
            match document.get("macos").and_then(|macos| macos.get("codesign_identity")) {
                None => String::new(),
                Some(toml::Value::String(identity)) => identity.clone(),
                Some(_) => return Err(format!("{}: macos.codesign_identity must be a string", path.display())),
            }
        }
    };
    Ok(if identity.is_empty() { AD_HOC.into() } else { identity })
}

/// `codesign_identity` from the process environment.
pub fn codesign_identity_from_env(repo_root: &Path) -> Result<String, String> {
    codesign_identity(repo_root, &|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_dotenv_lines_when_read_then_first_value_wins_and_quotes_go() {
        let values = read_dotenv("# comment\n\nA = \"x y\"\nA=second\nB='z'\nnoequals\n=v\n");
        assert_eq!(values.get("A").map(String::as_str), Some("x y"));
        assert_eq!(values.get("B").map(String::as_str), Some("z"));
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn given_identity_in_environment_dotenv_and_config_when_resolved_then_environment_then_dotenv_then_config() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "[macos]\ncodesign_identity = \"From File\"\n").unwrap();
        let file = config.display().to_string();
        let lookup = |set: Option<&'static str>| {
            let file = file.clone();
            move |name: &str| match name {
                "RECOG_CONFIG_FILE" => Some(file.clone()),
                CODESIGN_IDENTITY_VARIABLE => set.map(String::from),
                _ => None,
            }
        };
        assert_eq!(codesign_identity(root.path(), &lookup(None)).unwrap(), "From File");
        std::fs::write(root.path().join(".env"), "RECOG_CODESIGN_IDENTITY=From Dotenv\n").unwrap();
        assert_eq!(codesign_identity(root.path(), &lookup(None)).unwrap(), "From Dotenv");
        assert_eq!(codesign_identity(root.path(), &lookup(Some(" From Env "))).unwrap(), "From Env");
        // Set but empty: .env does not override it, and the file decides.
        assert_eq!(codesign_identity(root.path(), &lookup(Some(""))).unwrap(), "From File");
    }

    #[test]
    fn given_nothing_configured_when_resolved_then_ad_hoc() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("absent.toml").display().to_string();
        let lookup = move |name: &str| (name == "RECOG_CONFIG_FILE").then(|| missing.clone());
        assert_eq!(codesign_identity(root.path(), &lookup).unwrap(), AD_HOC);
    }
}
