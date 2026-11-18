//! What the shell scripts of an installation without Python (`[install] runtime =
//! "rust"`) ask the `recog` binary instead of the interpreter: the readiness check of
//! `scripts/deps.sh` (`recog check`), the values `scripts/data-dir.sh`,
//! `scripts/config-path.sh`, `recog copilot` and the extension build print
//! (`recog config …`), the build of `Recog.app` (`recog launcher`, in place of
//! `python -m recog.macos_launcher`), the model download (`recog prefetch`) and the
//! KTalk desktop subcommands (`recog desktop …`, run by `recog-desktop`).
//!
//! Each answers as the Python snippet it replaces: the same lines, on the same stream,
//! with the same exit status.

use std::path::{Path, PathBuf};

use recog_ai::install::LegacyEnv;
use recog_ai::json::{self, Json};
use recog_ai::settings::{AiSettingsStore, Fallback, validate_models};
use recog_core::data_dir::get_data_dir;
use recog_core::install_config::{self, Environ, InstallConfig};
use recog_core::user_profile::UserProfileStore;
use recog_zoom::helper_app::{PlistValue, SystemToolchain, build_app, find_or_build_app};

use crate::asr::WavoEngine;
use crate::cli::args::ConfigQuery;
use crate::cli::meetings::Printed;

/// The application the live service runs under on macOS (`recog/macos_launcher.py`).
pub const LAUNCHER_APP: &str = "Recog.app";
/// The permissions are recorded under the bundle identifier, so it must not change.
pub const LAUNCHER_BUNDLE_ID: &str = "com.recog.live";
pub const LAUNCHER_NAME: &str = "Recog";
pub const LAUNCHER_EXECUTABLE: &str = "recog_launcher";
pub const LAUNCHER_SOURCE: &str = "crates/recog-live/macos/recog_launcher.swift";
/// Beside the bundle, not in it: everything inside is sealed by the signature.
pub const LAUNCHER_STAMP: &str = "Recog.source-sha256";

fn printed(stdout: impl Into<String>, stderr: impl Into<String>, code: i32) -> Printed {
    Printed { stdout: stdout.into(), stderr: stderr.into(), code }
}

fn line(text: impl std::fmt::Display) -> String {
    format!("{text}\n")
}

/// Prints what a command produced and gives its exit status.
#[must_use]
pub fn print(outcome: Printed) -> i32 {
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    outcome.code
}

pub(super) fn backup(root: &Path, env: &Environ, args: &[String]) -> i32 {
    const HELP: &str = "usage: recog backup [--dest DIR] [--remote HOST:DIR] [--with-logs]";
    let home = recog_core::paths::expanduser("~", env);
    let mut opts = recog_install::backup::Backup::new(&home);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--with-logs" => opts.with_logs = true,
            "-h" | "--help" => {
                println!("{HELP}");
                return 0;
            }
            "--dest" | "--remote" => {
                let Some(value) = it.next().filter(|s| !s.is_empty() && !s.starts_with("--")) else {
                    eprintln!("{HELP}");
                    return 2;
                };
                if arg == "--dest" {
                    opts.destination = value.into();
                } else {
                    opts.remote = Some(value.clone());
                }
            }
            _ => {
                eprintln!("{HELP}");
                return 2;
            }
        }
    }
    let config = install_config::config_path(root, env);
    match super::data_dir(root, env)
        .and_then(|data| recog_install::backup::snapshot(root, &data, &config, &opts).map_err(|e| e.to_string()))
    {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Backup failed: {e}");
            1
        }
    }
}

pub(super) fn migrate(repo: &Path, environ: &Environ, args: &[String]) -> i32 {
    const HELP: &str = "usage: recog migrate-data [--apply]\nStop services and manual writers before --apply.";
    let apply = match args {
        [] => false,
        [arg] if arg == "--apply" => true,
        [arg] if arg == "--help" || arg == "-h" => {
            println!("{HELP}");
            return 0;
        }
        _ => {
            eprintln!("{HELP}");
            return 2;
        }
    };
    let moves = recog_core::data_dir::data_dir_target(environ)
        .map_err(|e| e.to_string())
        .and_then(|target| recog_install::migrate::migrate(repo, &target, apply).map_err(|e| e.to_string()));
    match moves {
        Ok(moves) => {
            for (source, dest) in &moves {
                println!("{} {} -> {}", if apply { "Moved" } else { "Would move" }, source.display(), dest.display());
            }
            if moves.is_empty() {
                println!("No legacy data to move.");
            } else if apply {
                println!("Reinstall and restart the services to use the new paths.");
            } else {
                println!("Dry run only. Stop services and manual writers, then repeat with --apply.");
            }
            0
        }
        Err(message) => {
            eprintln!("Migration refused: {message}");
            1
        }
    }
}

pub(super) fn service_backend() -> Result<recog_install::units::Backend, String> {
    use recog_install::units::Backend;
    let backend = match std::env::var("RECOG_SERVICE_OS").as_deref() {
        Ok("Darwin") => Backend::Launchd,
        Ok("Linux") => Backend::Systemd,
        Err(_) | Ok("") if cfg!(target_os = "macos") => Backend::Launchd,
        Err(_) | Ok("") if cfg!(target_os = "linux") => Backend::Systemd,
        _ => return Err("Unsupported service OS (expected Darwin or Linux)".into()),
    };
    Ok(backend)
}

pub(super) fn render_units(root: &Path, environ: &Environ, dest: &Path) -> Result<Vec<PathBuf>, String> {
    use recog_install::units::Layout;
    let backend = service_backend()?;
    let home = std::env::var_os("HOME").ok_or("HOME is missing")?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let data = get_data_dir(root, environ, &mut |warning| eprintln!("{warning}")).map_err(|e| e.to_string())?;
    let logs = data.join("logs");
    let unit_dir = backend.unit_dir(home.as_ref());
    backend
        .render(
            &Layout { root, home: Path::new(&home), data: &data, logs: &logs, search_path: &path, unit_dir: &unit_dir },
            dest,
        )
        .map_err(|e| e.to_string())
}

fn prompts_dir(repo_root: &Path) -> PathBuf {
    repo_root.join("crates").join("recog-ai").join("prompts")
}

/// The data directory with the legacy warning collected for stderr.
fn data_dir(repo_root: &Path, environ: &Environ, warnings: &mut String) -> Result<PathBuf, String> {
    get_data_dir(repo_root, environ, &mut |warning| warnings.push_str(&line(warning))).map_err(|e| e.to_string())
}

/// `recog check`: nothing and status 0 when the settings can serve, else the first
/// problem on stdout and status 1, as `deps_asr_config_check` printed them.
///
/// An unreadable `config.toml` or `.env` value is reported by the caller, which loads
/// the configuration before the command runs. Of the Python probe's `[asr]` checks
/// only the remote one has a counterpart (a gateway without an address): the Rust live
/// process runs ASR on wavo, which is part of the binary, for any other backend.
#[must_use]
pub fn check(repo_root: &Path, config: &InstallConfig, environ: &Environ) -> Printed {
    let runtime = &config.install.runtime;
    if runtime == "python" {
        return printed(
            line(
                r#"[install] runtime = "python" is no longer available: recog installs without Python; delete the key"#,
            ),
            "",
            1,
        );
    }
    if runtime != "rust" {
        let repr = recog_core::py::repr(runtime);
        return printed(line(format!(r#"[install] runtime must be "rust", not {repr}"#)), "", 1);
    }
    let mut warnings = String::new();
    let data_dir = match data_dir(repo_root, environ, &mut warnings) {
        Ok(dir) => dir,
        Err(e) => return printed(line(e), warnings, 1),
    };
    if let Err(e) = UserProfileStore::new(&data_dir).read() {
        return printed(line(format!("Invalid user profile: {e}")), warnings, 1);
    }
    if let Err(e) = AiSettingsStore::new(&data_dir, Some(prompts_dir(repo_root))).read() {
        return printed(line(format!("Invalid AI settings: {e}")), warnings, 1);
    }
    for (provider, model) in [("claude", &config.ai.claude_model), ("codex", &config.ai.codex_model)] {
        if let Err(e) = validate_models(&json::object([(provider, Json::from(model.as_str()))])) {
            return printed(line(format!("Invalid [ai] {provider}_model: {e}")), warnings, 1);
        }
    }
    if config.asr.backend == "remote" && recog_core::py::strip(&config.asr.remote_url).is_empty() {
        let problem = "RECOG_ASR_BACKEND=remote but RECOG_ASR_REMOTE_URL is empty — add the gateway URL as [asr] \
                       remote_url to config.toml (build_asr() refuses to fall back to the local model silently: \
                       different RAM and latency)";
        return printed(line(problem), warnings, 1);
    }
    printed("", warnings, 0)
}

/// `recog config <query>`: one value on stdout.
#[must_use]
pub fn config(query: ConfigQuery, repo_root: &Path, config: &InstallConfig, environ: &Environ) -> Printed {
    match query {
        ConfigQuery::DataDir => {
            let mut warnings = String::new();
            match data_dir(repo_root, environ, &mut warnings) {
                Ok(dir) => printed(line(dir.display()), warnings, 0),
                Err(e) => printed("", warnings + &line(e), 1),
            }
        }
        ConfigQuery::ConfigPath => printed(line(install_config::config_path(repo_root, environ).display()), "", 0),
        ConfigQuery::Conferences => printed(line(config.hosts_json()), "", 0),
        ConfigQuery::InstallRuntime => printed(line(&config.install.runtime), "", 0),
        ConfigQuery::CopilotLaunch => copilot_launch(repo_root, config, environ),
    }
}

/// The Copilot's Claude model and effort (`auto` without one) on two lines, as
/// `recog copilot` read them from Python; status 1 when Copilot is off.
fn copilot_launch(repo_root: &Path, config: &InstallConfig, environ: &Environ) -> Printed {
    let mut warnings = String::new();
    let data_dir = match data_dir(repo_root, environ, &mut warnings) {
        Ok(dir) => dir,
        Err(e) => return printed("", warnings + &line(e), 1),
    };
    let legacy = LegacyEnv::of_install(config, environ);
    let store = AiSettingsStore::new(&data_dir, Some(prompts_dir(repo_root))).with_legacy_env(legacy);
    let task = match store.resolve_task("copilot", None, Fallback::default()) {
        Ok(task) => task,
        Err(e) => return printed("", warnings + &line(e), 1),
    };
    if task.enabled == Some(false) {
        return printed("", warnings + "Copilot выключен в настройках.\n", 1);
    }
    let model = task.models.get("claude").cloned().unwrap_or_default();
    let effort = if task.claude_effort.is_empty() { "auto".to_owned() } else { task.claude_effort };
    printed(format!("{model}\n{effort}\n"), warnings, 0)
}

/// `Info.plist` of `Recog.app`, the entries of `_launcher_info()` in
/// `recog/macos_launcher.py`: the bytes are sealed by the signature.
#[must_use]
pub fn launcher_info() -> Vec<(&'static str, PlistValue)> {
    vec![
        ("CFBundleIdentifier", PlistValue::String(LAUNCHER_BUNDLE_ID)),
        ("CFBundleName", PlistValue::String(LAUNCHER_NAME)),
        ("CFBundleDisplayName", PlistValue::String(LAUNCHER_NAME)),
        ("CFBundleExecutable", PlistValue::String(LAUNCHER_EXECUTABLE)),
        ("CFBundlePackageType", PlistValue::String("APPL")),
        ("CFBundleShortVersionString", PlistValue::String("1.0")),
        // No Dock icon, no menu bar: it only runs the service.
        ("LSBackgroundOnly", PlistValue::Bool(true)),
        // Shown in the microphone dialog, which macOS refuses to show without it.
        ("NSMicrophoneUsageDescription", PlistValue::String("Recog transcribes your side of the conversation.")),
    ]
}

/// `recog launcher`: `Recog.app` under `repo_root`, built when it is missing or its
/// source changed (never otherwise: a new signature costs the user the Screen
/// Recording and microphone permissions); its executable on stdout.
#[must_use]
pub fn launcher(repo_root: &Path, config: &InstallConfig) -> Printed {
    let identity =
        if config.macos.codesign_identity.is_empty() { "-" } else { config.macos.codesign_identity.as_str() };
    let toolchain = SystemToolchain;
    let app = repo_root.join(LAUNCHER_APP);
    let built = find_or_build_app(
        &app,
        &repo_root.join(LAUNCHER_SOURCE),
        &repo_root.join(LAUNCHER_STAMP),
        &toolchain,
        |app, source| {
            eprintln!("Building {LAUNCHER_APP} from {}...", source.display());
            build_app(app, source, &launcher_info(), &[], identity, &toolchain)?;
            eprintln!(
                "Built {LAUNCHER_APP}: macOS asks again for Screen Recording and the microphone, now for \"{LAUNCHER_NAME}\""
            );
            Ok(())
        },
    );
    match built {
        Ok(app) => printed(line(app.join("Contents/MacOS").join(LAUNCHER_EXECUTABLE).display()), "", 0),
        Err(e) => printed("", line(e), 1),
    }
}

/// `recog prefetch`: the ASR model in the Hugging Face cache (`HF_HOME`), downloaded
/// when it is missing; its path on stdout.
#[must_use]
pub fn prefetch() -> Printed {
    let notice = if WavoEngine::cached_path().is_some() {
        String::new()
    } else {
        line("Downloading the ASR model gigaam-v3...")
    };
    eprint!("{notice}");
    match WavoEngine::fetch() {
        Ok(path) => printed(line(path.display()), "", 0),
        Err(e) => printed("", line(format!("Could not download the ASR model: {e}")), 1),
    }
}

pub(super) fn copilot(repo_root: &Path) -> i32 {
    use std::os::unix::process::CommandExt as _;
    let program = recog_core::install::program(repo_root, "recog-copilot-supervisor");
    let error = std::process::Command::new(&program).args(["foreground", "--repo-root"]).arg(repo_root).exec();
    eprintln!("recog copilot: cannot run {}: {error}", program.display());
    1
}

/// `recog desktop …`: becomes `recog-desktop` of the same installation with the
/// checkout and the arguments; returns only when it cannot run.
#[must_use]
pub fn desktop(repo_root: &Path, args: &[String]) -> i32 {
    use std::os::unix::process::CommandExt as _;
    let program = recog_core::install::program(repo_root, "recog-desktop");
    if !program.is_file() {
        eprintln!("recog desktop runs {}, which is not built: cargo setup builds it", program.display());
        return 2;
    }
    let error = std::process::Command::new(&program).arg("--repo-root").arg(repo_root).args(args).exec();
    eprintln!("recog desktop: cannot run {}: {error}", program.display());
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use recog_zoom::helper_app::info_plist;

    /// What `plistlib.dumps(recog.macos_launcher._launcher_info())` gives.
    const PYTHON_LAUNCHER_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDisplayName</key>
	<string>Recog</string>
	<key>CFBundleExecutable</key>
	<string>recog_launcher</string>
	<key>CFBundleIdentifier</key>
	<string>com.recog.live</string>
	<key>CFBundleName</key>
	<string>Recog</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>1.0</string>
	<key>LSBackgroundOnly</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>Recog transcribes your side of the conversation.</string>
</dict>
</plist>
"#;

    fn environ(pairs: &[(&str, &str)]) -> Environ {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn launcher_info_when_written_then_the_bytes_python_writes() {
        // given / when / then
        assert_eq!(info_plist(&launcher_info()), PYTHON_LAUNCHER_PLIST);
    }

    #[test]
    fn built_launcher_with_its_stamp_when_asked_then_kept_and_named() {
        // given
        let repo = tempfile::tempdir().unwrap();
        let source = repo.path().join(LAUNCHER_SOURCE);
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "// launcher\n").unwrap();
        let signature = repo.path().join("Recog.app/Contents/_CodeSignature/CodeResources");
        std::fs::create_dir_all(signature.parent().unwrap()).unwrap();
        std::fs::write(&signature, "sealed").unwrap();
        let digest: String = {
            use sha2::Digest as _;
            sha2::Sha256::digest(b"// launcher\n").iter().map(|b| format!("{b:02x}")).collect()
        };
        std::fs::write(repo.path().join(LAUNCHER_STAMP), &digest).unwrap();
        // when
        let outcome = launcher(repo.path(), &InstallConfig::default());
        // then
        assert_eq!(outcome.code, 0, "{}", outcome.stderr);
        assert_eq!(outcome.stdout, line(repo.path().join("Recog.app/Contents/MacOS/recog_launcher").display()));
        assert_eq!(std::fs::read_to_string(&signature).unwrap(), "sealed");
    }

    #[test]
    fn data_dir_and_install_runtime_when_queried_then_one_line_each() {
        // given
        let repo = tempfile::tempdir().unwrap();
        let data = repo.path().join("data");
        let env = environ(&[("RECOG_DATA_DIR", data.to_str().unwrap()), ("HOME", "/home/u")]);
        let mut config = InstallConfig::default();
        config.install.runtime = "rust".into();
        config.jitsi = vec!["https://meet.example.org".into()];
        // when / then
        assert_eq!(super::config(ConfigQuery::DataDir, repo.path(), &config, &env).stdout, line(data.display()));
        assert_eq!(super::config(ConfigQuery::InstallRuntime, repo.path(), &config, &env).stdout, "rust\n");
        assert_eq!(
            super::config(ConfigQuery::Conferences, repo.path(), &config, &env).stdout,
            "{\"jitsi\": [\"https://meet.example.org\"], \"matrix\": []}\n"
        );
        let env = environ(&[("RECOG_CONFIG_FILE", "/etc/recog.toml")]);
        assert_eq!(super::config(ConfigQuery::ConfigPath, repo.path(), &config, &env).stdout, "/etc/recog.toml\n");
    }

    #[test]
    fn relative_data_dir_when_queried_or_checked_then_an_error_and_status_1() {
        // given
        let repo = tempfile::tempdir().unwrap();
        let env = environ(&[("RECOG_DATA_DIR", "relative")]);
        // when
        let queried = super::config(ConfigQuery::DataDir, repo.path(), &InstallConfig::default(), &env);
        let checked = check(repo.path(), &InstallConfig::default(), &env);
        // then
        assert_eq!((queried.code, queried.stdout.as_str()), (1, ""));
        assert!(queried.stderr.contains("RECOG_DATA_DIR must be an absolute path"));
        assert_eq!(checked.code, 1);
        assert!(checked.stdout.contains("RECOG_DATA_DIR must be an absolute path"));
    }

    #[test]
    fn valid_or_absent_settings_when_checked_then_nothing_and_no_file_written() {
        // given
        let repo = tempfile::tempdir().unwrap();
        let data = repo.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let env = environ(&[("RECOG_DATA_DIR", data.to_str().unwrap())]);
        // when
        let absent = check(repo.path(), &InstallConfig::default(), &env);
        let payload = r#"{"version":1,"revision":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","models":{},"tasks":{}}"#;
        std::fs::write(data.join("ai-settings.json"), payload).unwrap();
        let saved = check(repo.path(), &InstallConfig::default(), &env);
        // then
        assert_eq!((absent.code, absent.stdout.as_str()), (0, ""));
        assert_eq!((saved.code, saved.stdout.as_str()), (0, ""));
        let mut names: Vec<_> = std::fs::read_dir(&data).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, vec![std::ffi::OsString::from("ai-settings.json")]);
        assert_eq!(std::fs::read_to_string(data.join("ai-settings.json")).unwrap(), payload);
    }

    #[test]
    fn broken_profile_ai_settings_or_runtime_when_checked_then_the_problem_and_status_1() {
        // given
        let repo = tempfile::tempdir().unwrap();
        let data = repo.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let env = environ(&[("RECOG_DATA_DIR", data.to_str().unwrap())]);
        let mut odd = InstallConfig::default();
        odd.install.runtime = "go".into();
        let mut python = InstallConfig::default();
        python.install.runtime = "python".into();
        // when
        let runtime = check(repo.path(), &odd, &env);
        let gone = check(repo.path(), &python, &env);
        std::fs::write(data.join("ai-settings.json"), "broken JSON").unwrap();
        let ai = check(repo.path(), &InstallConfig::default(), &env);
        std::fs::write(data.join("user-profile.json"), "{").unwrap();
        let profile = check(repo.path(), &InstallConfig::default(), &env);
        // then
        assert_eq!(runtime.stdout, "[install] runtime must be \"rust\", not 'go'\n");
        assert!(gone.stdout.contains("no longer available"), "{}", gone.stdout);
        assert_eq!(gone.code, 1);
        assert!(ai.stdout.starts_with("Invalid AI settings: "), "{}", ai.stdout);
        assert!(profile.stdout.starts_with("Invalid user profile: "), "{}", profile.stdout);
        assert_eq!((runtime.code, ai.code, profile.code), (1, 1, 1));
    }

    #[test]
    fn model_of_the_configuration_with_a_space_when_checked_then_its_key_and_status_1() {
        // given
        let repo = tempfile::tempdir().unwrap();
        let data = repo.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let env = environ(&[("RECOG_DATA_DIR", data.to_str().unwrap())]);
        let mut spaced = InstallConfig::default();
        spaced.ai.codex_model = "gpt 6".into();
        let mut named = InstallConfig::default();
        named.ai.codex_model = "gpt-6".into();
        // when
        let invalid = check(repo.path(), &spaced, &env);
        let valid = check(repo.path(), &named, &env);
        // then
        assert!(invalid.stdout.starts_with("Invalid [ai] codex_model: Model must be "), "{}", invalid.stdout);
        assert_eq!((invalid.code, valid.code, valid.stdout.as_str()), (1, 0, ""));
    }

    #[test]
    fn copilot_switched_off_or_on_when_asked_for_its_launch_then_refused_or_model_and_effort() {
        // given
        let repo = tempfile::tempdir().unwrap();
        let data = repo.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let env = environ(&[("RECOG_DATA_DIR", data.to_str().unwrap())]);
        let mut installed = InstallConfig::default();
        installed.ai.claude_model = "installed-model".into();
        let settings = |copilot: &str| {
            format!(
                r#"{{"version":1,"revision":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","models":{{}},"tasks":{{"copilot":{copilot}}}}}"#
            )
        };
        // when
        std::fs::write(data.join("ai-settings.json"), settings(r#"{"enabled":false}"#)).unwrap();
        let off = config(ConfigQuery::CopilotLaunch, repo.path(), &installed, &env);
        std::fs::write(data.join("ai-settings.json"), settings(r#"{"claude_effort":"high"}"#)).unwrap();
        let on = config(ConfigQuery::CopilotLaunch, repo.path(), &installed, &env);
        // then
        assert_eq!((off.code, off.stdout.as_str(), off.stderr.as_str()), (1, "", "Copilot выключен в настройках.\n"));
        assert_eq!((on.code, on.stdout.as_str()), (0, "installed-model\nhigh\n"));
    }
}
