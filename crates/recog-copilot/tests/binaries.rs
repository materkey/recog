//! The two binaries as the service units run them: their help, argument errors and
//! exit codes, a malformed installation stopping them, and the channel serving until
//! SIGTERM or SIGINT. Free ports and temporary directories only; the supervisor is
//! never run past its configuration (it would probe the real tmux).
#![forbid(unsafe_code)]

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;

const CHANNEL: &str = env!("CARGO_BIN_EXE_recog-copilot-channel");
const SUPERVISOR: &str = env!("CARGO_BIN_EXE_recog-copilot-supervisor");

/// The user's live services: never bind or reach these.
const LIVE_PORTS: [u16; 8] = [8765, 8766, 8767, 9080, 8770, 9001, 9002, 9003];

/// A binary with a temporary checkout and data directory and no installation file.
fn command(binary: &str, dir: &Path) -> Command {
    std::fs::create_dir_all(dir.join("repo")).unwrap();
    std::fs::create_dir_all(dir.join("data")).unwrap();
    let mut command = Command::new(binary);
    command
        .env("RECOG_DATA_DIR", dir.join("data"))
        .env("RECOG_CONFIG_FILE", dir.join("missing-config.toml"))
        .env("RECOG_CONFERENCE_HOSTS_FILE", dir.join("missing-hosts.json"))
        .env_remove("RECOG_COPILOT_AUTOSPAWN")
        .env_remove("RECOG_COPILOT_GRACE_S")
        .env_remove("RECOG_COPILOT_MCP_URL")
        .stdin(Stdio::null());
    command
}

fn run(binary: &str, args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    command(binary, dir.path()).args(args).arg("--repo-root").arg(&repo).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const CHANNEL_USAGE: &str = "usage: recog-copilot-channel [-h] [--bridge-url BRIDGE_URL] [--mcp-host MCP_HOST]
                             [--mcp-port MCP_PORT] [--repo-root REPO_ROOT]";

#[test]
fn help_when_asked_then_argparse_usage_on_stdout_and_success() {
    for (binary, args) in [(CHANNEL, ["-h"]), (CHANNEL, ["--help"]), (SUPERVISOR, ["--help"])] {
        // given / when
        let output = run(binary, &args);
        // then
        assert_eq!(output.status.code(), Some(0), "{binary} {args:?}");
        let stdout = text(&output.stdout);
        assert!(stdout.starts_with("usage: recog-copilot-"), "{stdout}");
        assert!(stdout.contains("  -h, --help            show this help message and exit"), "{stdout}");
        assert!(stdout.contains("--repo-root REPO_ROOT"), "{stdout}");
    }
}

#[test]
fn bad_channel_arguments_when_given_then_usage_and_error_with_status_2() {
    // given
    let cases: [(&[&str], &str); 5] = [
        (&["--bogus"], "unrecognized arguments: --bogus"),
        (&["--bogus=1"], "unrecognized arguments: --bogus=1"),
        (&["-x=1"], "unrecognized arguments: -x=1"),
        (&["--mcp-port=abc"], "argument --mcp-port: invalid int value: 'abc'"),
        (&["--mcp-port", "70000"], "argument --mcp-port: invalid int value: '70000'"),
    ];
    for (args, error) in cases {
        // when
        let output = run(CHANNEL, args);
        // then
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            text(&output.stderr),
            format!("{CHANNEL_USAGE}\nrecog-copilot-channel: error: {error}\n"),
            "{args:?}"
        );
    }
}

#[test]
fn flag_without_value_when_last_then_expected_one_argument() {
    for (binary, flag) in
        [(CHANNEL, "--bridge-url"), (CHANNEL, "--mcp-host"), (CHANNEL, "--mcp-port"), (SUPERVISOR, "--bridge-url")]
    {
        // given
        let dir = tempfile::tempdir().unwrap();
        // when
        let output = command(binary, dir.path()).arg(flag).output().unwrap();
        // then
        assert_eq!(output.status.code(), Some(2), "{binary} {flag}");
        let stderr = text(&output.stderr);
        assert!(stderr.ends_with(&format!(": error: argument {flag}: expected one argument\n")), "{stderr}");
    }
    // given / when
    let dir = tempfile::tempdir().unwrap();
    let output = command(CHANNEL, dir.path()).arg("--repo-root").output().unwrap();
    // then
    assert!(text(&output.stderr).ends_with("error: argument --repo-root: expected one argument\n"));
}

#[test]
fn bad_supervisor_argument_when_given_then_one_usage_line_and_error_with_status_2() {
    // given / when
    let output = run(SUPERVISOR, &["--mcp-port", "1"]);
    // then
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        text(&output.stderr),
        "usage: recog-copilot-supervisor [foreground] [-h] [--bridge-url BRIDGE_URL] [--repo-root REPO_ROOT]\n\
         recog-copilot-supervisor: error: unrecognized arguments: --mcp-port\n"
    );
}

#[test]
fn malformed_installation_config_when_starting_then_named_error_and_status_1() {
    for (binary, name) in [(CHANNEL, "recog-copilot-channel"), (SUPERVISOR, "recog-copilot-supervisor")] {
        // given
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "= =\n").unwrap();
        // when
        let output = command(binary, dir.path())
            .env("RECOG_CONFIG_FILE", &config)
            .arg("--repo-root")
            .arg(dir.path().join("repo"))
            .output()
            .unwrap();
        // then
        assert_eq!(output.status.code(), Some(1), "{binary}");
        let stderr = text(&output.stderr);
        assert!(stderr.starts_with(&format!("{name}: {}: ", config.display())), "{stderr}");
    }
}

#[test]
fn relative_data_dir_when_starting_then_named_error_and_status_1() {
    for (binary, name) in [(CHANNEL, "recog-copilot-channel"), (SUPERVISOR, "recog-copilot-supervisor")] {
        // given
        let dir = tempfile::tempdir().unwrap();
        // when
        let output = command(binary, dir.path())
            .env("RECOG_DATA_DIR", "relative/data")
            .arg("--repo-root")
            .arg(dir.path().join("repo"))
            .output()
            .unwrap();
        // then
        assert_eq!(output.status.code(), Some(1), "{binary}");
        assert_eq!(
            text(&output.stderr),
            format!("{name}: RECOG_DATA_DIR must be an absolute path (or start with ~/)\n"),
            "{binary}"
        );
    }
}

#[test]
fn foreground_copilot_when_started_then_claude_runs_in_the_checkout_and_owns_the_exit_code() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let claude = dir.path().join("claude");
    std::fs::write(&claude, "#!/bin/sh\npwd -P\nprintf '%s\\n' \"$@\"\nexit 17\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = command(SUPERVISOR, dir.path())
        .env("PATH", dir.path())
        .args(["foreground", "--repo-root"])
        .arg(&repo)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(17), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert_eq!(stdout.lines().next(), repo.canonicalize().unwrap().to_str());
    assert!(stdout.contains("--agent\ncopilot-session\n"));
    assert!(stdout.contains("--strict-mcp-config\n--mcp-config\n"));
    assert!(stdout.ends_with("server:copilot\n"));
}

fn free_port() -> u16 {
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        if !LIVE_PORTS.contains(&port) {
            return port;
        }
    }
}

/// The channel serving on a free port with a fake bridge; the bridge has received
/// the hello when this returns.
struct Running {
    child: tokio::process::Child,
    port: u16,
    log: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn start_channel() -> Running {
    let bridge = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bridge_url = format!("ws://{}", bridge.local_addr().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let log = dir.path().join("channel.log");
    let mut command = command(CHANNEL, dir.path());
    command
        .arg(format!("--bridge-url={bridge_url}"))
        .args(["--mcp-host", "127.0.0.1", "--mcp-port", &format!(" {port} ")])
        .arg(format!("--repo-root={}", dir.path().join("repo").display()))
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap());
    let child = tokio::process::Command::from(command).kill_on_drop(true).spawn().unwrap();
    let (stream, _) = tokio::time::timeout(Duration::from_secs(20), bridge.accept()).await.unwrap().unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(hello.to_text().unwrap(), r#"{"type": "copilot_channel_hello"}"#);
    tokio::spawn(async move { while socket.next().await.is_some() {} });
    let deadline = Instant::now() + Duration::from_secs(20);
    while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
        assert!(Instant::now() < deadline, "the channel did not listen");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Running { child, port, log, _dir: dir }
}

fn signal(child: &tokio::process::Child, name: &str) {
    let pid = child.id().unwrap().to_string();
    assert!(Command::new("kill").args([&format!("-{name}"), &pid]).status().unwrap().success());
}

#[tokio::test]
async fn running_channel_when_terminated_then_exits_cleanly() {
    // given
    let mut running = start_channel().await;
    // when
    signal(&running.child, "TERM");
    let status = tokio::time::timeout(Duration::from_secs(10), running.child.wait()).await.unwrap().unwrap();
    // then
    assert_eq!(status.code(), Some(0));
    let log = std::fs::read_to_string(&running.log).unwrap();
    assert!(
        log.contains(&format!(
            "[copilot-channel] MCP Streamable HTTP listening on http://127.0.0.1:{}/mcp",
            running.port
        )),
        "{log}"
    );
    assert!(log.contains("[copilot-channel] connected to ws://127.0.0.1:"), "{log}");
    assert!(!log.contains("interrupted"), "{log}");
}

#[tokio::test]
async fn running_channel_when_interrupted_then_says_so_and_exits_cleanly() {
    // given
    let mut running = start_channel().await;
    // when
    signal(&running.child, "INT");
    let status = tokio::time::timeout(Duration::from_secs(10), running.child.wait()).await.unwrap().unwrap();
    // then
    assert_eq!(status.code(), Some(0));
    let log = std::fs::read_to_string(&running.log).unwrap();
    assert!(log.ends_with("[copilot-channel] interrupted, exiting\n"), "{log}");
}

#[test]
fn port_taken_when_channel_starts_then_task_crash_and_status_1() {
    // given
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bridge = format!("ws://{}", closed.local_addr().unwrap());
    drop(closed);
    // when
    let output = run(CHANNEL, &["--mcp-port", &port, "--bridge-url", &bridge]);
    // then
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("[copilot-channel] task crashed: Address already in use"), "{stderr}");
}
