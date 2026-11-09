//! `recog/desktop/storage.py`: private installation state and immutable extension
//! snapshots.
//!
//! Files are written as `json.dump(value, f, ensure_ascii=False)` writes them, through
//! a `mkstemp` file (mode 0600) beside the target, fsynced and renamed over it; the
//! directory is created with mode 0700 (its missing parents with the default mode).

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use recog_core::paths;
use recog_core::pyjson::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::py::{self, PyErr};

/// `read_json(path, default)`: `{}` (or `default`) for a missing file.
///
/// # Errors
/// What reading or decoding raises (`OSError`, `UnicodeDecodeError`, `JSONDecodeError`).
pub fn read_json(path: &Path, default: Option<Value>) -> Result<Value, PyErr> {
    match fs::read(path) {
        Ok(raw) => py::loads_text(&raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default.unwrap_or_else(|| Value::Object(Map::new()))),
        Err(e) => Err(PyErr::os(&e, Some(path))),
    }
}

/// [`read_json`] with `{}` for a missing file.
///
/// # Errors
/// As [`read_json`].
pub fn read(path: &Path) -> Result<Value, PyErr> {
    read_json(path, None)
}

/// `write_json(path, value)`.
///
/// # Errors
/// The `OSError` of the failing step.
pub fn write_json(path: &Path, value: &Value) -> Result<(), PyErr> {
    let parent = path.parent().unwrap_or(Path::new("."));
    paths::mkdir_parents(parent, 0o700).map_err(|e| PyErr::os(&e, Some(parent)))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mut file, temporary) =
        paths::create_unique(parent, &format!("{name}."), "", 0o600).map_err(|e| PyErr::os(&e, Some(parent)))?;
    let result = (|| {
        file.write_all(py::dumps(value).as_bytes()).map_err(|e| PyErr::os(&e, None))?;
        file.flush().map_err(|e| PyErr::os(&e, None))?;
        file.sync_all().map_err(|e| PyErr::os(&e, None))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| {
            let error = PyErr::os(&e, Some(&temporary));
            // os.replace names both paths: "[Errno N] text: 'a' -> 'b'".
            PyErr::new(error.kind, format!("{} -> {}", error.message, recog_core::py::repr(&path.to_string_lossy())))
        })
    })();
    let _ = fs::remove_file(&temporary);
    result
}

/// `write_status(root, result)`: an unresolved inspector warning stays until closure
/// is established. Returns what `status.json` holds afterwards.
///
/// # Errors
/// What reading the current files or writing the new one raises.
pub fn write_status(root: &Path, result: &Value) -> Result<Value, PyErr> {
    let existing = read(&root.join("status.json"))?;
    if replaces_unclosed_warning(&existing, result)? && !operation_closed(root)? {
        return Ok(existing);
    }
    write_json(&root.join("status.json"), result)?;
    Ok(result.clone())
}

/// `existing` warns of an unclosed inspector and `result` neither does nor
/// establishes closure.
fn replaces_unclosed_warning(existing: &Value, result: &Value) -> Result<bool, PyErr> {
    Ok(py::get(existing, "state")?.and_then(Value::as_str) == Some("inspector_unclosed")
        && py::get(result, "state")?.and_then(Value::as_str) != Some("inspector_unclosed")
        && py::get(result, "inspector_closed")? != Some(&Value::Bool(true)))
}

/// `operation.json` records its operation closed.
fn operation_closed(root: &Path) -> Result<bool, PyErr> {
    let operation = read(&root.join("operation.json"))?;
    Ok(py::get(&operation, "phase")?.and_then(Value::as_str) == Some("closed"))
}

fn symlink_error() -> PyErr {
    PyErr::value("Extension snapshot cannot contain symbolic links")
}

/// The entries under `directory` as `sorted(directory.rglob("*"))` lists them: every
/// file, directory and link below it, ordered by their path components.
fn walk(directory: &Path) -> Result<Vec<(Vec<String>, PathBuf)>, PyErr> {
    fn visit(dir: &Path, prefix: &[String], out: &mut Vec<(Vec<String>, PathBuf)>) -> Result<(), PyErr> {
        // pathlib skips a directory it may not list.
        let Ok(entries) = fs::read_dir(dir) else { return Ok(()) };
        for entry in entries.flatten() {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|n| PyErr::new("UnicodeEncodeError", format!("cannot encode file name {n:?}")))?;
            let mut parts = prefix.to_vec();
            parts.push(name);
            let path = entry.path();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            out.push((parts.clone(), path.clone()));
            if is_dir {
                visit(&path, &parts, out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    visit(directory, &[], &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// `bundle_digest(directory)`: sha256 over every file's relative path, a NUL and its
/// bytes, in sorted order; a symbolic link anywhere is refused.
///
/// # Errors
/// `ValueError` for a symbolic link, `OSError` for a file that cannot be read.
pub fn bundle_digest(directory: &Path) -> Result<String, PyErr> {
    let mut digest = Sha256::new();
    for (parts, path) in walk(directory)? {
        let meta = fs::symlink_metadata(&path).map_err(|e| PyErr::os(&e, Some(&path)))?;
        if meta.file_type().is_symlink() {
            return Err(symlink_error());
        }
        if meta.is_file() {
            digest.update(parts.join("/").as_bytes());
            digest.update(b"\0");
            digest.update(&fs::read(&path).map_err(|e| PyErr::os(&e, Some(&path)))?);
        }
    }
    Ok(digest.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn copy_times(from: &fs::Metadata, to: &Path) {
    let times = fs::FileTimes::new();
    let times = match from.accessed() {
        Ok(t) => times.set_accessed(t),
        Err(_) => times,
    };
    let times = match from.modified() {
        Ok(t) => times.set_modified(t),
        Err(_) => times,
    };
    if let Ok(file) = fs::File::open(to) {
        let _ = file.set_times(times);
    }
}

/// `shutil.copytree(source, target, dirs_exist_ok=True)`: files with their contents,
/// permission bits and times (`copy2`), directories with theirs once filled.
fn copy_tree(source: &Path, target: &Path) -> Result<(), PyErr> {
    fs::create_dir_all(target).map_err(|e| PyErr::os(&e, Some(target)))?;
    let entries = fs::read_dir(source).map_err(|e| PyErr::os(&e, Some(source)))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = target.join(entry.file_name());
        let meta = fs::metadata(&from).map_err(|e| PyErr::os(&e, Some(&from)))?;
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| PyErr::os(&e, Some(&from)))?;
            copy_times(&meta, &to);
        }
    }
    let meta = fs::metadata(source).map_err(|e| PyErr::os(&e, Some(source)))?;
    fs::set_permissions(target, fs::Permissions::from_mode(meta.mode() & 0o7777))
        .map_err(|e| PyErr::os(&e, Some(target)))?;
    copy_times(&meta, target);
    Ok(())
}

/// `tempfile.mkdtemp(prefix=prefix, dir=parent)`: a new directory, mode 0700.
fn mkdtemp(parent: &Path, prefix: &str) -> Result<PathBuf, PyErr> {
    use std::os::unix::fs::DirBuilderExt as _;
    loop {
        let path = parent.join(format!("{prefix}{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(PyErr::os(&e, Some(&path))),
        }
    }
}

/// `prepare_bundle(source, root)`: the snapshot `<root>/extensions/<digest>` of a
/// built extension, taken once per build and checked against its digest.
///
/// # Errors
/// `ValueError` for an unbuilt extension, a build changing meanwhile or a damaged
/// snapshot; `OSError` for the copy.
pub fn prepare_bundle(source: &Path, root: &Path) -> Result<(String, PathBuf), PyErr> {
    let built = ["manifest.json", "injected.js", "src/audio-worklet/downsampler.js"];
    if !built.iter().all(|name| source.join(name).is_file()) {
        return Err(PyErr::value("Build browser-extension before enabling desktop integration"));
    }
    let digest = bundle_digest(source)?;
    let parent = root.join("extensions");
    paths::mkdir_parents(&parent, 0o700).map_err(|e| PyErr::os(&e, Some(&parent)))?;
    let target = parent.join(&digest);
    if !target.exists() {
        snapshot(source, &parent, &target, &digest)?;
    }
    if bundle_digest(&target)? != digest {
        return Err(PyErr::value("Extension snapshot integrity check failed"));
    }
    Ok((digest, target))
}

/// The copy of `source` staged under `parent` and renamed to `target` once it has
/// `digest`; the stage is removed whatever happens.
fn snapshot(source: &Path, parent: &Path, target: &Path, digest: &str) -> Result<(), PyErr> {
    let temporary = mkdtemp(parent, "stage-")?;
    let result = (|| {
        copy_tree(source, &temporary)?;
        if bundle_digest(&temporary)? != digest {
            return Err(PyErr::value("Extension build changed while taking snapshot"));
        }
        fs::rename(&temporary, target).map_err(|e| PyErr::os(&e, Some(&temporary)))
    })();
    if temporary.exists() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

/// `config.get("bundle", "")`, as `len()` takes it.
fn configured_digest(config: &Value) -> Result<&str, PyErr> {
    match py::get(config, "bundle")? {
        None => Ok(""),
        Some(Value::Str(s)) => Ok(s.as_str()),
        Some(other) => Err(PyErr::type_error(format!("object of type '{}' has no len()", other.type_name()))),
    }
}

/// `validate_bundle(root, config)`: the snapshot `config["bundle"]` names, intact.
///
/// # Errors
/// `ValueError` for a malformed identifier or a damaged snapshot, the Python error of
/// a malformed configuration.
pub fn validate_bundle(root: &Path, config: &Value) -> Result<PathBuf, PyErr> {
    let digest = configured_digest(config)?;
    if digest.chars().count() != 64 || digest.chars().any(|c| !"0123456789abcdef".contains(c)) {
        return Err(PyErr::value("Invalid extension snapshot identifier"));
    }
    let path = root.join("extensions").join(digest);
    let intact = path.is_dir()
        && !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
        && bundle_digest(&path)? == digest;
    if !intact {
        return Err(PyErr::value("Extension snapshot integrity check failed"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_changing_during_the_copy_when_snapshotted_then_refused_and_stage_removed() {
        // given: a digest the copy cannot have, as when the build changed meanwhile
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("dist");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("injected.js"), "built").unwrap();
        let parent = dir.path().join("extensions");
        fs::create_dir_all(&parent).unwrap();
        let target = parent.join("digest");
        // when
        let error = snapshot(&source, &parent, &target, "digest").unwrap_err();
        // then
        assert_eq!(error.described(), "ValueError: Extension build changed while taking snapshot");
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
        snapshot(&source, &parent, &target, &bundle_digest(&source).unwrap()).unwrap();
        assert_eq!(fs::read_to_string(target.join("injected.js")).unwrap(), "built");
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
    }
}
