//! The `recog` binary's commands as the service and the shell scripts run them: no
//! command, `meetings`, `replay` and `live`'s setup up to a whole run on a stand-in
//! sound server, each in a sandbox of its own (data directory, home, configuration).
//!
//! The binary is installed into the sandbox's checkout (`repo/bin/recog`, as
//! `cargo setup` installs it), so that checkout, not the one it was built in, is its own.
//! A child keeps `LLVM_PROFILE_FILE`, so a coverage run counts what it executed.

mod common;

#[cfg(target_os = "linux")]
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use recog_core::meeting_storage::{MeetingStorage, NewSegment, SessionMode, StorageOptions};
use recog_core::pyjson::{self, Value};
use recog_live::cli::args::{MAIN_USAGE, MeetingsArgs, main_help};
use recog_live::cli::meetings;

struct Sandbox {
    dir: tempfile::TempDir,
    recog: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["home", "repo/bin", "repo/crates/recog-live", "bin"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        std::fs::write(dir.path().join("repo/crates/recog-live/Cargo.toml"), "").unwrap();
        let recog = dir.path().join("repo/bin/recog");
        common::place_recog(&recog);
        Self { dir, recog }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.dir.path().join(sub)
    }

    fn data(&self) -> PathBuf {
        self.path("data")
    }

    /// A stand-in program on the sandbox's PATH.
    #[cfg(target_os = "linux")]
    fn program(&self, name: &str, script: &str) {
        let path = self.path("bin").join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.recog);
        command
            .args(args)
            .env_clear()
            .env("HOME", self.path("home"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.path("bin").display()))
            .env("RECOG_CONFIG_FILE", self.path("none.toml"))
            .env("RECOG_DATA_DIR", self.data())
            .env("MIC_SPEAKER_NAME", "Me")
            .env("TZ", "UTC")
            .current_dir(self.path("home"))
            .stdin(Stdio::null());
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        command
    }

    fn run(&self, args: &[&str]) -> (String, String, i32) {
        let Output { status, stdout, stderr } = self.command(args).output().unwrap();
        (
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
            status.code().unwrap_or(-1),
        )
    }
}

#[test]
fn no_command_when_run_then_the_main_help_and_status_1() {
    // given
    let sandbox = Sandbox::new();
    // when
    let (stdout, _, code) = sandbox.run(&[]);
    // then
    assert_eq!((stdout, code), (main_help(), 1));
}

#[test]
fn an_unknown_live_option_when_run_then_the_main_usage_and_status_2_as_argparse() {
    // given
    let sandbox = Sandbox::new();
    // when
    let (stdout, stderr, code) = sandbox.run(&["live", "--bogus"]);
    // then
    assert_eq!((stdout.as_str(), code), ("", 2));
    assert_eq!(stderr, format!("{MAIN_USAGE}\nrecog: error: unrecognized arguments: --bogus\n"));
}

#[test]
fn a_configuration_that_does_not_parse_when_running_a_command_then_its_error_and_status_1() {
    // given
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.path("none.toml"), "[live\n").unwrap();
    // when
    let (stdout, stderr, code) = sandbox.run(&["meetings"]);
    let checked = sandbox.run(&["check"]);
    // then: the readiness check reports it on stdout, any other command on stderr
    assert_eq!((stdout.as_str(), code), ("", 1));
    assert!(!stderr.is_empty());
    assert_eq!(checked.2, 1);
    assert!(checked.0.starts_with("Invalid installation configuration: "), "{}", checked.0);
}

fn storage(data: &Path) -> MeetingStorage {
    let mut options = StorageOptions::new(data);
    options.log = std::sync::Arc::new(|_, _| {});
    MeetingStorage::new(&data.join("meetings"), options).unwrap()
}

fn saved_meeting(data: &Path) -> String {
    let storage = storage(data);
    storage.start_session(&SessionMode::default()).unwrap();
    storage
        .append_segment(&NewSegment {
            speaker: "Alice".into(),
            speaker_id: "Alice".into(),
            text: "добрый день".into(),
            uncertain: true,
            uncertain_reason: "mics_off".into(),
            captured_at: Some(1_790_000_000.0),
            ..NewSegment::default()
        })
        .unwrap();
    let id = storage.current_meeting_id().unwrap();
    storage.end_session().unwrap();
    id
}

#[test]
fn saved_meetings_when_listing_or_showing_them_then_the_list_the_transcript_or_not_found() {
    // given
    let sandbox = Sandbox::new();
    let empty = sandbox.run(&["meetings"]);
    let id = saved_meeting(&sandbox.data());
    // when
    let listed = sandbox.run(&["meetings"]);
    let shown = sandbox.run(&["meetings", &id]);
    let missing = sandbox.run(&["meetings", "nope"]);
    // then
    assert_eq!(empty, ("No meetings found.\n".into(), String::new(), 0));
    assert_eq!(listed.2, 0, "{}", listed.1);
    assert!(listed.0.lines().nth(2).is_some_and(|l| l.starts_with(&id)), "{}", listed.0);
    assert_eq!(shown.2, 0, "{}", shown.1);
    assert!(shown.0.contains(&format!(" Meeting: {id}\n")), "{}", shown.0);
    assert!(shown.0.contains("**Alice (🔇?)** *[") && shown.0.ends_with("]*: добрый день\n"), "{}", shown.0);
    assert_eq!(missing, (String::new(), "Meeting not found: nope\n".into(), 1));
}

#[test]
fn a_relative_data_dir_when_listing_meetings_then_the_error_and_status_1() {
    // given
    let sandbox = Sandbox::new();
    // when
    let output = sandbox.command(&["meetings"]).env("RECOG_DATA_DIR", "relative/data").output().unwrap();
    // then
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("RECOG_DATA_DIR"), "{output:?}");
}

struct Utc;

impl recog_core::time::Clock for Utc {
    fn now_micros(&self) -> i64 {
        0
    }
    fn utc_offset(&self, _utc_micros: i64) -> i64 {
        0
    }
}

#[test]
fn saved_meetings_when_asked_for_json_then_the_storage_answers_as_json() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let id = saved_meeting(dir.path());
    let storage = storage(dir.path());
    let args = |meeting_id: Option<&str>| MeetingsArgs {
        meeting_id: meeting_id.map(str::to_owned),
        json: true,
        refresh_cache: true,
    };
    // when
    let list = meetings::run(&storage, &args(None), &Utc);
    let one = meetings::run(&storage, &args(Some(&id)), &Utc);
    // then
    assert_eq!((list.code, one.code), (0, 0));
    let Value::Array(listed) = pyjson::loads(&list.stdout).unwrap() else { panic!("{}", list.stdout) };
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].as_object().and_then(|m| m.get("id")).and_then(Value::as_str), Some(id.as_str()));
    let meeting = pyjson::loads(&one.stdout).unwrap();
    let segments = meeting.as_object().and_then(|m| m.get("segments")).unwrap();
    let Value::Array(segments) = segments else { panic!("{segments:?}") };
    assert_eq!(segments[0].as_object().and_then(|m| m.get("text")).and_then(Value::as_str), Some("добрый день"));
    assert!(list.stdout.starts_with("[\n  {") && list.stdout.ends_with("\n"));
}

#[test]
fn an_unreadable_meeting_when_shown_then_the_storage_error_and_status_1() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let id = saved_meeting(dir.path());
    std::fs::write(dir.path().join("meetings").join(&id).join("meta.json"), "{broken").unwrap();
    let storage = storage(dir.path());
    let args = MeetingsArgs { meeting_id: Some(id), json: false, refresh_cache: false };
    // when
    let shown = meetings::run(&storage, &args, &Utc);
    // then
    assert_eq!(shown.code, 1, "{shown:?}");
    assert!(shown.stdout.is_empty() && shown.stderr.ends_with('\n') && shown.stderr.len() > 1, "{shown:?}");
}

#[test]
fn valid_settings_when_checked_or_asked_for_the_data_dir_then_nothing_wrong_and_the_dir() {
    // given
    let sandbox = Sandbox::new();
    // when
    let checked = sandbox.run(&["check"]);
    let data_dir = sandbox.run(&["config", "data-dir"]);
    // then
    assert_eq!(checked, (String::new(), String::new(), 0));
    assert_eq!(data_dir, (format!("{}\n", sandbox.data().display()), String::new(), 0));
}

#[test]
fn a_checkout_without_recog_desktop_when_running_desktop_then_how_to_build_it_and_status_2() {
    // given
    let sandbox = Sandbox::new();
    let repo = std::fs::canonicalize(sandbox.path("repo")).unwrap();
    // when
    let (stdout, stderr, code) = sandbox.run(&["desktop", "status"]);
    // then
    assert_eq!((stdout.as_str(), code), ("", 2));
    assert_eq!(
        stderr,
        format!(
            "recog desktop runs {}, which is not built: cargo setup builds it\n",
            repo.join("target/release/recog-desktop").display()
        )
    );
}

/// The model's file where wavo's hub finds it in a Hugging Face cache.
fn cached_model(cache: &Path) -> PathBuf {
    let snapshot = cache.join("models--handy-computer--gigaam-v3-e2e-rnnt-gguf/snapshots/0123abc");
    std::fs::create_dir_all(&snapshot).unwrap();
    let model = snapshot.join("gigaam-v3-e2e-rnnt-Q8_0.gguf");
    std::fs::write(&model, "stand-in").unwrap();
    model
}

#[test]
fn a_cached_model_when_prefetching_then_its_path_and_nothing_downloaded() {
    // given
    let sandbox = Sandbox::new();
    let cache = sandbox.path("hf-hub");
    let model = cached_model(&cache);
    // when
    let output = sandbox
        .command(&["prefetch"])
        .env("HF_HUB_CACHE", &cache)
        .env("HF_ENDPOINT", "http://127.0.0.1:9")
        .output()
        .unwrap();
    // then
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), format!("{}\n", model.display()));
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn no_cached_model_and_no_hub_when_prefetching_then_the_download_fails_with_status_1() {
    // given
    let sandbox = Sandbox::new();
    // when
    let output = sandbox
        .command(&["prefetch"])
        .env("HF_HUB_CACHE", sandbox.path("hf-hub"))
        .env("HF_ENDPOINT", "http://127.0.0.1:9")
        .output()
        .unwrap();
    // then
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        stderr.starts_with("Downloading the ASR model gigaam-v3...\nCould not download the ASR model: "),
        "{stderr}"
    );
}

#[test]
fn a_checkout_without_the_launcher_source_when_building_the_launcher_then_the_error_and_status_1() {
    // given
    let sandbox = Sandbox::new();
    let repo = std::fs::canonicalize(sandbox.path("repo")).unwrap();
    // when
    let (stdout, stderr, code) = sandbox.run(&["launcher"]);
    // then
    assert_eq!((stdout.as_str(), code), ("", 1));
    let source = repo.join("crates/recog-live/macos/recog_launcher.swift");
    assert!(stderr.starts_with(&format!("cannot read {}: ", source.display())), "{stderr}");
}

#[test]
fn a_missing_session_when_replayed_then_the_error_naming_it_and_status_1() {
    // given
    let sandbox = Sandbox::new();
    let session = sandbox.path("missing");
    // when
    let (stdout, stderr, code) =
        sandbox.run(&["replay", session.to_str().unwrap(), sandbox.path("out").to_str().unwrap()]);
    // then
    assert_eq!((stdout.as_str(), code), ("", 1));
    assert!(stderr.starts_with(&format!("{}: ", session.display())), "{stderr}");
}

#[test]
fn nothing_to_capture_when_starting_live_then_the_reason_and_status_1() {
    // given
    let sandbox = Sandbox::new();
    // when
    let (stdout, stderr, code) = sandbox.run(&["live", "--mic-device", "none", "--system-audio", "none"]);
    // then
    assert_eq!((stdout.as_str(), code), ("", 1));
    assert!(stderr.contains("нечего захватывать"), "{stderr}");
}

#[test]
fn an_unknown_system_audio_backend_when_starting_live_then_the_reason_and_status_1() {
    // given
    let sandbox = Sandbox::new();
    // when
    let (stdout, stderr, code) = sandbox.run(&["live", "--system-audio", "bogus"]);
    // then
    assert_eq!((stdout.as_str(), code), ("", 1));
    assert_eq!(stderr.lines().last(), Some("unknown system audio backend: 'bogus'"), "{stderr}");
}

#[cfg(target_os = "linux")]
#[test]
fn the_remote_backend_without_a_gateway_when_starting_live_then_the_reason_and_status_1() {
    // given
    let sandbox = Sandbox::new();
    // when
    let output = sandbox
        .command(&["live", "--mic-device", "none", "--system-audio", "device:7"])
        .env("RECOG_ASR_BACKEND", "remote")
        .output()
        .unwrap();
    // then
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr.lines().last(), Some("RECOG_ASR_BACKEND=remote requires RECOG_ASR_REMOTE_URL"), "{stderr}");
}

#[cfg(target_os = "linux")]
#[test]
fn the_sound_servers_sources_when_listing_devices_then_printed_after_the_empty_device_list() {
    // given
    let sandbox = Sandbox::new();
    sandbox.program("pactl", "echo '1\tsink.monitor\tmodule\ts16le 2ch 44100Hz\tIDLE'");
    // when
    let (stdout, _, code) = sandbox.run(&["live", "--list-devices"]);
    // then
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "Доступные аудио устройства:\n\n\nИсточники PulseAudio/PipeWire (pactl list short sources):\n1\tsink.monitor\tmodule\ts16le 2ch 44100Hz\tIDLE\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn blackhole_asked_for_on_linux_when_starting_live_then_no_system_audio_and_nothing_to_capture() {
    // given
    let sandbox = Sandbox::new();
    // when
    let (_, stderr, code) = sandbox.run(&["live", "--mic-device", "none", "--system-audio", "device:auto"]);
    // then
    assert_eq!(code, 1);
    assert!(stderr.contains("Warning: Could not find BlackHole device\n"), "{stderr}");
    assert!(stderr.contains("нечего захватывать"), "{stderr}");
}

/// Waits for a line of the child's stdout holding `needle`; the lines so far.
#[cfg(target_os = "linux")]
fn wait_for_line(reader: &mut impl std::io::BufRead, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut seen = String::new();
    while Instant::now() < deadline {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        seen.push_str(&line);
        if line.contains(needle) {
            return seen;
        }
    }
    panic!("no line with {needle:?} in {seen:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn live_on_a_silent_sound_server_when_interrupted_then_it_stops_cleanly_with_status_0() {
    // given: a sound server whose monitor plays silence, and a gateway nobody answers on
    let sandbox = Sandbox::new();
    sandbox.program("pactl", "echo stand-in-sink");
    sandbox.program("parec", "exec cat /dev/zero");
    let mut child = sandbox
        .command(&["live", "--mic-device", "none", "--system-audio", "pulse", "--no-save-audio"])
        .env("RECOG_ASR_BACKEND", "remote")
        .env("RECOG_ASR_REMOTE_URL", "http://127.0.0.1:9")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let banner = wait_for_line(&mut stdout, "Press Ctrl+C to stop", Duration::from_secs(60));
    std::thread::sleep(Duration::from_millis(500));
    // when
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap()),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut rest).unwrap();
    let status = child.wait().unwrap();
    // then
    assert!(banner.contains(" Mic: none | System: PulseAudio/PipeWire (parec)\n"), "{banner}");
    assert!(banner.contains(" Speaker detection: unavailable without conference data\n"), "{banner}");
    assert!(rest.contains("Остановлено.\n"), "{rest}");
    assert_eq!(status.code(), Some(0), "{rest}");
}
