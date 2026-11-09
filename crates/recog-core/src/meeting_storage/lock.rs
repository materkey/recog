//! The storage's lock. Two processes rewrite the same files: the live one through
//! its storage and the web server (a generated title, a deleted meeting) through a
//! storage of each request. Every write is an atomic rename, so a reader never sees
//! half a file, but a read-modify-write that reads before another's rename and renames
//! after it drops that change. So each cycle runs under one exclusive `flock` of
//! `<meetings>/.lock`, taken with the storage's own mutex: once per operation, the
//! nested cycles of an operation (a meeting ended, its pause marker counted and its
//! index row written) run under the same acquisition, and never across AI work, which
//! runs before or after the operation.
//!
//! An `flock` belongs to an open file, not to a process: two storages of one process
//! exclude each other as two processes do, and a second acquisition in a thread that
//! holds one would wait for itself forever. So the guard is passed down instead, and
//! the JSON writes take it as proof: a write outside the lock does not compile. Readers
//! take no lock.

use std::fs;
use std::io;
use std::ops::{Deref, DerefMut};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{Inner, Result, StorageError};
use crate::paths;
use crate::pyjson::{self, Value};

/// The lock file in the meetings directory; not a directory, so no scan takes it for
/// a meeting.
const LOCK_FILE: &str = ".lock";

/// The storage's lock, held: the cross-process `flock`, released when the file
/// closes, and this storage's state.
pub(super) struct Locked<'a> {
    _file: fs::File,
    inner: MutexGuard<'a, Inner>,
}

impl<'a> Locked<'a> {
    /// The state, then the `flock`; the file is created on first use.
    pub(super) fn acquire(base_path: &Path, inner: &'a Mutex<Inner>) -> Result<Self> {
        let inner = inner.lock().unwrap_or_else(PoisonError::into_inner);
        let path = base_path.join(LOCK_FILE);
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| StorageError::os(&e, &path))?;
        file.lock().map_err(|e| StorageError::os(&e, &path))?;
        Ok(Self { _file: file, inner })
    }

    /// `_save_json`, under the lock.
    pub(super) fn save_json(&self, path: &Path, data: &Value) -> Result<()> {
        save_json(path, data)
    }
}

impl Deref for Locked<'_> {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }
}

/// `_save_json`: a unique temp file beside the target, created with the target's
/// mode (so a restrictive mode also protects the temp while it is written), renamed
/// over it.
fn save_json(path: &Path, data: &Value) -> Result<()> {
    let mode = match fs::metadata(path) {
        Ok(meta) => Some(meta.permissions().mode() & 0o7777),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(StorageError::os(&e, path)),
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = paths::create_unique(dir, &format!("{name}."), ".tmp", mode.unwrap_or(0o666))
        .map_err(|e| StorageError::os(&e, dir))?;
    paths::replace_with(path, temp, pyjson::dumps(data, Some(2)).as_bytes(), false, mode)
        .map_err(|e| StorageError::os(&e, path))
}

/// The writes of tests that set a stage, without a storage to lock.
#[cfg(test)]
pub(super) fn unlocked_save_json(path: &Path, data: &Value) -> Result<()> {
    save_json(path, data)
}
