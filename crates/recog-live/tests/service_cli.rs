//! Exercise service operations in a throwaway checkout with stub OS managers.
mod common;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn stub(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn installed_services_when_restarted_or_stopped_then_readiness_and_cleanup_follow_user_intent() {
    for os in ["Linux", "Darwin"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let home = dir.path().join("home");
        let bin = dir.path().join("stubs");
        for path in [&bin, &home, &root.join("bin"), &root.join("crates/recog-live")] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(root.join("crates/recog-live/Cargo.toml"), "").unwrap();
        let recog = root.join("bin/recog");
        common::place_recog(&recog);
        let log = dir.path().join("commands");
        for name in ["launchctl", "systemctl", "tmux"] {
            stub(
                &bin.join(name),
                &format!(
                    "#!/bin/sh\necho '{name}' \"$@\" >> \"$TEST_LOG\"\ncase \"$*\" in *has-session*) exit 1;; esac\n"
                ),
            );
        }
        let run = |args: &[&str], skip: bool| {
            Command::new(&recog)
                .env_clear()
                .env("HOME", &home)
                .env("PATH", &bin)
                .env("TEST_LOG", &log)
                .env("RECOG_SERVICE_OS", os)
                .env("RECOG_SKIP_READINESS", if skip { "1" } else { "0" })
                .args(args)
                .current_dir(&home)
                .output()
                .unwrap()
        };
        let installed = run(&["service", "install"], false);
        assert!(installed.status.success(), "{}", String::from_utf8_lossy(&installed.stderr));
        let before = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!before.contains(" load ") && !before.contains("--now"));
        assert!(!run(&["service", "restart", "live"], false).status.success());
        assert_eq!(before, std::fs::read_to_string(&log).unwrap_or_default());
        assert!(run(&["service", "restart", "live"], true).status.success());
        assert!(!std::fs::read_to_string(&log).unwrap().contains("kill-session"));
        let config = home.join(".config/recog/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(config, "invalid = [").unwrap();
        assert!(run(&["service", "stop"], false).status.success());
        let after = std::fs::read_to_string(&log).unwrap();
        assert!(after.contains("kill-session -t =copilot-manual"));
    }
}
