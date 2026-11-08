//! Where an installation keeps its programs: `<repo>/bin/`, which `setup.sh` fills
//! from `rust/target/release/` under `[install] runtime = "rust"` and the service units
//! run. The directory sits in the checkout because the programs need it anyway (the
//! static UI, the prompts, the Swift helpers and `Recog.app`), and outside
//! `rust/target/`, which a cargo cache may clean while a service runs the binary.

use std::path::{Path, PathBuf};

/// The installation's program directory, relative to the checkout.
pub const BIN_DIR: &str = "bin";

/// The checkout an installed program at `exe` (a canonical path) belongs to:
/// `<repo>/bin/<name>`, where `<repo>` is the directory holding `recog-service.sh`.
#[must_use]
pub fn installed_repo_of(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    if bin.file_name()? != BIN_DIR {
        return None;
    }
    let repo = bin.parent()?;
    repo.join("recog-service.sh").is_file().then(|| repo.to_path_buf())
}

/// The checkout of the running program when it is installed in one's `bin/`.
#[must_use]
pub fn installed_repo() -> Option<PathBuf> {
    let exe = std::env::current_exe().and_then(std::fs::canonicalize).ok()?;
    installed_repo_of(&exe)
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The program `name` of the checkout at `repo_root`: the installed one in `bin/`
/// when it is executable, else the release build under `rust/target/release/`.
#[must_use]
pub fn program(repo_root: &Path, name: &str) -> PathBuf {
    let installed = repo_root.join(BIN_DIR).join(name);
    if is_executable(&installed) {
        return installed;
    }
    repo_root.join("rust").join("target").join("release").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn program_in_the_bin_of_a_checkout_when_resolving_then_that_checkout_is_its_repo() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(repo.join("recog-service.sh"), "").unwrap();
        // when / then
        assert_eq!(installed_repo_of(&repo.join("bin/recog")), Some(repo.clone()));
        assert_eq!(installed_repo_of(&repo.join("rust/target/release/recog")), None);
        assert_eq!(installed_repo_of(&repo.join("other/recog")), None);
    }

    #[test]
    fn bin_directory_outside_a_checkout_when_resolving_then_no_repo() {
        // given
        let dir = tempfile::tempdir().unwrap();
        // when / then
        assert_eq!(installed_repo_of(&dir.path().join("bin/recog")), None);
    }

    #[test]
    fn installed_and_built_program_when_looking_up_then_the_installed_one_wins() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let built = dir.path().join("rust/target/release/recog-mic");
        executable(&built);
        // when / then
        assert_eq!(program(dir.path(), "recog-mic"), built);
        let installed = dir.path().join("bin/recog-mic");
        executable(&installed);
        assert_eq!(program(dir.path(), "recog-mic"), installed);
    }
}
