//! The `recog` binary installed into a checkout's `bin/`, as `setup.sh` does under
//! `[install] runtime = "rust"`: it takes that checkout as its own (its `.env`, its
//! legacy data root) and answers the shell scripts' questions there.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A checkout with `recog` in its `bin/`: a hard link, else a copy, since a symlink
/// would resolve back to the build directory.
fn installed(repo: &Path) -> PathBuf {
    std::fs::write(repo.join("recog-service.sh"), "#!/bin/bash\n").unwrap();
    let bin = repo.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = bin.join("recog");
    let built = Path::new(env!("CARGO_BIN_EXE_recog"));
    if std::fs::hard_link(built, &program).is_err() {
        std::fs::copy(built, &program).unwrap();
    }
    program
}

fn run(program: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let output = Command::new(program)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("RECOG_CONFIG_FILE", home.join("none.toml"))
        .current_dir(home)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn installed_binary_with_legacy_data_in_its_checkout_when_asked_then_that_checkout_holds_the_data() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap().join("checkout");
    let home = repo.parent().unwrap().join("home");
    std::fs::create_dir_all(repo.join("meetings")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let program = installed(&repo);
    // when
    let (stdout, stderr, code) = run(&program, &home, &["config", "data-dir"]);
    // then
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, format!("{}\n", repo.display()));
    assert!(stderr.contains("using legacy data"), "{stderr}");
}

#[test]
fn installed_binary_with_a_dotenv_in_its_checkout_when_asked_then_the_dotenv_counts() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap().join("checkout");
    let home = repo.parent().unwrap().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(repo.join(".env"), "RECOG_INSTALL_RUNTIME=rust\nRECOG_ZOOM_APP=maybe\n").unwrap();
    let program = installed(&repo);
    // when
    let (stdout, _, code) = run(&program, &home, &["check"]);
    std::fs::write(repo.join(".env"), "RECOG_INSTALL_RUNTIME=rust\n").unwrap();
    let runtime = run(&program, &home, &["config", "install-runtime"]);
    let checked = run(&program, &home, &["check"]);
    // then
    assert_eq!(code, 1);
    assert!(stdout.starts_with("Invalid installation configuration: RECOG_ZOOM_APP"), "{stdout}");
    assert_eq!(runtime.0, "rust\n");
    assert_eq!((checked.0.as_str(), checked.2), ("", 0), "{}", checked.1);
}

#[test]
fn installed_binary_with_the_remote_backend_and_no_gateway_when_checked_then_it_blocks_as_the_python_probe() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap().join("checkout");
    let home = repo.parent().unwrap().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let remote = "RECOG_INSTALL_RUNTIME=rust\nRECOG_ASR_BACKEND=remote\n";
    std::fs::write(repo.join(".env"), remote).unwrap();
    let program = installed(&repo);
    // when
    let (stdout, _, code) = run(&program, &home, &["check"]);
    std::fs::write(repo.join(".env"), format!("{remote}RECOG_ASR_REMOTE_URL=http://gw\n")).unwrap();
    let with_url = run(&program, &home, &["check"]);
    // then
    assert_eq!(code, 1);
    assert!(stdout.starts_with("RECOG_ASR_BACKEND=remote but RECOG_ASR_REMOTE_URL is empty"), "{stdout}");
    assert_eq!((with_url.0.as_str(), with_url.2), ("", 0), "{}", with_url.1);
}
