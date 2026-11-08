//! The real build of `RecogZoomHelper.app` in a temporary checkout: swiftc and codesign
//! as `recog/zoom_app.py` runs them, then the same build by the Python module in the
//! same place. The two bundles must be identical file by file and carry the same code
//! directory hash, the value an ad-hoc signature and so the Accessibility grant rest
//! on. Nothing is launched, and the checkout's own bundle is only read.
//!
//! macOS with swiftc only; slow (two optimized Swift builds), so it runs when asked:
//! `cargo test -p recog-zoom --test real_build -- --ignored`.
#![forbid(unsafe_code)]
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;

use recog_zoom::helper_app::{HELPER_APP, HELPER_SOURCE, SystemToolchain, find_or_build_helper};
use recog_zoom::log::Logger;

fn repo_root() -> PathBuf {
    recog_zoom::run::default_repo_root()
}

fn python() -> PathBuf {
    std::env::var_os("RECOG_ZOOM_PARITY_PYTHON").filter(|v| !v.is_empty()).map_or_else(
        || {
            let venv = repo_root().join("venv-gigaam/bin/python");
            if venv.exists() { venv } else { PathBuf::from("python3") }
        },
        PathBuf::from,
    )
}

/// The `CDHash=` line of `codesign -dvvv`.
fn cdhash(app: &Path) -> String {
    let output = Command::new("codesign").arg("-dvvv").arg(app).output().unwrap();
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.lines().find_map(|line| line.strip_prefix("CDHash=")).unwrap_or_else(|| panic!("no CDHash: {text}")).to_owned()
}

fn files(app: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut stack = vec![app.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push((path.strip_prefix(app).unwrap().to_path_buf(), std::fs::read(&path).unwrap()));
            }
        }
    }
    found.sort();
    found
}

#[test]
#[ignore = "two optimized Swift builds; run with --ignored"]
fn given_the_helper_source_when_rust_and_python_build_it_then_the_bundles_and_their_signatures_are_identical() {
    let checkout = tempfile::tempdir().unwrap();
    let source = checkout.path().join(HELPER_SOURCE);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::copy(repo_root().join(HELPER_SOURCE), &source).unwrap();
    let (logger, log) = Logger::buffer();

    let app = find_or_build_helper(checkout.path(), "-", &SystemToolchain, &logger).unwrap();
    let rust_hash = cdhash(&app);
    let rust_files = files(&app);
    let built_by_rust = checkout.path().join("rust-built.app");
    std::fs::rename(&app, &built_by_rust).unwrap();
    std::fs::remove_file(checkout.path().join("RecogZoomHelper.source-sha256")).unwrap();

    let script = format!(
        "import sys; sys.path.insert(0, {root:?}); from pathlib import Path; from recog import zoom_app; \
         print(zoom_app.find_or_build_helper(Path({checkout:?})))",
        root = repo_root().display().to_string(),
        checkout = checkout.path().display().to_string(),
    );
    let output =
        Command::new(python()).arg("-I").arg("-c").arg(script).env_remove("RECOG_CODESIGN_IDENTITY").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    assert_eq!(cdhash(&app), rust_hash);
    assert_eq!(files(&app), rust_files);
    let logged = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("[zoom-app] Building RecogZoomHelper.app from"), "{logged}");
    // The bundle a checkout runs (`RECOG_ZOOM_INSTALLED_HELPER` names another one):
    // the same hash means a rebuild by either implementation keeps its grant.
    let installed =
        std::env::var_os("RECOG_ZOOM_INSTALLED_HELPER").map_or_else(|| repo_root().join(HELPER_APP), PathBuf::from);
    eprintln!("built here: CDHash {rust_hash}");
    if installed.exists() {
        eprintln!("installed {}: CDHash {}; built here: {rust_hash}", installed.display(), cdhash(&installed));
    }
}
