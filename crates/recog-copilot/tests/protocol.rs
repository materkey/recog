//! The `recog-copilot-supervisor` binary as a process, against a fake bridge (a
//! WebSocket server in the test) and a fake tmux (a stub script first on PATH that
//! keeps its sessions as files and logs every call): the frames it sends and the tmux
//! commands it runs for a conference meeting, a manual session, a bridge restart and a
//! supervisor restart mid-meeting, and a dead session.
//!
//! Every scenario runs against the Rust binary. They ran against
//! `python -m recog.copilot_supervisor` too, with the same assertions and the same
//! tmux commands, until the Python supervisor was deleted.
//!
//! The environment is cleared for the child, its data directory is temporary, and
//! `systemd-run`/`systemctl` are stubs too, so neither a real Claude session nor the
//! service's tmux socket nor a user unit is ever touched.
#![forbid(unsafe_code)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

const TMUX_STUB: &str = r#"#!/bin/sh
# Fake tmux: sessions are files under $FAKE_TMUX_STATE/sessions, every call one line
# of unit-separated arguments in calls.log (one write, so parallel calls do not mix).
state="$FAKE_TMUX_STATE"
sep=$(printf '\037')
line="tmux$sep"
for arg in "$@"; do line="$line$arg$sep"; done
printf '%s\n' "$line" >> "$state/calls.log"
case "$3" in
    has-session)
        name="${5#=}"
        [ -e "$state/sessions/$name" ] && exit 0
        echo "can't find session: $name" >&2
        exit 1 ;;
    new-session)
        : > "$state/sessions/$6"
        exit 0 ;;
    kill-session)
        rm -f "$state/sessions/${5#=}"
        exit 0 ;;
    capture-pane)
        target="${6#=}"
        name="${target%:}"
        if [ ! -e "$state/sessions/$name" ]; then
            echo "can't find pane: $6" >&2
            exit 1
        fi
        cat "$state/screen"
        exit 0 ;;
esac
exit 0
"#;

/// On Linux the supervisor wraps tmux in `systemd-run`: run the wrapped command.
const SYSTEMD_RUN_STUB: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do
    case "$1" in --*) shift ;; *) break ;; esac
done
exec "$@"
"#;

const SYSTEMCTL_STUB: &str = r#"#!/bin/sh
case " $* " in *" show "*) echo not-found ;; esac
exit 0
"#;

const PROMPT_SCREEN: &str = " ▐▛███▜▌   Claude Code v2.1.197\n\n│ ❯                │\n  ? for shortcuts\n";

const WAIT: Duration = Duration::from_secs(12);

/// Which supervisor a rig runs.
#[derive(Clone, Debug)]
enum Implementation {
    Rust,
}

/// Runs `scenario` against the Rust binary.
async fn both<F, Fut>(scenario: F)
where
    F: Fn(Implementation) -> Fut,
    Fut: std::future::Future<Output = Vec<Vec<String>>>,
{
    scenario(Implementation::Rust).await;
}

struct Rig {
    implementation: Implementation,
    dir: tempfile::TempDir,
    child: Option<Child>,
    url: String,
    connections: mpsc::UnboundedReceiver<Connection>,
}

/// One accepted supervisor connection.
struct Connection {
    frames: mpsc::UnboundedReceiver<Value>,
    to_supervisor: mpsc::UnboundedSender<Message>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Rig {
    async fn new(implementation: Implementation) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["repo", "data", "home", "bin", "state/sessions"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        common::pre_change_settings(&dir.path().join("data"));
        write_script(&dir.path().join("bin/tmux"), TMUX_STUB);
        write_script(&dir.path().join("bin/systemd-run"), SYSTEMD_RUN_STUB);
        write_script(&dir.path().join("bin/systemctl"), SYSTEMCTL_STUB);
        std::fs::write(dir.path().join("state/screen"), PROMPT_SCREEN).unwrap();
        std::fs::write(dir.path().join("state/calls.log"), "").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (accepted, connections) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let Ok(socket) = tokio_tungstenite::accept_async(stream).await else { continue };
                let (mut sink, mut stream) = socket.split();
                let (frames_tx, frames) = mpsc::unbounded_channel();
                let (to_supervisor, mut outgoing) = mpsc::unbounded_channel::<Message>();
                tokio::spawn(async move {
                    while let Some(Ok(message)) = stream.next().await {
                        if let Ok(text) = message.to_text()
                            && let Ok(value) = serde_json::from_str::<Value>(text)
                        {
                            let _ = frames_tx.send(value);
                        }
                    }
                });
                tokio::spawn(async move {
                    while let Some(message) = outgoing.recv().await {
                        let close = matches!(message, Message::Close(_));
                        if sink.send(message).await.is_err() || close {
                            break;
                        }
                    }
                });
                let _ = accepted.send(Connection { frames, to_supervisor });
            }
        });
        Rig { implementation, dir, child: None, url, connections }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.dir.path().join(sub)
    }

    /// A live session left by a previous supervisor.
    fn leave_session(&self, name: &str) {
        std::fs::write(self.path("state/sessions").join(name), "").unwrap();
    }

    fn end_session(&self, name: &str) {
        let _ = std::fs::remove_file(self.path("state/sessions").join(name));
    }

    /// The checkout the supervisor runs Claude in.
    fn repo(&self) -> String {
        match &self.implementation {
            Implementation::Rust => self.path("repo").display().to_string(),
        }
    }

    fn start(&mut self, env: &[(&str, &str)]) {
        let path = format!("{}:/usr/bin:/bin", self.path("bin").display());
        let mut command = match &self.implementation {
            Implementation::Rust => {
                let mut command = Command::new(env!("CARGO_BIN_EXE_recog-copilot-supervisor"));
                command.args(["--bridge-url", &self.url, "--repo-root"]).arg(self.path("repo"));
                command
            }
        };
        // Set explicitly, so that a checkout's .env (which never overrides the
        // environment) changes neither side.
        let pinned = [
            ("RECOG_COPILOT_AUTOSPAWN", "1"),
            ("RECOG_COPILOT_GRACE_S", "90"),
            ("RECOG_COPILOT_MCP_URL", "http://127.0.0.1:8767/mcp"),
            ("CLAUDE_MODEL", ""),
            ("CODEX_MODEL", ""),
            ("CODEX_REASONING_EFFORT", ""),
        ];
        command
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.path("home"))
            .env("RECOG_DATA_DIR", self.path("data"))
            .env("RECOG_CONFIG_FILE", "/nonexistent/recog/config.toml")
            .env("RECOG_CONFERENCE_HOSTS_FILE", "/nonexistent/recog/conference-hosts.json")
            .env("FAKE_TMUX_STATE", self.path("state"))
            .envs(pinned)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(self.path("supervisor.log")).unwrap());
        self.child = Some(command.spawn().unwrap());
    }

    /// The commands that create and kill sessions, the checkout and the Claude command
    /// left out (the Python module runs in its own checkout).
    fn session_commands(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.get(3).is_some_and(|v| v == "new-session" || v == "kill-session"))
            .map(|c| if c[3] == "new-session" { c[..7].to_vec() } else { c })
            .collect()
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.path("supervisor.log")).unwrap_or_default()
    }

    async fn connection(&mut self) -> Connection {
        match tokio::time::timeout(WAIT, self.connections.recv()).await {
            Ok(Some(connection)) => connection,
            _ => panic!("the supervisor did not connect; its log:\n{}", self.log()),
        }
    }

    /// Every tmux call so far, as argument lists.
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.path("state/calls.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| line.split('\u{1f}').filter(|a| !a.is_empty()).map(str::to_owned).collect())
            .collect()
    }

    fn verb_calls(&self, verb: &str) -> Vec<Vec<String>> {
        self.calls().into_iter().filter(|c| c.get(3).is_some_and(|v| v == verb)).collect()
    }

    async fn wait_calls(&self, verb: &str, count: usize) -> Instant {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if self.verb_calls(verb).len() >= count {
                return Instant::now();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no {count} × {verb}; calls: {:?}\nlog:\n{}", self.calls(), self.log());
    }
}

impl Connection {
    fn send(&self, frame: Value) {
        self.to_supervisor.send(Message::text(frame.to_string())).unwrap();
    }

    fn close_going_away(&self) {
        let frame = CloseFrame { code: CloseCode::Away, reason: "bridge restart".into() };
        let _ = self.to_supervisor.send(Message::Close(Some(frame)));
    }

    async fn next(&mut self) -> Value {
        match tokio::time::timeout(WAIT, self.frames.recv()).await {
            Ok(Some(frame)) => frame,
            other => panic!("no frame from the supervisor: {other:?}"),
        }
    }

    /// Skips frames until a `copilot_agent_status` with this status and meeting.
    async fn status(&mut self, status: &str, meeting_id: Value) -> Instant {
        loop {
            let frame = self.next().await;
            assert_eq!(frame["type"], "copilot_agent_status", "unexpected frame {frame}");
            if frame["status"] == status && frame["meeting_id"] == meeting_id {
                return Instant::now();
            }
        }
    }
}

fn hello(meeting_id: Value) -> Value {
    json!({"type": "supervisor_hello", "current_meeting_id": meeting_id})
}

#[tokio::test]
async fn conference_meeting_spawns_the_session_and_its_end_kills_it_after_the_grace() {
    both(conference_meeting).await;
}

async fn conference_meeting(implementation: Implementation) -> Vec<Vec<String>> {
    // given
    let mut rig = Rig::new(implementation).await;
    rig.start(&[("RECOG_COPILOT_GRACE_S", "1"), ("RECOG_COPILOT_AUTOSPAWN", "1")]);
    let mut bridge = rig.connection().await;
    assert_eq!(bridge.next().await, hello(Value::Null));
    bridge.send(json!({"type": "meeting_state", "active": false, "meeting_id": null, "meeting_title": null}));
    // when: the meeting starts
    bridge.send(json!({"type": "meeting_started", "meeting_id": "A", "meeting_title": "Standup"}));
    rig.wait_calls("new-session", 1).await;
    // then: a `copilot` session in the checkout running the Copilot agent, reported online
    let created = &rig.verb_calls("new-session")[0];
    let repo = rig.repo();
    assert_eq!(
        created[..9],
        ["tmux", "-L", "recog-copilot", "new-session", "-d", "-s", "copilot", "-c", repo.as_str()]
    );
    assert!(created[9].starts_with("claude --agent copilot-session --model claude-opus-5-5 "), "{}", created[9]);
    bridge.status("online", json!("A")).await;
    // when: it ends
    bridge.send(json!({"type": "meeting_ended", "meeting_id": "A"}));
    let ended = Instant::now();
    let killed = rig.wait_calls("kill-session", 1).await;
    // then: killed once the grace is over, the chip closed, nothing respawned
    let waited = killed.duration_since(ended);
    assert!(waited >= Duration::from_millis(900) && waited < Duration::from_secs(4), "{waited:?}");
    assert_eq!(rig.verb_calls("kill-session")[0][4..], ["-t", "=copilot"]);
    bridge.status("offline", Value::Null).await;
    assert_eq!(rig.verb_calls("new-session").len(), 1);
    rig.session_commands()
}

#[tokio::test]
async fn manual_spawn_and_stop_from_the_webapp_create_and_kill_the_manual_session_at_once() {
    both(manual_session).await;
}

async fn manual_session(implementation: Implementation) -> Vec<Vec<String>> {
    // given: autospawn off, which gates only the meeting frames
    let mut rig = Rig::new(implementation).await;
    rig.start(&[("RECOG_COPILOT_GRACE_S", "30"), ("RECOG_COPILOT_AUTOSPAWN", "0")]);
    let mut bridge = rig.connection().await;
    assert_eq!(bridge.next().await, hello(Value::Null));
    // when
    bridge.send(json!({"type": "copilot_spawn_request", "request_id": "r1"}));
    rig.wait_calls("new-session", 1).await;
    bridge.status("online", json!("manual")).await;
    bridge.send(json!({"type": "copilot_stop_request", "request_id": "r2"}));
    let asked = Instant::now();
    let killed = rig.wait_calls("kill-session", 1).await;
    // then: the manual name both times, the stop without the 30 s grace
    assert_eq!(rig.verb_calls("new-session")[0][6], "copilot-manual");
    assert_eq!(rig.verb_calls("kill-session")[0][4..], ["-t", "=copilot-manual"]);
    assert!(killed.duration_since(asked) < Duration::from_millis(1500));
    bridge.status("offline", Value::Null).await;
    assert!(rig.log().contains("manual stop request 'r2' — killing session now"), "{}", rig.log());
    rig.session_commands()
}

#[tokio::test]
async fn bridge_restart_mid_meeting_reconnects_with_the_meeting_and_keeps_the_session() {
    both(bridge_restart).await;
}

async fn bridge_restart(implementation: Implementation) -> Vec<Vec<String>> {
    // given: a running conference session
    let mut rig = Rig::new(implementation).await;
    rig.start(&[("RECOG_COPILOT_GRACE_S", "2")]);
    let mut first = rig.connection().await;
    assert_eq!(first.next().await, hello(Value::Null));
    first.send(json!({"type": "meeting_started", "meeting_id": "A"}));
    rig.wait_calls("new-session", 1).await;
    first.status("online", json!("A")).await;
    // when: the bridge goes away and comes back, first without the meeting
    first.close_going_away();
    let mut second = rig.connection().await;
    assert_eq!(second.next().await, hello(json!("A")));
    second.send(json!({"type": "meeting_state", "active": false, "meeting_id": null}));
    tokio::time::sleep(Duration::from_millis(300)).await;
    second.send(json!({"type": "meeting_started", "meeting_id": "A"}));
    tokio::time::sleep(Duration::from_secs(3)).await; // past the grace
    // then: the same session lives on, reported online on the new connection
    assert!(rig.verb_calls("kill-session").is_empty(), "{:?}", rig.calls());
    assert_eq!(rig.verb_calls("new-session").len(), 1);
    second.status("online", json!("A")).await;
    assert!(rig.log().contains(&format!("reconnected to {}", rig.url)), "{}", rig.log());
    rig.session_commands()
}

#[tokio::test]
async fn supervisor_restart_mid_conference_adopts_the_live_session_without_spawn_or_kill() {
    both(supervisor_restart_conference).await;
}

async fn supervisor_restart_conference(implementation: Implementation) -> Vec<Vec<String>> {
    // given: a `copilot` session the previous supervisor left running
    let mut rig = Rig::new(implementation).await;
    rig.leave_session("copilot");
    rig.start(&[("RECOG_COPILOT_GRACE_S", "2")]);
    let mut bridge = rig.connection().await;
    // when: the bridge reports the meeting as active
    assert_eq!(bridge.next().await, hello(Value::Null));
    bridge.send(json!({"type": "meeting_state", "active": true, "meeting_id": "A", "meeting_title": "Standup"}));
    // then
    bridge.status("online", json!("A")).await;
    assert!(rig.verb_calls("new-session").is_empty(), "{:?}", rig.calls());
    assert!(rig.verb_calls("kill-session").is_empty(), "{:?}", rig.calls());
    rig.session_commands()
}

#[tokio::test]
async fn supervisor_restart_during_a_manual_session_readopts_it_and_says_so_in_the_hello() {
    both(supervisor_restart_manual).await;
}

async fn supervisor_restart_manual(implementation: Implementation) -> Vec<Vec<String>> {
    // given: a `copilot-manual` session left running, autospawn off
    let mut rig = Rig::new(implementation).await;
    rig.leave_session("copilot-manual");
    rig.start(&[("RECOG_COPILOT_AUTOSPAWN", "0")]);
    let mut bridge = rig.connection().await;
    // when / then: the hello carries "manual" before any frame, the bridge confirms it
    assert_eq!(bridge.next().await, hello(json!("manual")));
    bridge.send(json!({"type": "meeting_state", "active": true, "meeting_id": "manual"}));
    bridge.status("online", json!("manual")).await;
    assert!(rig.verb_calls("new-session").is_empty() && rig.verb_calls("kill-session").is_empty());
    assert_eq!(rig.verb_calls("has-session")[0][4..], ["-t", "=copilot-manual"]);
    assert!(rig.log().contains("re-adopted live manual copilot session (copilot-manual)"), "{}", rig.log());
    rig.session_commands()
}

#[tokio::test]
async fn dead_session_is_reported_offline_and_respawned_after_the_five_second_backoff() {
    both(dead_session).await;
}

async fn dead_session(implementation: Implementation) -> Vec<Vec<String>> {
    // given: a running conference session
    let mut rig = Rig::new(implementation).await;
    rig.start(&[]);
    let mut bridge = rig.connection().await;
    assert_eq!(bridge.next().await, hello(Value::Null));
    bridge.send(json!({"type": "meeting_started", "meeting_id": "A"}));
    rig.wait_calls("new-session", 1).await;
    bridge.status("online", json!("A")).await;
    // when: the session dies
    rig.end_session("copilot");
    let offline = bridge.status("offline", json!("A")).await;
    let respawned = rig.wait_calls("new-session", 2).await;
    // then
    let backoff = respawned.duration_since(offline);
    assert!(backoff >= Duration::from_millis(4500) && backoff < Duration::from_secs(8), "{backoff:?}");
    assert_eq!(rig.verb_calls("new-session")[1][6], "copilot");
    bridge.status("online", json!("A")).await;
    rig.stop();
    rig.session_commands()
}
