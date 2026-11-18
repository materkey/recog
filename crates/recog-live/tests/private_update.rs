//! Exercise the installed updater without depending on a real account in CI.
mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn private_release_when_updating_then_only_verified_bytes_reach_setup() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("home/.local/share/recog/runtime");
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("recog-release"), "v1.0.0").unwrap();
    common::place_recog(&root.join("bin/recog"));
    let gh = dir.path().join("gh");
    fs::write(
        &gh,
        r#"#!/bin/sh
case "$*" in
 "release view --json tagName --jq .tagName --repo github.com/materkey/recog") echo "$LATEST";;
 "release download v1.0.1 --pattern SHA256SUMS --output - --repo github.com/materkey/recog")
    printf '%064d  %s\n' 0 "$ASSET";;
 "release download v1.0.1 --pattern $ASSET --output - --repo github.com/materkey/recog") echo corrupt;;
 *) exit 9;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |tag: &str| {
        Command::new(root.join("bin/recog"))
            .arg("update")
            .env_clear()
            .env("PATH", dir.path())
            .env("HOME", dir.path().join("home"))
            .env("LATEST", tag)
            .env(
                "ASSET",
                format!(
                    "recog-v1.0.1-{}.tar.gz",
                    if cfg!(target_os = "macos") { "macos-arm64" } else { "linux-x86_64" }
                ),
            )
            .output()
            .unwrap()
    };
    let current = run("v1.0.0");
    assert!(current.status.success(), "{}", String::from_utf8_lossy(&current.stderr));
    let corrupt = run("v1.0.1");
    assert!(!corrupt.status.success());
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("checksum mismatch"));
    assert_eq!(fs::read_to_string(root.join("recog-release")).unwrap(), "v1.0.0");
}
