//! `recog-web` as the service runs it: its arguments, the exit codes of a bad
//! configuration or an address in use, and a clean stop on SIGTERM.

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The binary over a temporary home, data directory and checkout, the bridge on a
/// dead port: nothing of the installation is read.
fn command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recog-web"));
    command.args(args).env_clear();
    // A coverage run collects the binary's profile through this variable.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("RECOG_DATA_DIR", dir.join("data"))
        .env("RECOG_CONFIG_FILE", dir.join("config.toml"))
        .env("RECOG_BRIDGE_WS_URL", "ws://127.0.0.1:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run(dir: &Path, args: &[&str]) -> Output {
    command(dir, args).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn repo(dir: &Path) -> String {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    repo.to_string_lossy().into_owned()
}

#[test]
fn bad_arguments_when_started_then_exit_two_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    for (args, message) in [
        (&["--bogus"][..], "unknown argument --bogus"),
        (&["--port"][..], "--port needs a value"),
        (&["--port", "x"][..], "--port: invalid digit found in string"),
        (&["--help"][..], "usage: recog-web [--host HOST] [--port PORT] [--repo DIR]"),
        (&["-h"][..], "usage: recog-web [--host HOST] [--port PORT] [--repo DIR]"),
    ] {
        let output = run(dir.path(), args);

        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(stderr(&output).trim(), message, "{args:?}");
    }
}

#[test]
fn malformed_installation_configuration_when_started_then_exit_one() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "[web\n").unwrap();
    let repo = repo(dir.path());

    let output = run(dir.path(), &["--repo", &repo, "--port", &free_port().to_string()]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(!stderr(&output).is_empty());
}

#[test]
fn address_in_use_when_started_then_exit_one_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let repo = repo(dir.path());

    let output = run(dir.path(), &["--repo", &repo, "--host", "127.0.0.1", "--port", &port]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(&format!("error while attempting to bind on address (127.0.0.1, {port})")),
        "{}",
        stderr(&output)
    );
}

fn wait_listening(child: &mut Child, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(child.try_wait().unwrap().is_none(), "the server exited");
        assert!(Instant::now() < deadline, "the server did not listen");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The settings file once the server listens on an empty port, stopped afterwards.
fn settings_after_start(dir: &Path) -> String {
    let port = free_port();
    let repo = repo(dir);
    let mut child = command(dir, &["--repo", &repo, "--port", &port.to_string()]).spawn().unwrap();
    wait_listening(&mut child, port);
    let settings = std::fs::read_to_string(dir.join("data/ai-settings.json")).unwrap_or_default();
    child.kill().unwrap();
    child.wait().unwrap();
    settings
}

#[test]
fn fresh_data_dir_when_server_starts_then_access_unasked_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_after_start(dir.path());
    assert!(settings.contains(r#""access": "unasked""#), "{settings}");
}

#[test]
fn data_dir_with_a_finished_meeting_when_server_starts_then_a_file_without_access_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let meeting = dir.path().join("data/meetings/m1");
    std::fs::create_dir_all(&meeting).unwrap();
    std::fs::write(meeting.join("meta.json"), r#"{"id": "m1", "ended_at": "2026-11-01T10:00:00"}"#).unwrap();
    let settings = settings_after_start(dir.path());
    assert!(settings.contains(r#""revision""#) && !settings.contains("access"), "{settings}");
}

#[test]
fn running_server_when_terminated_then_it_stops_with_exit_zero() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let repo = repo(dir.path());
    let mut child = command(dir.path(), &["--repo", &repo, "--port", &port.to_string()]).spawn().unwrap();
    wait_listening(&mut child, port);

    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();

    assert!(killed.success());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}
