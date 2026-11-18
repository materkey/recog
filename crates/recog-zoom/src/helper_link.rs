//! Starting the helper application and reading its snapshots.
//!
//! The helper is launched by LaunchServices (`open -g -n -a … --args --socket <path>`)
//! and therefore shares no pipe with this process: it connects back to a Unix socket
//! in a private temporary directory and exits when that socket goes away. Started as a
//! child instead, the Accessibility permission would belong to this process.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::helper_app::CommandOutcome;

/// How long the helper has to connect back after its launch.
pub const HELPER_CONNECT: Duration = Duration::from_secs(15);
/// A line longer than this is not a snapshot (the helper writes a few kilobytes).
pub const MAX_LINE_BYTES: usize = 16 << 20;

/// Starts the helper application; tests start a stand-in.
pub trait Launcher {
    /// Run the launch command (`open …`) to completion.
    fn launch(&self, command: Vec<String>) -> impl Future<Output = std::io::Result<CommandOutcome>> + Send;
}

/// `open`, the LaunchServices command line.
pub struct OpenLauncher;

impl Launcher for OpenLauncher {
    async fn launch(&self, command: Vec<String>) -> std::io::Result<CommandOutcome> {
        let output = tokio::process::Command::new(&command[0]).args(&command[1..]).output().await?;
        Ok(CommandOutcome { success: output.status.success(), stderr: String::from_utf8_lossy(&output.stderr).into() })
    }
}

/// The helper could not be started or did not connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchError(pub String);

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LaunchError {}

/// The launch command: `-n` a fresh instance bound to this socket, `-g` never comes to
/// the front; `--no-prompt` keeps a helper without the permission from showing the
/// system dialog again.
pub fn launch_command(app: &Path, socket: &Path, prompt: bool) -> Vec<String> {
    let mut command: Vec<String> = ["open", "-g", "-n", "-a"].map(String::from).into();
    command.extend([app.display().to_string(), "--args".into(), "--socket".into(), socket.display().to_string()]);
    if !prompt {
        command.push("--no-prompt".into());
    }
    command
}

/// A running helper's stream of snapshot lines. Dropping it closes the connection,
/// stops listening and removes the private directory; the helper then exits.
pub struct HelperLines {
    reader: BufReader<OwnedReadHalf>,
    pending: Vec<u8>,
    _writer: OwnedWriteHalf,
    accepting: JoinHandle<()>,
    socket: PathBuf,
    _directory: tempfile::TempDir,
}

impl HelperLines {
    /// The socket the helper was told to connect to.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// The next line with its newline, a last line without one, or `None` at the end
    /// of the stream. Cancel-safe: bytes read before a cancellation are kept.
    pub async fn next_line(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                return Ok((!self.pending.is_empty()).then(|| std::mem::take(&mut self.pending)));
            }
            let (taken, done) = match available.iter().position(|&byte| byte == b'\n') {
                Some(end) => (end + 1, true),
                None => (available.len(), false),
            };
            self.pending.extend_from_slice(&available[..taken]);
            self.reader.consume(taken);
            if done {
                return Ok(Some(std::mem::take(&mut self.pending)));
            }
            if self.pending.len() > MAX_LINE_BYTES {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "a helper line exceeds 16 MiB"));
            }
        }
    }
}

impl Drop for HelperLines {
    fn drop(&mut self) {
        self.accepting.abort();
    }
}

fn app_name(app: &Path) -> String {
    app.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Start the helper application at `app` and connect to its stream of snapshots.
pub async fn helper_lines(
    app: &Path,
    prompt: bool,
    launcher: &impl Launcher,
    connect_within: Duration,
) -> Result<HelperLines, LaunchError> {
    let io = |what: &str, error: std::io::Error| LaunchError(format!("{what}: {error}"));
    let directory = tempfile::Builder::new()
        .prefix("recog-zoom-")
        .tempdir()
        .map_err(|e| io("cannot create a socket directory", e))?;
    let socket = directory.path().join("probe.sock");
    let listener = UnixListener::bind(&socket).map_err(|e| io("cannot listen", e))?;
    let (connected, first) = oneshot::channel();
    // The first connection is the helper's; any later one is turned away.
    let accepting = tokio::spawn(async move {
        let mut connected = Some(connected);
        while let Ok((stream, _)) = listener.accept().await {
            if let Some(connected) = connected.take() {
                let _ = connected.send(stream);
            }
        }
    });
    let abort_on_error = scopeguard(accepting);
    let outcome = launcher
        .launch(launch_command(app, &socket, prompt))
        .await
        .map_err(|e| LaunchError(format!("cannot launch {}: {e}", app_name(app))))?;
    if !outcome.success {
        return Err(LaunchError(format!("cannot launch {}: {}", app_name(app), crate::py::strip(&outcome.stderr))));
    }
    let stream = match tokio::time::timeout(connect_within, first).await {
        Ok(Ok(stream)) => stream,
        _ => {
            return Err(LaunchError(format!(
                "{} did not connect within {:.0} s",
                app_name(app),
                connect_within.as_secs_f64()
            )));
        }
    };
    let (read, write) = stream.into_split();
    Ok(HelperLines {
        reader: BufReader::new(read),
        pending: Vec::new(),
        _writer: write,
        accepting: abort_on_error.defuse(),
        socket,
        _directory: directory,
    })
}

/// Ask the helper itself about Accessibility. Its first snapshot is enough:
/// closing the private socket makes this probe instance exit.
pub fn accessibility(app: &Path, prompt: bool) -> Result<bool, LaunchError> {
    accessibility_with(app, prompt, &OpenLauncher)
}

fn accessibility_with(app: &Path, prompt: bool, launcher: &impl Launcher) -> Result<bool, LaunchError> {
    let runtime =
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| LaunchError(e.to_string()))?;
    runtime.block_on(async {
        let mut lines = helper_lines(app, prompt, launcher, HELPER_CONNECT).await?;
        let line = tokio::time::timeout(HELPER_CONNECT, lines.next_line())
            .await
            .map_err(|_| LaunchError("Zoom permission probe timed out".into()))?
            .map_err(|e| LaunchError(e.to_string()))?
            .ok_or_else(|| LaunchError("Zoom helper exited before reporting its state".into()))?;
        accessibility_state(&line)
    })
}

fn accessibility_state(line: &[u8]) -> Result<bool, LaunchError> {
    let snapshot: serde_json::Value = serde_json::from_slice(line).map_err(|e| LaunchError(e.to_string()))?;
    match snapshot["state"].as_str() {
        Some("not_trusted") => Ok(false),
        Some("zoom_not_running" | "idle" | "meeting") => Ok(true),
        _ => Err(LaunchError("Zoom helper returned an unknown permission state".into())),
    }
}

/// Aborts the accept task unless defused, so an early return stops listening.
struct AbortGuard(Option<JoinHandle<()>>);

fn scopeguard(task: JoinHandle<()>) -> AbortGuard {
    AbortGuard(Some(task))
}

impl AbortGuard {
    fn defuse(mut self) -> JoinHandle<()> {
        self.0.take().expect("defused once")
    }
}

impl Drop for AbortGuard {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    struct Snapshot(&'static [u8]);
    impl Launcher for Snapshot {
        async fn launch(&self, command: Vec<String>) -> std::io::Result<CommandOutcome> {
            use tokio::io::AsyncWriteExt;
            let index = command.iter().position(|p| p == "--socket").unwrap() + 1;
            let mut writer = tokio::net::UnixStream::connect(&command[index]).await?;
            writer.write_all(self.0).await?;
            Ok(CommandOutcome { success: true, stderr: String::new() })
        }
    }

    #[test]
    fn helper_connection_when_checking_access_then_reads_its_first_snapshot_and_detects_disconnect() {
        let app = Path::new("RecogZoomHelper.app");
        assert!(!accessibility_with(app, false, &Snapshot(br#"{"state":"not_trusted"}"#)).unwrap());
        assert!(accessibility_with(app, true, &Snapshot(br#"{"state":"idle"}"#)).unwrap());
        assert!(accessibility_with(app, false, &Snapshot(b"")).is_err());
    }

    #[test]
    fn helper_snapshot_when_checking_accessibility_then_only_known_trusted_states_pass() {
        for state in ["idle", "meeting", "zoom_not_running", "not_trusted"] {
            assert_eq!(
                accessibility_state(format!(r#"{{"state":"{state}"}}"#).as_bytes()).unwrap(),
                state != "not_trusted"
            );
        }
        assert!(accessibility_state(b"{}").is_err());
    }
}
