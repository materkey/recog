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
    if let Some(repo) = recog_core::install::installed_repo() {
        return repo;
    }
    if let Ok(exe) = std::env::current_exe().and_then(std::fs::canonicalize) {
        let mut ancestors = exe.ancestors();
        let (profile, target, rust) = (ancestors.nth(1), ancestors.next(), ancestors.next());
        if let (Some(_), Some(target), Some(rust)) = (profile, target, rust)
            && target.file_name().is_some_and(|n| n == "target")
            && rust.file_name().is_some_and(|n| n == "rust")
            && let Some(root) = rust.parent()
        {
            return root.to_path_buf();
        }
    }
    if let Some(dir) = std::env::var_os("RECOG_REPO_DIR") {
        return PathBuf::from(dir);
    }
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&built).unwrap_or(built)
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

fn run_live(args: &LiveArgs, config: &InstallConfig, environ: &Environ, repo_root: &Path) -> i32 {
    if (args.zoom_app || config.live.zoom_app) && !args.list_devices {
        let _ = recog_zoom::run::spawn_in_background(recog_zoom::run::Options::new(repo_root.to_path_buf()));
    }
    let platform = current_platform();
    let devices = devices::list_devices();
    if args.list_devices {
        print_devices(&devices);
        return 0;
    }
    let mut system_audio =
        normalize_system_audio(args.system_audio.as_deref(), args.system_device.as_deref(), args.screencapture);
    let (mic, mic_display) = match resolve_mic(&args.mic_device, &devices, platform) {
        Ok(choice) => choice,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if system_audio == "device:auto" {
        match devices::find_blackhole(&devices) {
            Some(index) => {
                let name = devices.iter().find(|d| d.index == index).map(|d| d.name.clone()).unwrap_or_default();
                eprintln!("Auto-detected system audio: {name} (device {index})");
                system_audio = format!("device:{index}");
            }
            None => {
                eprintln!("Warning: Could not find BlackHole device");
                system_audio = NO_SYSTEM_AUDIO.into();
            }
        }
    }
    let backend = match resolve_system_audio_backend(Some(&system_audio), platform, &which) {
        Ok(backend) => backend,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    // A device index means the PortAudio list on a Mac: the worker opens it by name.
    let capture_backend = match backend.strip_prefix("device:") {
        Some(id) if platform == "darwin" => match devices::device_name(&devices, id) {
            Some(name) => format!("device:{name}"),
            None => {
                eprintln!("input device {id} not found");
                return 1;
            }
        },
        _ => backend.clone(),
    };
    let log = Logger::stderr();
    let settings = CaptureSettings { repo_root, config, platform, log: log.clone() };
    let (mic_capture, system_capture) = match build_captures(&capture_backend, &mic, &settings) {
        Ok(captures) => captures,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let data_dir = match data_dir(repo_root, environ) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let asr = match build_asr(&config.asr, &log) {
        Ok(asr) => asr,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
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
    let options = LiveOptions {
        system_audio: backend,
        mic_display,
        chunk_duration: args.chunk_duration,
        output_file: args.output.as_ref().map(PathBuf::from),
        use_ktalk: args.ktalk,
        ktalk_only: args.ktalk_only,
        save_audio: !args.no_save_audio,
        mic_speaker_name: args.mic_speaker_name.clone(),
    };
    let live = match Live::new(options, env, asr, mic_capture, system_capture) {
        Ok(live) => live,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut storage = storage_options(config, environ);
    storage.title_generator = Some(Arc::new(AiTitles));
    let session_config = SessionConfig {
        bridge: BridgeConfig {
            extra_origins: config.web.extra_ws_origins.clone(),
            legacy_mic_name: legacy,
            ..BridgeConfig::default()
        },
        policy: Arc::new(recog_bridge::ai::AiSettingsPolicy::new(&data_dir)),
        increments: Some(ai_increments(repo_root.join("recog").join("prompts"))),
        storage,
        golden_record: config.live.golden_record,
    };
    stop_on_interrupt(&live);
    if let Err(e) = session::run(&live, session_config) {
        eprintln!("{e}");
        return 1;
    }
    0
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
    let config = match install_config::load(&repo_root, &mut environ, &mut |notice| eprintln!("{notice}")) {
        Ok(config) => config,
        // The readiness check reports an unreadable configuration as its problem.
        Err(e) if argv == ["check"] => {
            println!("Invalid installation configuration: {e}");
            return 1;
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let command = match args::parse(argv, &legacy_mic_name(&environ)) {
        Ok(command) => command,
        Err(exit) => {
            print!("{}", exit.stdout);
            eprint!("{}", exit.stderr);
            return exit.code;
        }
    };
    match command {
        Command::None => {
            print!("{}", args::main_help());
            1
        }
        Command::Live(live) => run_live(&live, &config, &environ, &repo_root),
        Command::Meetings(meetings) => run_meetings(&meetings, &config, &environ, &repo_root),
        Command::Replay(replay) => run_replay(&replay, &config, &environ),
        Command::Desktop(args) => install::desktop(&repo_root, &args),
        Command::Check => install::print(install::check(&repo_root, &config, &environ)),
        Command::Config(query) => install::print(install::config(query, &repo_root, &config, &environ)),
        Command::Launcher => install::print(install::launcher(&repo_root, &config)),
        Command::Prefetch => install::print(install::prefetch()),
    }
}
