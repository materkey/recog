//! The shared data root — the port of `get_data_dir()` in `recog/config.py`, which
//! `scripts/data-dir.sh` prints for the shell scripts.
//!
//! `~/.recog`, or the absolute (or `~/…`) `RECOG_DATA_DIR`. Without an explicit
//! override an installation whose data still lives in the checkout keeps the whole
//! root there, with a warning; data in both roots is a fatal conflict until
//! `scripts/migrate-data.py` resolves it.
//!
//! The environment is the one after `.env` is loaded (`install_config::load`, or
//! [`get_data_dir_process`]): Python imports `install_config` before resolving, so a
//! `RECOG_DATA_DIR` in `.env` counts there too. Known difference: Python's import also
//! fails on a malformed `config.toml`, which this resolution does not read.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::install_config::Environ;
use crate::paths;
use crate::py;

/// Why no data root can be chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataDirError {
    /// Python's `ValueError`: `RECOG_DATA_DIR` is relative.
    Relative(String),
    /// Python's `RuntimeError`: data in both roots.
    Conflict(String),
}

impl fmt::Display for DataDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Relative(message) | Self::Conflict(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DataDirError {}

fn exists_or_symlink(path: &Path) -> bool {
    path.exists() || path.is_symlink()
}

/// `data_entries(root)`: the movable data, old speaker databases included, without
/// `.env`.
#[must_use]
pub fn data_entries(root: &Path) -> Vec<PathBuf> {
    let mut speakers: Vec<PathBuf> = fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("speakers") && name.ends_with(".json") && name.len() >= 13)
                .map(|name| root.join(name))
                .collect()
        })
        .unwrap_or_default();
    speakers.sort();
    ["meetings", "logs", "user-profile.json", "ai-settings.json", "privacy.json"]
        .iter()
        .map(|name| root.join(name))
        .chain(speakers)
        .filter(|path| exists_or_symlink(path))
        .collect()
}

/// `data_dir_target()`: the new root, the same for the CLI, setup and services
/// whatever the working directory.
///
/// # Errors
/// A relative `RECOG_DATA_DIR`.
pub fn data_dir_target(environ: &Environ) -> Result<PathBuf, DataDirError> {
    let value = py::strip(environ.get("RECOG_DATA_DIR").map_or("", String::as_str));
    let path = if value.is_empty() {
        paths::expanduser("~", environ).join(".recog")
    } else {
        paths::expanduser(value, environ)
    };
    if !path.is_absolute() {
        return Err(DataDirError::Relative("RECOG_DATA_DIR must be an absolute path (or start with ~/)".into()));
    }
    Ok(path)
}

/// `get_data_dir(repo_dir)`: only an explicit migration or the override moves an old
/// installation. The legacy warning goes to `warn`.
///
/// # Errors
/// A relative override, or data in both the checkout and the new root.
pub fn get_data_dir(repo_dir: &Path, environ: &Environ, warn: &mut dyn FnMut(String)) -> Result<PathBuf, DataDirError> {
    let target = data_dir_target(environ)?;
    if !py::strip(environ.get("RECOG_DATA_DIR").map_or("", String::as_str)).is_empty() {
        return Ok(target);
    }
    let legacy = data_entries(repo_dir);
    if !legacy.is_empty() && paths::resolve(repo_dir) != paths::resolve(&target) {
        if !data_entries(&target).is_empty() {
            return Err(DataDirError::Conflict(format!(
                "Recog data exists in both {} and {}; resolve the conflict before starting services \
                 (scripts/migrate-data.py).",
                repo_dir.display(),
                target.display()
            )));
        }
        warn(format!(
            "Recog: using legacy data in {}; run scripts/migrate-data.py to move it outside the checkout.",
            repo_dir.display()
        ));
        return Ok(repo_dir.to_path_buf());
    }
    Ok(target)
}

/// What `scripts/data-dir.sh` prints: [`get_data_dir`] over the process environment
/// with the repository's `.env` filling unset variables, as importing `recog.config`
/// does in Python (a `RECOG_DATA_DIR` in `.env` counts). The warning goes to stderr.
///
/// # Errors
/// As [`get_data_dir`].
pub fn get_data_dir_process(repo_dir: &Path) -> Result<PathBuf, DataDirError> {
    let mut environ: Environ = std::env::vars().collect();
    crate::install_config::load_dotenv(&repo_dir.join(".env"), &mut environ);
    get_data_dir(repo_dir, &environ, &mut |warning| eprintln!("{warning}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> (tempfile::TempDir, PathBuf, Environ) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let home = dir.path().join("home");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&home).unwrap();
        let environ: Environ = [("HOME".to_owned(), home.to_string_lossy().into_owned())].into();
        (dir, repo, environ)
    }

    fn quiet() -> impl FnMut(String) {
        |_| {}
    }

    #[test]
    fn fresh_checkout_when_resolving_then_data_is_outside_repo() {
        // given
        let (_dir, repo, environ) = roots();
        // when
        let root = get_data_dir(&repo, &environ, &mut quiet()).unwrap();
        // then
        assert_eq!(root, PathBuf::from(&environ["HOME"]).join(".recog"));
    }

    #[test]
    fn explicit_override_when_resolving_then_home_expands() {
        // given
        let (_dir, repo, mut environ) = roots();
        environ.insert("RECOG_DATA_DIR".into(), " ~/data ".into());
        // when / then
        assert_eq!(get_data_dir(&repo, &environ, &mut quiet()).unwrap(), PathBuf::from(&environ["HOME"]).join("data"));
    }

    #[test]
    fn relative_override_when_resolving_then_rejected() {
        // given
        let (_dir, repo, mut environ) = roots();
        environ.insert("RECOG_DATA_DIR".into(), "data".into());
        // when / then
        assert!(matches!(get_data_dir(&repo, &environ, &mut quiet()), Err(DataDirError::Relative(_))));
    }

    #[test]
    fn legacy_data_when_resolving_then_whole_root_stays_with_warning() {
        for entry in ["meetings", "logs", "user-profile.json", "ai-settings.json", "privacy.json", "speakers_old.json"]
        {
            // given
            let (_dir, repo, environ) = roots();
            if entry.ends_with(".json") {
                fs::write(repo.join(entry), "{}").unwrap();
            } else {
                fs::create_dir(repo.join(entry)).unwrap();
            }
            let mut warnings = Vec::new();
            // when
            let root = get_data_dir(&repo, &environ, &mut |w| warnings.push(w)).unwrap();
            // then
            assert_eq!(root, repo, "{entry}");
            assert!(warnings[0].contains("migrate-data.py"), "{entry}");
        }
    }

    #[test]
    fn two_populated_roots_when_resolving_then_conflict_is_visible() {
        // given
        let (_dir, repo, environ) = roots();
        fs::create_dir(repo.join("meetings")).unwrap();
        let target = PathBuf::from(&environ["HOME"]).join(".recog");
        fs::create_dir_all(target.join("logs")).unwrap();
        // when
        let error = get_data_dir(&repo, &environ, &mut quiet()).unwrap_err();
        // then
        assert!(matches!(&error, DataDirError::Conflict(m) if m.contains("both")));
    }

    #[test]
    fn leftover_settings_lock_when_resolving_then_not_mistaken_for_legacy_data() {
        // given
        let (_dir, repo, environ) = roots();
        fs::write(repo.join("ai-settings.json.lock"), "").unwrap();
        fs::write(repo.join("speakers.txt"), "").unwrap();
        // when / then
        assert_eq!(
            get_data_dir(&repo, &environ, &mut quiet()).unwrap(),
            PathBuf::from(&environ["HOME"]).join(".recog")
        );
    }

    #[test]
    fn a_broken_symlink_counts_as_legacy_data() {
        // given
        let (_dir, repo, environ) = roots();
        std::os::unix::fs::symlink(repo.join("nowhere"), repo.join("logs")).unwrap();
        // when / then
        assert_eq!(data_entries(&repo), vec![repo.join("logs")]);
        assert_eq!(get_data_dir(&repo, &environ, &mut quiet()).unwrap(), repo);
    }
}
