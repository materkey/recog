//! A real freshly built CLI and stub Cargo, always under a temporary installation.
mod common;
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::Path, process::Command};
fn write_exec(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn given_release_sources_when_packed_then_both_apps_target_ventura_and_use_ad_hoc_signatures() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (project, file) in [("recog-live", "recog_launcher.swift"), ("recog-zoom", "zoom_ax_probe.swift")] {
        let mac = root.join("crates").join(project).join("macos");
        fs::create_dir_all(&mac).unwrap();
        fs::write(mac.join(file), "print(1)").unwrap();
    }
    write_exec(
        &root.join("swiftc"),
        r#"#!/bin/sh
case "$*" in *"-target arm64-apple-macosx13.0"*) ;; *) exit 1;; esac
while [ "$1" != -o ]; do shift; done
echo built > "$2"
"#,
    );
    write_exec(
        &root.join("codesign"),
        r#"#!/bin/sh
[ "$2" = --sign ] && [ "$3" = - ] || exit 1
mkdir -p "$6/Contents/_CodeSignature"
echo sealed > "$6/Contents/_CodeSignature/CodeResources"
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_recog"))
        .arg("__package-macos-apps")
        .arg(root)
        .env("PATH", format!("{}:/usr/bin:/bin", root.display()))
        .env("RECOG_CODESIGN_IDENTITY", "must not be used")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    for name in ["Recog", "RecogZoomHelper"] {
        assert!(root.join(format!("{name}.app/Contents/_CodeSignature/CodeResources")).is_file());
        assert!(root.join(format!("{name}.source-sha256")).is_file());
    }
}
#[test]
fn checkout_when_setup_runs_then_build_is_handed_off_and_no_services_flag_preserves_units() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("checkout");
    let release = dir.path().join("build/release");
    let stubs = dir.path().join("tools");
    let home = dir.path().join("home");
    for p in [&root.join("crates/recog-live/macos"), &release, &stubs, &home] {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(root.join("crates/recog-live/Cargo.toml"), "").unwrap();
    common::place_recog(&release.join("recog"));
    for name in ["recog-web", "recog-copilot-channel", "recog-copilot-supervisor", "recog-desktop", "recog-mic"] {
        write_exec(&release.join(name), "#!/bin/sh\nexit 0\n");
    }
    write_exec(&root.join("sck_audio_capture"), "#!/bin/sh\nexit 0\n");
    fs::write(root.join("crates/recog-live/macos/recog_launcher.swift"), "print(1)").unwrap();
    write_exec(
        &stubs.join("cargo"),
        "#!/bin/sh\ncase $1 in build) exit 0;; metadata) printf '{\"target_directory\":\"%s\"}\\n' \"$TEST_TARGET\";; *) exit 12;; esac\n",
    );
    fs::write(root.join("crates/recog-live/macos/recog_permission_probe.swift"), "print(1)").unwrap();
    fs::write(root.join("crates/recog-live/macos/recog_permissions_window.swift"), "print(1)").unwrap();
    let run = |args: &[&str]| {
        Command::new(release.join("recog"))
            .env_clear()
            .env("HOME", &home)
            .env("RECOG_REPO_DIR", &root)
            .env("TEST_TARGET", release.parent().unwrap())
            .env("PATH", format!("{}:/usr/bin:/bin", stubs.display()))
            .args(args)
            .current_dir(&home)
            .output()
            .unwrap()
    };
    let dry = run(&["setup", "--dry-run"]);
    assert!(dry.status.success(), "{}", String::from_utf8_lossy(&dry.stderr));
    assert!(!root.join("bin").exists());
    let setup = run(&["setup", "--no-env", "--no-extension", "--no-prefetch", "--no-services"]);
    assert!(setup.status.success(), "{}", String::from_utf8_lossy(&setup.stderr));
    assert!(root.join("bin/recog-web").is_file());
    assert!(!home.join("Library/LaunchAgents").exists());
    assert!(!home.join(".config/systemd/user").exists());
    assert!(!root.join(".env").exists());
}

#[test]
fn active_conference_when_updating_or_installing_then_checkout_and_binaries_stay_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let bin = dir.path().join("stubs");
    let units = dir.path().join("units");
    for path in [&bin, &units, &root.join("crates/recog-live"), &dir.path().join("data/meetings/m")] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(root.join("crates/recog-live/Cargo.toml"), "").unwrap();
    fs::write(units.join("recog-live.service"), "test").unwrap();
    fs::write(
        dir.path().join("data/meetings/m/meta.json"),
        r#"{"id":"m","created_at":"2026-11-18T10:00:00","source":"gmeet"}"#,
    )
    .unwrap();
    write_exec(&bin.join("systemctl"), "#!/bin/sh\necho 123\n");
    let program = dir.path().join("recog");
    common::place_recog(&program);
    for action in ["update", "__install-built"] {
        let result = Command::new(&program)
            .arg(action)
            .env_clear()
            .env("HOME", dir.path())
            .env("PATH", &bin)
            .env("RECOG_REPO_DIR", &root)
            .env("RECOG_DATA_DIR", dir.path().join("data"))
            .env("SVC_UNIT_DIR", &units)
            .env("RECOG_SERVICE_OS", "Linux")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("conference is active"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!root.join("bin").exists());
    }
}
