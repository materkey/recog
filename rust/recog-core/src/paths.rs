//! What the ported modules take from `pathlib` and `os`: path normalisation as
//! `Path(text)` does it, `expanduser`, `resolve()` without `strict`, `mkdir` with a
//! mode on the last directory only, atomic writes, and `str(OSError)`.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use crate::py;

/// The process environment, or what a caller wants it to be.
pub type Environ = HashMap<String, String>;

/// `Path(text)`: repeated and trailing slashes and `.` components dropped, a leading
/// `//` kept, an empty path `.`.
#[must_use]
pub fn normalize(text: &str) -> PathBuf {
    let root = if text.starts_with("//") && !text.starts_with("///") {
        "//"
    } else if text.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts: Vec<&str> = text.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let joined = format!("{root}{}", parts.join("/"));
    PathBuf::from(if joined.is_empty() { ".".to_owned() } else { joined })
}

/// `Path(text).expanduser()` for `~` and `~/…`, the home from `HOME` in `environ`.
#[must_use]
pub fn expanduser(text: &str, environ: &Environ) -> PathBuf {
    let path = normalize(text);
    let text = path.to_string_lossy();
    let rest = if text == "~" {
        ""
    } else if let Some(rest) = text.strip_prefix("~/") {
        rest
    } else {
        return path;
    };
    let home = environ.get("HOME").map(PathBuf::from).or_else(std::env::home_dir).unwrap_or_else(|| "/".into());
    let home = home.to_string_lossy().trim_end_matches('/').to_owned();
    let home = if home.is_empty() { "/".to_owned() } else { home };
    if rest.is_empty() { normalize(&home) } else { normalize(&format!("{home}/{rest}")) }
}

/// `Path.resolve()`: symlinks resolved as far as the path exists, the rest appended.
#[must_use]
pub fn resolve(path: &Path) -> PathBuf {
    let absolute =
        if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut resolved = PathBuf::from("/");
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                let candidate = resolved.join(name);
                resolved = fs::canonicalize(&candidate).unwrap_or(candidate);
            }
        }
    }
    resolved
}

/// `path.mkdir(mode=mode, parents=True, exist_ok=True)`: missing parents get the
/// default mode, the directory itself `mode` (both under the umask).
///
/// # Errors
/// What creating a directory fails with, other than it existing.
pub fn mkdir_parents(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(mode).create(path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        other => other,
    }
}

/// A new file in `dir` named `prefix + random + suffix`, opened exclusively with
/// `mode`, as `tempfile.mkstemp` (mode 0600) or the storage's own temp files make
/// them.
///
/// # Errors
/// What creating the file fails with.
pub fn create_unique(dir: &Path, prefix: &str, suffix: &str, mode: u32) -> io::Result<(fs::File, PathBuf)> {
    loop {
        let name = format!("{prefix}{}{suffix}", uuid::Uuid::new_v4().simple());
        let path = dir.join(name);
        match fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
}

/// Write `contents` to a fresh temp file beside `target`, fsync it when asked,
/// optionally chmod it, and rename it over `target`; the temp never outlives a
/// failure.
///
/// # Errors
/// The first failing step.
pub fn replace_with(
    target: &Path,
    temp: (fs::File, PathBuf),
    contents: &[u8],
    fsync: bool,
    chmod: Option<u32>,
) -> io::Result<()> {
    let (mut file, temp_path) = temp;
    let result = (|| {
        file.write_all(contents)?;
        file.flush()?;
        if fsync {
            file.sync_all()?;
        }
        drop(file);
        if let Some(mode) = chmod {
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&temp_path, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

/// `str(OSError)` for an error about `path`: `[Errno 2] No such file or directory:
/// '/x'`.
#[must_use]
pub fn os_error_message(error: &io::Error, path: &Path) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let text = error.to_string();
            let strerror = text.strip_suffix(&format!(" (os error {code})")).unwrap_or(&text);
            format!("[Errno {code}] {strerror}: {}", py::repr(&path.to_string_lossy()))
        }
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_text_normalizes_like_pathlib() {
        // given / when / then
        assert_eq!(normalize("/a//b/./c/"), PathBuf::from("/a/b/c"));
        assert_eq!(normalize("//a"), PathBuf::from("//a"));
        assert_eq!(normalize("///a"), PathBuf::from("/a"));
        assert_eq!(normalize(""), PathBuf::from("."));
        assert_eq!(normalize("a/../b"), PathBuf::from("a/../b"));
    }

    #[test]
    fn a_tilde_expands_to_the_home_of_the_given_environment() {
        // given
        let environ: Environ = [("HOME".to_owned(), "/home/u/".to_owned())].into();
        // when / then
        assert_eq!(expanduser("~/x.toml", &environ), PathBuf::from("/home/u/x.toml"));
        assert_eq!(expanduser("~", &environ), PathBuf::from("/home/u"));
        assert_eq!(expanduser("~other/x", &environ), PathBuf::from("~other/x"));
        assert_eq!(expanduser("rel/x", &environ), PathBuf::from("rel/x"));
    }

    #[test]
    fn resolving_a_missing_tail_keeps_it_after_the_real_prefix() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(dir.path()).unwrap();
        // when / then
        assert_eq!(resolve(&dir.path().join("missing/../x")), real.join("x"));
    }
}
