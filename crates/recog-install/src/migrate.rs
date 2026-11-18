//! Relocate legacy data without merging directories or copying private files.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use recog_core::{data_dir, paths};
use rustix::fs::{CWD, RenameFlags, renameat_with};

/// Check every entry before the first rename. Each rename also refuses a
/// destination created after preflight. A retry finds only the remaining entries.
pub fn migrate(repo: &Path, target: &Path, apply: bool) -> io::Result<Vec<(PathBuf, PathBuf)>> {
    let repo = fs::canonicalize(repo)?;
    let target = paths::resolve(target);
    if target.starts_with(&repo) || repo.starts_with(&target) {
        return Err(io::Error::other("Data directory must be outside and separate from the checkout"));
    }
    let parent =
        target.ancestors().find(|p| p.exists()).ok_or_else(|| io::Error::other("No existing target ancestor"))?;
    let meta = fs::metadata(parent)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!("Not a directory: {}", parent.display())));
    }
    let target_device = meta.dev();
    let mut moves = Vec::new();
    for source in data_dir::data_entries(&repo) {
        let meta = fs::symlink_metadata(&source)?;
        if meta.is_symlink() {
            return Err(io::Error::other(format!(
                "Resolve the legacy symlink manually before migration: {}",
                source.display()
            )));
        }
        let destination = target.join(source.file_name().expect("data entry within checkout"));
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("Destination already exists: {}", destination.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if meta.dev() != target_device {
            return Err(io::Error::other(format!("Migration requires the same filesystem: {}", source.display())));
        }
        moves.push((source, destination));
    }
    if apply && !moves.is_empty() {
        paths::mkdir_parents(&target, 0o700)?;
        for (source, dest) in &moves {
            renameat_with(CWD, source, CWD, dest, RenameFlags::NOREPLACE)?;
        }
    }
    Ok(moves)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_migration_when_retried_then_preserves_inodes_and_moves_remaining_entries() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let target = dir.path().join("home/data");
        fs::create_dir_all(repo.join("meetings")).unwrap();
        fs::write(repo.join("privacy.json"), "private").unwrap();
        let inode = fs::metadata(repo.join("privacy.json")).unwrap().ino();
        assert_eq!(migrate(&repo, &target, false).unwrap().len(), 2);
        assert!(!target.exists());
        fs::create_dir_all(&target).unwrap();
        fs::rename(repo.join("meetings"), target.join("meetings")).unwrap();
        assert_eq!(migrate(&repo, &target, true).unwrap().len(), 1);
        assert_eq!(fs::metadata(target.join("privacy.json")).unwrap().ino(), inode);
        assert!(migrate(&repo, &target, true).unwrap().is_empty());
    }

    #[test]
    fn collision_on_last_entry_when_migrating_then_first_entry_stays_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let target = dir.path().join("data");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(repo.join("meetings")).unwrap();
        fs::write(repo.join("speakers.json"), "old").unwrap();
        std::os::unix::fs::symlink("missing", target.join("speakers.json")).unwrap();
        assert_eq!(migrate(&repo, &target, true).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert!(repo.join("meetings").is_dir());
        assert!(migrate(&repo, &repo.join("nested"), true).is_err());
    }
}
