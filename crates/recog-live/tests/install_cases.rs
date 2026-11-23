//! Drive checkout setup through its public CLI with isolated data and service managers.
mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn exe(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
struct Rig {
    temp: tempfile::TempDir,
    root: PathBuf,
    built: PathBuf,
    tools: PathBuf,
}
impl Rig {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("checkout");
        let built = temp.path().join("target/release");
        let tools = temp.path().join("tools");
        for dir in [
            &root.join("crates/recog-live/macos"),
            &root.join("browser-extension"),
            &built,
            &tools,
            &temp.path().join("home"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(root.join("crates/recog-live/Cargo.toml"), "").unwrap();
        fs::write(root.join(".env.example"), "EXAMPLE=1\n").unwrap();
        fs::write(root.join("browser-extension/package-lock.json"), "{}").unwrap();
        for source in ["recog_launcher.swift", "sck_audio_capture.swift", "recog_permission_probe.swift"] {
            fs::write(root.join("crates/recog-live/macos").join(source), "").unwrap();
        }
        common::place_recog(&built.join("recog"));
        for name in [
            "recog-web",
            "recog-desktop",
            "recog-copilot-supervisor",
            "recog-copilot-channel",
            "recog-copilot-codex",
            "recog-mic",
        ] {
            exe(&built.join(name), "#!/bin/sh\nexit 0\n");
        }
        exe(
            &tools.join("cargo"),
            r#"#!/bin/sh
echo "cargo $*" >> "$LOG"
case $1 in
  build) exit "${BUILD_RC:-0}";;
  metadata) printf '{"target_directory":"%s"}\n' "$TARGET";;
  *) exit 1;;
esac
"#,
        );
        exe(
            &tools.join("systemctl"),
            r#"#!/bin/sh
echo "systemctl $*" >> "$LOG"
case "$*" in
  *MainPID*recog-live.service*) echo "${LIVE_PID:-0}";;
  *MainPID*) echo 0;;
  *"--now"*) exit "${ACTIVATE_RC:-0}";;
esac
"#,
        );
        for name in ["ffmpeg", "parec", "pactl", "node", "npm", "codesign"] {
            exe(&tools.join(name), &format!("#!/bin/sh\necho '{name}' \"$@\" >> \"$LOG\"\nexit 0\n"));
        }
        exe(
            &tools.join("swiftc"),
            r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then echo '#!/bin/sh' > "$2"; /bin/chmod 755 "$2"; exit 0; fi
    shift
done
exit 1
"#,
        );
        Self { temp, root, built, tools }
    }
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let t = self.temp.path();
        Command::new(self.built.join("recog"))
            .args(args)
            .current_dir(t)
            .env_clear()
            .env("HOME", t.join("home"))
            .env("PATH", &self.tools)
            .env("LOG", t.join("calls"))
            .env("TARGET", self.built.parent().unwrap())
            .env("RECOG_REPO_DIR", &self.root)
            .env("RECOG_DATA_DIR", t.join("data"))
            .env("RECOG_SERVICE_OS", "Linux")
            .env("SVC_UNIT_DIR", t.join("units"))
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.temp.path().join("calls")).unwrap_or_default()
    }
}
fn success(out: &Output) {
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

#[test]
fn first_install_when_setup_runs_twice_then_configuration_is_preserved_and_services_reactivate() {
    let rig = Rig::new();
    success(&rig.run(&["setup", "--no-prefetch"], &[]));
    let calls = rig.calls();
    assert!(calls.contains("npm ci") && calls.contains("npm run build"));
    assert!(calls.contains("cargo build --release --locked --bin recog"));
    assert!(calls.find("daemon-reload").unwrap() < calls.find("enable --now").unwrap());
    assert_eq!(calls.matches("enable --now").count(), 4);
    assert_eq!(fs::read_to_string(rig.root.join(".env")).unwrap(), "EXAMPLE=1\n");
    fs::write(rig.root.join(".env"), "MY_SETTING=1\n").unwrap();
    success(&rig.run(&["setup", "--no-prefetch", "--no-extension"], &[("LIVE_PID", "123")]));
    assert!(rig.calls().contains("systemctl --user restart recog-live.service"));
    assert_eq!(fs::read_to_string(rig.root.join(".env")).unwrap(), "MY_SETTING=1\n");
    fs::remove_file(rig.root.join(".env")).unwrap();
    success(&rig.run(&["setup", "--no-prefetch", "--no-extension"], &[]));
    assert!(!rig.root.join(".env").exists());
}

#[test]
fn failed_build_invalid_configuration_or_missing_program_when_setting_up_then_no_binaries_are_replaced() {
    for kind in ["build", "config", "binary", "cargo"] {
        let r = Rig::new();
        fs::create_dir(r.root.join("bin")).unwrap();
        fs::write(r.root.join("bin/recog"), "old").unwrap();
        if kind == "binary" {
            fs::remove_file(r.built.join("recog-web")).unwrap();
        }
        if kind == "cargo" {
            fs::remove_file(r.tools.join("cargo")).unwrap();
        }
        let env = match kind {
            "build" => vec![("BUILD_RC", "7")],
            "config" => vec![("RECOG_INSTALL_RUNTIME", "python")],
            _ => vec![],
        };
        let out = r.run(&["setup", "--no-prefetch"], &env);
        assert!(!out.status.success(), "{kind}");
        assert_eq!(fs::read_to_string(r.root.join("bin/recog")).unwrap(), "old");
        assert!(!r.calls().contains("enable --now"));
    }
}

#[test]
fn dry_run_or_disabled_steps_when_requested_then_their_effects_are_absent() {
    let r = Rig::new();
    success(&r.run(&["setup", "--dry-run", "--no-services", "--no-extension", "--no-env"], &[]));
    assert!(r.calls().is_empty());
    assert!(!r.root.join("bin").exists());
    success(&r.run(&["setup", "--no-services", "--no-prefetch", "--no-extension", "--no-env"], &[]));
    assert!(!r.calls().contains("systemctl") && !r.calls().contains("npm"));
    assert!(!r.root.join(".env").exists());
    assert!(r.root.join("bin/recog").is_file());
}

#[test]
fn missing_audio_or_failed_activation_when_setting_up_then_setup_reports_failure() {
    for missing in [true, false] {
        let r = Rig::new();
        if missing {
            fs::remove_file(r.tools.join("ffmpeg")).unwrap();
        }
        let out = r.run(&["setup", "--no-prefetch", "--no-extension"], &[("ACTIVATE_RC", "1")]);
        assert!(!out.status.success());
        assert_eq!(r.calls().contains("enable --now"), !missing);
    }
}

#[test]
fn private_fields_in_ai_settings_when_setup_checks_then_no_activation_or_settings_rewrite() {
    let rig = Rig::new();
    let data = rig.temp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let settings = data.join("ai-settings.json");
    let content =
        r#"{"version":1,"revision":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","models":{"api_key":"fixture"},"tasks":{}}"#;
    fs::write(&settings, content).unwrap();
    let result = rig.run(&["setup", "--no-prefetch", "--no-extension"], &[]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Invalid AI settings"));
    assert!(!rig.calls().contains("enable --now"));
    assert_eq!(fs::read_to_string(settings).unwrap(), content);
    assert!(!data.join("ai-settings.lock").exists());
}
