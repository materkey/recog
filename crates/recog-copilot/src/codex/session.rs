//! One Copilot session on Codex (plan D1–D5): `codex app-server` as a child with its
//! own `CODEX_HOME`, one thread, the channel's events as turns, and the pane the
//! supervisor reads, whose last line is always the status.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use recog_ai::provider::{CODEX_ISOLATION, codex_effort};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::app_server::{AppServer, Event};
use super::events::{ChannelClient, CopilotEvent};
use super::harness::{Command, Harness, Kind, Status};

/// How long one `await_copilot_event` call waits.
const POLL_S: u64 = 25;

pub struct Options {
    /// The `codex` CLI.
    pub codex: PathBuf,
    /// The Copilot's own `CODEX_HOME`, holding a link to the user's login.
    pub codex_home: PathBuf,
    /// The thread's working directory, without an `AGENTS.md`.
    pub cwd: PathBuf,
    pub mcp_url: String,
    pub instructions: String,
    pub model: String,
    pub reasoning_effort: String,
    /// Where the app-server's own stderr goes.
    pub stderr: Option<PathBuf>,
}

/// The pane: lines of the dialog above, the status below them.
pub struct Pane<W: Write> {
    out: W,
    status: String,
}

impl<W: Write> Pane<W> {
    pub fn new(out: W) -> Self {
        Pane { out, status: Status::Spawning.line() }
    }

    pub fn say(&mut self, text: &str) {
        let _ = write!(self.out, "\r\x1b[2K{text}\n{}", self.status);
        let _ = self.out.flush();
    }

    pub fn status(&mut self, status: &Status) {
        self.status = status.line();
        let _ = write!(self.out, "\r\x1b[2K{}", self.status);
        let _ = self.out.flush();
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

/// `CODEX_HOME` for the Copilot with `auth.json` linked to the user's, so it keeps the
/// login while none of the user's instructions or MCP servers reach the meeting.
///
/// # Errors
/// A directory or the link that cannot be made.
pub fn prepare_home(codex_home: &Path, user_home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(codex_home)?;
    let link = codex_home.join("auth.json");
    if std::fs::symlink_metadata(&link).is_err() {
        std::os::unix::fs::symlink(user_home.join("auth.json"), &link)?;
    }
    Ok(())
}

/// `thread/start`'s parameters: no approvals, read-only, not saved, the channel as the
/// only MCP server, its tools allowed.
pub fn thread_params(options: &Options) -> Value {
    let mut config = json!({
        "mcp_servers.copilot.url": options.mcp_url,
        "mcp_servers.copilot.default_tools_approval_mode": "approve",
    });
    if let Some(effort) = codex_effort(&options.model, Some(&options.reasoning_effort)) {
        config["model_reasoning_effort"] = json!(effort);
    }
    json!({
        "model": options.model,
        "cwd": options.cwd,
        "approvalPolicy": "never",
        "sandbox": "read-only",
        "ephemeral": true,
        "developerInstructions": options.instructions,
        "config": config,
    })
}

/// The session until the meeting ends (`Ok`) or Codex stops (`Err`).
///
/// # Errors
/// Codex that cannot start or stops, or a broken protocol.
pub async fn run<W: Write>(options: &Options, pane: &mut Pane<W>) -> Result<(), String> {
    let mut child = spawn(options)?;
    let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
        return Err("codex app-server has no pipes".into());
    };
    let mut app = AppServer::new(stdout, stdin);
    let result = serve(&mut app, options, pane).await;
    drop(app); // closes stdin: the app-server shuts down
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    result
}

/// `codex app-server` with the isolation switches, in its own `CODEX_HOME`.
fn spawn(options: &Options) -> Result<tokio::process::Child, String> {
    let mut command = tokio::process::Command::new(&options.codex);
    command.arg("app-server");
    for setting in CODEX_ISOLATION {
        command.args(["-c", setting]);
    }
    let stderr = options.stderr.as_ref().and_then(|path| std::fs::File::create(path).ok());
    command
        .env("CODEX_HOME", &options.codex_home)
        .current_dir(&options.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr.map_or_else(Stdio::null, Stdio::from))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", options.codex.display()))
}

/// Stops the channel poller when the session ends, however it ends.
struct Poller(tokio::task::JoinHandle<()>);

impl Drop for Poller {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The thread, fed by the channel's events, until the harness exits.
async fn serve<R, T, W>(app: &mut AppServer<R, T>, options: &Options, pane: &mut Pane<W>) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
    T: tokio::io::AsyncWrite + Unpin,
    W: Write,
{
    let fail = |e: super::app_server::Error| e.to_string();
    app.initialize().await.map_err(fail)?;
    let logged_in = app.logged_in().await.map_err(fail)?;
    let params = thread_params(options);
    let mut thread = app.start_thread(params.clone()).await.map_err(fail)?;
    let mut harness = Harness::new();
    harness.ready(logged_in);
    pane.status(harness.status());
    let (events, mut received) = mpsc::unbounded_channel();
    let _poller = Poller(tokio::spawn(poll(options.mcp_url.clone(), events)));
    loop {
        let commands = tokio::select! {
            event = received.recv() => from_channel(&mut harness, event)?,
            event = app.next_event() => from_codex(&mut harness, pane, event)?,
        };
        if execute(app, &mut thread, &params, commands, pane).await? {
            return Ok(());
        }
        pane.status(harness.status());
    }
}

fn from_channel(harness: &mut Harness, event: Option<CopilotEvent>) -> Result<Vec<Command>, String> {
    let event = event.ok_or("the channel poller stopped")?;
    Ok(harness.on_copilot(Kind::of(&event.event_type), event.text))
}

fn from_codex<W: Write>(
    harness: &mut Harness,
    pane: &mut Pane<W>,
    event: Result<Option<Event>, super::app_server::Error>,
) -> Result<Vec<Command>, String> {
    let event = event.map_err(|e| e.to_string())?.ok_or("codex app-server exited")?;
    show(pane, &event);
    Ok(harness.on_app(&event))
}

/// Runs the harness's commands; `true` once it says to exit.
async fn execute<R, T, W>(
    app: &mut AppServer<R, T>,
    thread: &mut String,
    params: &Value,
    commands: Vec<Command>,
    pane: &mut Pane<W>,
) -> Result<bool, String>
where
    R: tokio::io::AsyncRead + Unpin,
    T: tokio::io::AsyncWrite + Unpin,
    W: Write,
{
    for command in commands {
        match command {
            Command::StartTurn(text) => {
                if let Err(error) = app.start_turn(thread, &text).await {
                    pane.say(&format!("turn not started: {error}"));
                }
            }
            Command::NewThread => *thread = app.start_thread(params.clone()).await.map_err(|e| e.to_string())?,
            Command::Exit => return Ok(true),
        }
    }
    Ok(false)
}

fn show<W: Write>(pane: &mut Pane<W>, event: &Event) {
    match event {
        Event::AgentMessage { text } if !text.trim().is_empty() => pane.say(&format!("codex: {text}")),
        Event::Refused { method } => pane.say(&format!("refused: {method}")),
        Event::Error { failure, will_retry } => {
            pane.say(&format!("error ({}): {}", if *will_retry { "retrying" } else { "final" }, failure.message));
        }
        _ => {}
    }
}

/// The channel's events, forever: reconnects when the channel restarts.
async fn poll(url: String, events: mpsc::UnboundedSender<CopilotEvent>) {
    let Ok(mut client) = ChannelClient::new(&url) else { return };
    loop {
        if client.connect().await.is_ok() {
            loop {
                match client.next_event(POLL_S).await {
                    Ok(Some(event)) => {
                        if events.send(event).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
