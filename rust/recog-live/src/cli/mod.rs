//! The `recog` binary: `recog/cli.py` and `live_pipeline` of `recog/live.py`.

pub mod args;
pub mod install;
pub mod meetings;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use recog_bridge::BridgeConfig;
use recog_core::install_config::{self, Environ, InstallConfig};
use recog_core::meeting_storage::{MeetingStorage, StorageOptions, external_paths};

use crate::capture::backend::{current_platform, resolve_system_audio_backend};
use crate::clock::SystemClock;
use crate::config::DEFAULT_MIC_SPEAKER_NAME;
use crate::live::build::{CaptureSettings, MicChoice, build_asr, build_captures};
use crate::live::devices::{self, DeviceInfo};
use crate::live::replay::{ReplaySettings, replay};
use crate::live::session::{self, AiTitles, SessionConfig, ai_increments, stop_on_interrupt};
use crate::live::text::normalize_system_audio;
use crate::live::transcriber::{Live, LiveEnv, LiveOptions, stdout_out};
use crate::live::{NO_MIC, NO_SYSTEM_AUDIO};
use crate::log::Logger;
use args::{Command, LiveArgs, ReplayArgs};

/// The checkout the binary belongs to: `<repo>/bin/recog` (installed by setup.sh) or
/// `rust/target/<profile>/recog` four levels down, else `RECOG_REPO_DIR`, else the
/// checkout it was built from.
#[must_use]
pub fn repo_root() -> PathBuf {
    let exe = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    repo_root_of(exe.as_deref(), std::env::var_os("RECOG_REPO_DIR"))
}

/// [`repo_root`] of the program at `exe` (canonical), `repo_dir` being
/// `RECOG_REPO_DIR`.
fn repo_root_of(exe: Option<&Path>, repo_dir: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(exe) = exe {
        if let Some(repo) = recog_core::install::installed_repo_of(exe) {
            return repo;
        }
        if let Some(root) = build_checkout(exe) {
            return root;
        }
    }
    if let Some(dir) = repo_dir {
        return PathBuf::from(dir);
    }
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&built).unwrap_or(built)
}

/// The checkout of a program built at `<checkout>/rust/target/<profile>/<name>`.
fn build_checkout(exe: &Path) -> Option<PathBuf> {
    let mut ancestors = exe.ancestors();
    let (profile, target, rust) = (ancestors.nth(1), ancestors.next(), ancestors.next());
    if let (Some(_), Some(target), Some(rust)) = (profile, target, rust)
        && target.file_name().is_some_and(|n| n == "target")
        && rust.file_name().is_some_and(|n| n == "rust")
        && let Some(root) = rust.parent()
    {
        return Some(root.to_path_buf());
    }
    None
}

/// `MIC_SPEAKER_NAME` of the environment (after `.env`), or `Me`.
#[must_use]
pub fn legacy_mic_name(environ: &Environ) -> String {
    environ
        .get("MIC_SPEAKER_NAME")
        .map(|v| recog_core::py::strip(v).to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_MIC_SPEAKER_NAME.to_owned())
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

fn data_dir(repo_root: &Path, environ: &Environ) -> Result<PathBuf, String> {
    recog_core::data_dir::get_data_dir(repo_root, environ, &mut |warning| eprintln!("{warning}"))
        .map_err(|e| e.to_string())
}

fn storage_options(config: &InstallConfig, environ: &Environ) -> StorageOptions {
    let home = environ.get("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let mut options = StorageOptions::new(&home);
    options.source_aliases = config.source_aliases.clone();
    options
}

fn print_devices(devices: &[DeviceInfo]) {
    println!("Доступные аудио устройства:");
    println!("{}", devices::format_devices(devices));
    if current_platform().starts_with("linux") {
        let (out, err) = devices::pulse_sources(&devices::run_with_timeout);
        for line in out {
            println!("{line}");
        }
        for line in err {
            eprintln!("{line}");
        }
    }
}

/// `--mic-device`: `auto` finds the default input, `none` is no microphone.
fn resolve_mic(spec: &str, devices: &[DeviceInfo], platform: &str) -> Result<(MicChoice, String), String> {
    if spec == NO_MIC {
        return Ok((MicChoice::Off, "none".into()));
    }
    if spec == "auto" {
        if platform != "darwin" {
            // The sound server's default source, as PortAudio's `pulse` device is.
            return Ok((MicChoice::Default, "default".into()));
        }
        return Ok(match devices::find_default_mic(devices, platform) {
            Some(index) => {
                let name = devices.iter().find(|d| d.index == index).map(|d| d.name.clone()).unwrap_or_default();
                eprintln!("Auto-detected mic: {name} (device {index})");
                (MicChoice::Named(name), index.to_string())
            }
            None => {
                eprintln!("Warning: Could not find an input device");
                (MicChoice::Default, "default".into())
            }
        });
    }
    if platform != "darwin" {
        return Ok((MicChoice::Named(spec.to_owned()), spec.to_owned()));
    }
    let name = devices::device_name(devices, spec).ok_or_else(|| format!("input device {spec} not found"))?;
    Ok((MicChoice::Named(name), spec.to_owned()))
}

/// The system audio and microphone `live` was asked for, resolved on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedAudio {
    mic: MicChoice,
    /// The microphone as the banner shows it.
    mic_display: String,
    /// The resolved backend (`device:<index>` as given).
    backend: String,
    /// The backend the captures are built from (`device:<name>` on a Mac).
    capture_backend: String,
}

/// `--system-audio device:auto`: BlackHole's index, or no system audio.
fn auto_system_device(devices: &[DeviceInfo]) -> String {
    match devices::find_blackhole(devices) {
        Some(index) => {
            let name = devices.iter().find(|d| d.index == index).map(|d| d.name.clone()).unwrap_or_default();
            eprintln!("Auto-detected system audio: {name} (device {index})");
            format!("device:{index}")
        }
        None => {
            eprintln!("Warning: Could not find BlackHole device");
            NO_SYSTEM_AUDIO.into()
        }
    }
}

/// A device index means the PortAudio list on a Mac: the worker opens it by name.
fn capture_backend(backend: &str, devices: &[DeviceInfo], platform: &str) -> Result<String, String> {
    match backend.strip_prefix("device:") {
        Some(id) if platform == "darwin" => devices::device_name(devices, id)
            .map(|name| format!("device:{name}"))
            .ok_or_else(|| format!("input device {id} not found")),
        _ => Ok(backend.to_owned()),
    }
}

fn resolve_audio(
    args: &LiveArgs,
    devices: &[DeviceInfo],
    platform: &str,
    which: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<ResolvedAudio, String> {
    let mut system_audio =
        normalize_system_audio(args.system_audio.as_deref(), args.system_device.as_deref(), args.screencapture);
    let (mic, mic_display) = resolve_mic(&args.mic_device, devices, platform)?;
    if system_audio == "device:auto" {
        system_audio = auto_system_device(devices);
    }
    let backend = resolve_system_audio_backend(Some(&system_audio), platform, which).map_err(|e| e.to_string())?;
    let capture_backend = capture_backend(&backend, devices, platform)?;
    Ok(ResolvedAudio { mic, mic_display, backend, capture_backend })
}

fn live_options(args: &LiveArgs, audio: ResolvedAudio) -> LiveOptions {
    LiveOptions {
        system_audio: audio.backend,
        mic_display: audio.mic_display,
        chunk_duration: args.chunk_duration,
        output_file: args.output.as_ref().map(PathBuf::from),
        use_ktalk: args.ktalk,
        ktalk_only: args.ktalk_only,
        save_audio: !args.no_save_audio,
        mic_speaker_name: args.mic_speaker_name.clone(),
    }
}

fn live_session_config(
    config: &InstallConfig,
    environ: &Environ,
    repo_root: &Path,
    data_dir: &Path,
    legacy_mic_name: String,
) -> SessionConfig {
    let mut storage = storage_options(config, environ);
    storage.title_generator = Some(Arc::new(AiTitles));
    SessionConfig {
        bridge: BridgeConfig {
            extra_origins: config.web.extra_ws_origins.clone(),
            legacy_mic_name,
            ..BridgeConfig::default()
        },
        policy: Arc::new(recog_bridge::ai::AiSettingsPolicy::new(data_dir)),
        increments: Some(ai_increments(repo_root.join("recog").join("prompts"))),
        storage,
        golden_record: config.live.golden_record,
    }
}

/// `--zoom-app` (or `[live] zoom_app`): the Zoom application's source beside the live
/// process, except for a device listing.
fn start_zoom_app(args: &LiveArgs, config: &InstallConfig, repo_root: &Path) {
    if (args.zoom_app || config.live.zoom_app) && !args.list_devices {
        let _ = recog_zoom::run::spawn_in_background(recog_zoom::run::Options::new(repo_root.to_path_buf()));
    }
}

fn run_live(args: &LiveArgs, config: &InstallConfig, environ: &Environ, repo_root: &Path) -> i32 {
    exit_code(live_command(args, config, environ, repo_root))
}

/// The status of a command that printed nothing of its own failure: 1 and the error
/// on stderr, else 0.
fn exit_code(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn live_command(args: &LiveArgs, config: &InstallConfig, environ: &Environ, repo_root: &Path) -> Result<(), String> {
    start_zoom_app(args, config, repo_root);
    let platform = current_platform();
    let devices = devices::list_devices();
    if args.list_devices {
        print_devices(&devices);
        return Ok(());
    }
    let audio = resolve_audio(args, &devices, platform, &which)?;
    let log = Logger::stderr();
    let settings = CaptureSettings { repo_root, config, platform, log: log.clone() };
    let (mic_capture, system_capture) = build_captures(&audio.capture_backend, &audio.mic, &settings)?;
    let data_dir = data_dir(repo_root, environ)?;
    let asr = build_asr(&config.asr, &log)?;
    let legacy = legacy_mic_name(environ);
    let env = LiveEnv {
        data_dir: data_dir.clone(),
        dedup_max_gap_s: config.live.dedup_max_gap_s,
        legacy_mic_name: legacy.clone(),
        clock: SystemClock::shared(),
        zone: Arc::new(recog_core::time::SystemClock),
        log,
        out: stdout_out(),
    };
    let live = Live::new(live_options(args, audio), env, asr, mic_capture, system_capture)?;
    let session_config = live_session_config(config, environ, repo_root, &data_dir, legacy);
    stop_on_interrupt(&live);
    session::run(&live, session_config)
}

fn run_meetings(args: &args::MeetingsArgs, config: &InstallConfig, environ: &Environ, repo_root: &Path) -> i32 {
    let data_dir = match data_dir(repo_root, environ) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut options = storage_options(config, environ);
    options.external_paths = external_paths(&config.web.external_meetings_paths);
    let storage = match MeetingStorage::new(&data_dir.join("meetings"), options) {
        Ok(storage) => storage,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let printed = meetings::run(&storage, args, &recog_core::time::SystemClock);
    print!("{}", printed.stdout);
    eprint!("{}", printed.stderr);
    printed.code
}

fn run_replay(args: &ReplayArgs, config: &InstallConfig, environ: &Environ) -> i32 {
    let settings = ReplaySettings {
        config: config.clone(),
        legacy_mic_name: legacy_mic_name(environ),
        profile: args.profile.as_ref().map(PathBuf::from),
        tail_s: args.tail,
        clock: SystemClock::shared(),
        zone: Arc::new(recog_core::time::SystemClock),
        log: Logger::stderr(),
        out: stdout_out(),
        policy: None,
    };
    match replay(Path::new(&args.session), Path::new(&args.out), settings) {
        Ok(summary) => {
            println!("{}", recog_core::pyjson::dumps(&summary, Some(1)));
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `main()` of `recog/cli.py`: the exit code.
#[must_use]
pub fn main(argv: &[String]) -> i32 {
    let repo_root = repo_root();
    let mut environ: Environ = std::env::vars().collect();
    match load_command(argv, &repo_root, &mut environ) {
        Ok((command, config)) => run_command(command, &config, &environ, &repo_root),
        Err(code) => code,
    }
}

/// The installation configuration and the parsed command, or the exit code of a
/// configuration that does not load or a command line that ends the process.
fn load_command(argv: &[String], repo_root: &Path, environ: &mut Environ) -> Result<(Command, InstallConfig), i32> {
    let config = match install_config::load(repo_root, environ, &mut |notice| eprintln!("{notice}")) {
        Ok(config) => config,
        // The readiness check reports an unreadable configuration as its problem.
        Err(e) if argv == ["check"] => {
            println!("Invalid installation configuration: {e}");
            return Err(1);
        }
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    match args::parse(argv, &legacy_mic_name(environ)) {
        Ok(command) => Ok((command, config)),
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            Err(exit.code)
        }
    }
}

fn run_command(command: Command, config: &InstallConfig, environ: &Environ, repo_root: &Path) -> i32 {
    match command {
        Command::None => {
            print!("{}", args::main_help());
            1
        }
        Command::Live(live) => run_live(&live, config, environ, repo_root),
        Command::Meetings(meetings) => run_meetings(&meetings, config, environ, repo_root),
        Command::Replay(replay) => run_replay(&replay, config, environ),
        Command::Desktop(args) => install::desktop(repo_root, &args),
        Command::Check => install::print(install::check(repo_root, config, environ)),
        Command::Config(query) => install::print(install::config(query, repo_root, config, environ)),
        Command::Launcher => install::print(install::launcher(repo_root, config)),
        Command::Prefetch => install::print(install::prefetch()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(index: usize, name: &str, inputs: u16) -> DeviceInfo {
        DeviceInfo { index, name: name.into(), inputs, outputs: 0, default_input: false, default_output: false }
    }

    fn mac_devices() -> Vec<DeviceInfo> {
        vec![device(0, "Studio Display", 0), device(1, "MacBook Pro Microphone", 1), device(2, "BlackHole 2ch", 2)]
    }

    fn no_parec(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn a_mac_with_a_built_in_microphone_and_blackhole_when_resolving_auto_then_both_by_name() {
        // given
        let args = LiveArgs { system_audio: Some("device:auto".into()), ..LiveArgs::default() };
        // when
        let audio = resolve_audio(&args, &mac_devices(), "darwin", &no_parec).unwrap();
        // then
        assert_eq!(
            audio,
            ResolvedAudio {
                mic: MicChoice::Named("MacBook Pro Microphone".into()),
                mic_display: "1".into(),
                backend: "device:2".into(),
                capture_backend: "device:BlackHole 2ch".into(),
            }
        );
    }

    #[test]
    fn a_mac_without_inputs_when_resolving_auto_then_the_default_input_and_no_system_audio() {
        // given
        let args = LiveArgs { system_audio: Some("device:auto".into()), ..LiveArgs::default() };
        // when
        let audio = resolve_audio(&args, &[device(0, "Studio Display", 0)], "darwin", &no_parec).unwrap();
        // then
        assert_eq!((audio.mic, audio.mic_display.as_str()), (MicChoice::Default, "default"));
        assert_eq!((audio.backend.as_str(), audio.capture_backend.as_str()), ("none", "none"));
    }

    #[test]
    fn device_indexes_a_mac_does_not_have_when_resolving_then_the_missing_device_is_named() {
        // given
        let mic = LiveArgs { mic_device: "9".into(), ..LiveArgs::default() };
        let system =
            LiveArgs { mic_device: "none".into(), system_audio: Some("device:9".into()), ..LiveArgs::default() };
        // when
        let mic = resolve_audio(&mic, &mac_devices(), "darwin", &no_parec);
        let system = resolve_audio(&system, &mac_devices(), "darwin", &no_parec);
        // then
        assert_eq!(mic, Err("input device 9 not found".into()));
        assert_eq!(system, Err("input device 9 not found".into()));
    }

    #[test]
    fn a_device_index_or_name_on_a_mac_when_resolving_then_the_name_the_worker_opens() {
        // given
        let args = LiveArgs { mic_device: "2".into(), system_audio: Some("device:1".into()), ..LiveArgs::default() };
        let named = LiveArgs { mic_device: "USB Mic".into(), ..LiveArgs::default() };
        // when
        let audio = resolve_audio(&args, &mac_devices(), "darwin", &no_parec).unwrap();
        let named = resolve_audio(&named, &mac_devices(), "darwin", &no_parec).unwrap();
        // then
        assert_eq!((audio.mic, audio.mic_display.as_str()), (MicChoice::Named("BlackHole 2ch".into()), "2"));
        assert_eq!(
            (audio.backend.as_str(), audio.capture_backend.as_str()),
            ("device:1", "device:MacBook Pro Microphone")
        );
        assert_eq!((named.mic, named.mic_display.as_str()), (MicChoice::Named("USB Mic".into()), "USB Mic"));
    }

    #[test]
    fn a_binary_installed_built_or_elsewhere_when_finding_its_checkout_then_that_checkout_or_the_fallbacks() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("checkout");
        std::fs::create_dir_all(installed.join("bin")).unwrap();
        std::fs::write(installed.join("recog-service.sh"), "#!/bin/bash\n").unwrap();
        let elsewhere = Path::new("/opt/tools/recog");
        // when
        let from_bin = repo_root_of(Some(&installed.join("bin/recog")), Some("/ignored".into()));
        let from_target = repo_root_of(Some(Path::new("/src/recog/rust/target/release/recog")), None);
        let from_env = repo_root_of(Some(elsewhere), Some("/srv/recog".into()));
        let from_build = repo_root_of(None, None);
        // then
        assert_eq!(from_bin, installed);
        assert_eq!(from_target, Path::new("/src/recog"));
        assert_eq!(from_env, Path::new("/srv/recog"));
        let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert_eq!(from_build, std::fs::canonicalize(&built).unwrap());
    }

    #[test]
    fn a_target_dir_not_under_rust_when_finding_the_build_checkout_then_none() {
        // given / when / then
        assert_eq!(build_checkout(Path::new("/cache/recog-target/debug/recog")), None);
        assert_eq!(build_checkout(Path::new("/src/target/debug/recog")), None);
        assert_eq!(build_checkout(Path::new("/recog")), None);
    }

    #[test]
    fn linux_without_parec_when_resolving_the_pulse_backend_then_the_install_hint() {
        // given
        let args =
            LiveArgs { mic_device: "alsa_input.usb".into(), system_audio: Some("auto".into()), ..LiveArgs::default() };
        // when
        let audio = resolve_audio(&args, &[], "linux", &no_parec);
        // then
        assert!(audio.unwrap_err().starts_with("parec not found"));
    }
}
