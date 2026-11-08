//! The Python channel (`python -m recog.copilot_channel`) and the Rust binary side by
//! side: each on a free port, with its own fake bridge (a WebSocket server in the test)
//! and its own temporary data directory, driven through the same conversation over
//! HTTP. Compared: every HTTP answer (status, headers, body bytes with session ids
//! masked), the initialization result, the tool list, every tool call, the channel
//! notifications on the GET stream, the frames each sent to its bridge, and the
//! `[copilot-channel]` log lines people grep. A second test runs the official Python
//! SDK client against both.
//!
//! The interpreter is `RECOG_ORACLE_PYTHON` (scripts/check.sh sets it), else
//! `venv-gigaam/bin/python` of the checkout; without either the tests say so and pass.
//! Never the user's services: free ports only, temporary data only.
#![forbid(unsafe_code)]

mod channel_support;
mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use channel_support::{ACCEPT, JSON, request, sse_data};
use common::oracle_python;
use futures_util::{SinkExt as _, StreamExt as _};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// The user's live services: never bind or reach these.
const LIVE_PORTS: [u16; 4] = [8765, 8766, 8767, 9080];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

async fn free_port() -> u16 {
    loop {
        let port = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        if !LIVE_PORTS.contains(&port) {
            return port;
        }
    }
}

/// The fake bridge of one channel: frames received, snapshot answers, pushes, and
/// dropping the connection to make the channel reconnect.
struct Bridge {
    url: String,
    frames: mpsc::UnboundedReceiver<Value>,
    push: mpsc::UnboundedSender<String>,
    answer_snapshots: Arc<Mutex<bool>>,
    drop_connection: mpsc::UnboundedSender<()>,
}

impl Bridge {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (frames_tx, frames) = mpsc::unbounded_channel();
        let (push, push_rx) = mpsc::unbounded_channel::<String>();
        let (drop_connection, drop_rx) = mpsc::unbounded_channel::<()>();
        let answer_snapshots = Arc::new(Mutex::new(true));
        let answer = answer_snapshots.clone();
        let push_rx = Arc::new(tokio::sync::Mutex::new(push_rx));
        let drop_rx = Arc::new(tokio::sync::Mutex::new(drop_rx));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let Ok(socket) = tokio_tungstenite::accept_async(stream).await else { continue };
                let (mut sink, mut stream) = socket.split();
                let mut push_rx = push_rx.lock().await;
                let mut drop_rx = drop_rx.lock().await;
                loop {
                    tokio::select! {
                        frame = stream.next() => {
                            let text = match frame {
                                Some(Ok(Message::Text(text))) => text,
                                // Pings (the Python client sends one every 20 s) and pongs.
                                Some(Ok(_)) => continue,
                                _ => break,
                            };
                            let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                            let _ = frames_tx.send(frame.clone());
                            if frame["type"] == "get_recent_transcript_request" && *answer.lock().unwrap() {
                                let reply = snapshot_answer(&frame);
                                if sink.send(Message::text(reply.to_string())).await.is_err() {
                                    break;
                                }
                            }
                        }
                        text = push_rx.recv() => {
                            let Some(text) = text else { break };
                            if sink.send(Message::text(text)).await.is_err() {
                                break;
                            }
                        }
                        _ = drop_rx.recv() => break,
                    }
                }
            }
        });
        Bridge { url, frames, push, answer_snapshots, drop_connection }
    }

    /// The next frame, within `seconds`.
    async fn next(&mut self, seconds: u64) -> Value {
        tokio::time::timeout(Duration::from_secs(seconds), self.frames.recv())
            .await
            .unwrap_or_else(|_| panic!("no bridge frame within {seconds} s"))
            .unwrap()
    }

    /// The next frame of `kind`, skipping others, within `seconds`.
    async fn next_of(&mut self, kind: &str, seconds: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let frame = tokio::time::timeout(left, self.frames.recv())
                .await
                .unwrap_or_else(|_| panic!("no {kind} frame within {seconds} s"))
                .unwrap();
            if frame["type"] == kind {
                return frame;
            }
        }
    }

    /// Frames received so far.
    fn drain(&mut self) -> Vec<Value> {
        std::iter::from_fn(|| self.frames.try_recv().ok()).collect()
    }
}

/// A snapshot echoing the request's arguments, with identity and policy fields.
fn snapshot_answer(request: &Value) -> Value {
    json!({
        "type": "get_recent_transcript_response",
        "request_id": request["request_id"],
        "meeting_id": "2026-11-08_parity",
        "segments": [
            {"segment_id": 1, "speaker": "Alice", "text": "Привет {всем}", "meeting_id": "2026-11-08_parity",
             "start_ms": 1000, "end_ms": 2500.5, "refined": false},
            {"segment_id": 2, "speaker": "Bob", "speaker_id": "bob@example.test", "text": "echo",
             "window_seconds": request["window_seconds"], "since_segment_id": request["since_segment_id"],
             "include_refined": request["include_refined"]},
        ],
        "local_user_name": "Alice", "local_user_aliases": ["Alice", "Al"],
        "local_user_profile_revision": "rev-7", "local_user_identity_known": true,
        "copilot_instructions": "Answer in {language}; keep \"quotes\" and $[literal]",
        "copilot_policy_revision": "policy-3",
    })
}

/// One channel process under test.
struct Target {
    name: &'static str,
    child: tokio::process::Child,
    address: SocketAddr,
    bridge: Bridge,
    data: tempfile::TempDir,
    log: PathBuf,
}

impl Drop for Target {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Target {
    async fn start(name: &'static str, python: &Path) -> Self {
        let bridge = Bridge::start().await;
        let data = tempfile::tempdir().unwrap();
        std::fs::create_dir(data.path().join("data")).unwrap();
        let port = free_port().await;
        let log = data.path().join("channel.stderr.log");
        let mut command = if name == "python" {
            let mut command = tokio::process::Command::new(python);
            command.args(["-m", "recog.copilot_channel"]);
            command
        } else {
            let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_recog-copilot-channel"));
            command.arg("--repo-root").arg(repo_root());
            command
        };
        command
            .args(["--bridge-url", &bridge.url, "--mcp-port", &port.to_string()])
            .current_dir(repo_root())
            .env("RECOG_DATA_DIR", data.path().join("data"))
            .env("RECOG_CONFIG_FILE", "/nonexistent/recog/config.toml")
            .env("RECOG_CONFERENCE_HOSTS_FILE", "/nonexistent/recog/conference-hosts.json")
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .kill_on_drop(true);
        let child = command.spawn().unwrap_or_else(|e| panic!("cannot start the {name} channel: {e}"));
        let address: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let target = Target { name, child, address, bridge, data, log };
        let deadline = Instant::now() + Duration::from_secs(30);
        while tokio::net::TcpStream::connect(address).await.is_err() {
            assert!(Instant::now() < deadline, "the {name} channel did not listen:\n{}", target.log_text());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        target
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn data_dir(&self) -> PathBuf {
        self.data.path().join("data")
    }
}

/// Masks what legitimately differs between two runs: session ids, generated request
/// ids, the reply timestamp, the ping time.
fn normalise_text(text: &str, sessions: &[String]) -> String {
    let mut text = text.to_owned();
    for (i, session) in sessions.iter().enumerate() {
        text = text.replace(session, &format!("<session-{i}>"));
    }
    for prefix in ["snapshot_", "proactive_"] {
        let mut out = String::new();
        let mut rest = text.as_str();
        while let Some(at) = rest.find(prefix) {
            out.push_str(&rest[..at + prefix.len()]);
            rest = &rest[at + prefix.len()..];
            let id_len = rest.chars().take_while(char::is_ascii_hexdigit).count();
            if id_len == 12 {
                out.push_str("<id>");
                rest = &rest[12..];
            }
        }
        out.push_str(rest);
        text = out;
    }
    text
}

fn normalise_frame(frame: &Value) -> Value {
    let mut frame = frame.clone();
    if let Some(created) = frame.get_mut("created_at") {
        assert!(created.is_f64(), "created_at is a float: {created}");
        *created = json!("<float>");
    }
    frame
}

/// What the conversation observed, step by step.
struct Transcript {
    steps: Vec<(String, Value)>,
    sessions: Vec<String>,
    address: String,
}

impl Transcript {
    fn record(&mut self, label: &str, value: Value) {
        self.steps.push((label.to_owned(), value));
    }

    fn session(&mut self, id: &str) {
        if !id.is_empty() && !self.sessions.iter().any(|s| s == id) {
            self.sessions.push(id.to_owned());
        }
    }

    /// The steps with session ids masked, as text for the comparison.
    fn normalised(&self) -> Vec<(String, String)> {
        self.steps
            .iter()
            .map(|(label, value)| {
                let text = normalise_text(&value.to_string(), &self.sessions).replace(&self.address, "<address>");
                (label.clone(), text)
            })
            .collect()
    }
}

/// An HTTP answer as compared: status, the headers both servers set, the body bytes.
async fn observe(t: &mut Transcript, response: channel_support::HttpResponse) -> (Value, String) {
    let session = response.header("mcp-session-id").unwrap_or("").to_owned();
    t.session(&session);
    let headers: serde_json::Map<String, Value> =
        ["content-type", "allow", "location", "cache-control", "x-accel-buffering", "mcp-session-id"]
            .iter()
            .filter_map(|name| response.header(name).map(|v| ((*name).to_owned(), json!(v))))
            .collect();
    let status = response.status;
    let body = response.text().await;
    (json!({"status": status, "headers": headers, "body": body}), session)
}

const MCP_PROTOCOL: (&str, &str) = ("mcp-protocol-version", "2025-06-18");

async fn post_raw(target: &Target, headers: &[(&str, &str)], body: impl Into<Bytes>) -> channel_support::HttpResponse {
    request(target.address, "POST", headers, body).await
}

async fn rpc(target: &Target, session: &str, body: &Value) -> channel_support::HttpResponse {
    post_raw(target, &[ACCEPT, JSON, ("mcp-session-id", session), MCP_PROTOCOL], body.to_string()).await
}

/// Posts and records the answer under `label`; returns the session header.
async fn step(t: &mut Transcript, target: &Target, label: &str, session: &str, body: &Value) -> String {
    let response = rpc(target, session, body).await;
    let (observed, session) = observe(t, response).await;
    t.record(label, observed);
    session
}

/// A standing GET stream whose raw text is kept.
struct GetStream {
    text: Arc<Mutex<String>>,
    task: tokio::task::JoinHandle<()>,
    opened: Instant,
}

impl GetStream {
    async fn open(target: &Target, session: &str) -> (Value, Self) {
        let response = request(
            target.address,
            "GET",
            &[("accept", "text/event-stream"), ("mcp-session-id", session), MCP_PROTOCOL],
            Bytes::new(),
        )
        .await;
        let head = json!({"status": response.status, "content-type": response.header("content-type"),
                          "x-accel-buffering": response.header("x-accel-buffering")});
        let text = Arc::new(Mutex::new(String::new()));
        let sink = text.clone();
        let mut body = response.body;
        let task = tokio::spawn(async move {
            while let Some(Ok(frame)) = body.frame().await {
                if let Ok(data) = frame.into_data() {
                    sink.lock().unwrap().push_str(&String::from_utf8_lossy(&data));
                }
            }
        });
        (head, GetStream { text, task, opened: Instant::now() })
    }

    /// The data events so far.
    fn events(&self) -> Vec<Value> {
        sse_data(&self.text.lock().unwrap())
    }

    async fn wait_events(&self, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.events().len() < count && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.events()
    }

    /// Whether a ping comment in sse-starlette's format has arrived.
    fn ping_seen(&self) -> bool {
        self.text.lock().unwrap().split("\r\n\r\n").any(|block| {
            block.strip_prefix(": ping - ").is_some_and(|time| {
                time.len() >= 25
                    && time.ends_with("+00:00")
                    && time.as_bytes()[4] == b'-'
                    && time.as_bytes()[10] == b' '
            })
        })
    }

    fn close(self) {
        self.task.abort();
    }
}

fn tool_call(id: i64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

/// Arguments that do not block: validation failures, lax coercions, edge values.
fn argument_cases() -> Vec<(&'static str, Value)> {
    vec![
        ("send_copilot_message", json!({"kind": 5})),
        ("send_copilot_message", json!({})),
        ("send_copilot_message", json!({"kind": "", "text": "x"})),
        ("send_copilot_message", json!({"kind": "reply", "text": ""})),
        ("send_copilot_message", json!({"kind": null, "text": ["a"]})),
        ("send_copilot_message", json!({"kind": "reply", "text": "t", "request_id": 5})),
        ("send_copilot_message", json!({"kind": "reply", "text": "t", "request_id": "[1]"})),
        ("send_copilot_message", json!({"kind": "reply", "text": "t", "meeting_id": "{}"})),
        ("send_copilot_message", json!({"kind": "reply", "text": "t", "meeting_id": {"a": "x".repeat(60)}})),
        ("send_copilot_message", json!({"kind": true, "text": "t"})),
        ("set_copilot_status", json!({"request_id": "", "status": "x", "extra": 1})),
        ("set_copilot_status", json!({"request_id": "r", "status": ""})),
        ("set_copilot_status", json!({"request_id": "r"})),
        ("set_copilot_status", json!({"request_id": "r", "status": "s", "message": 3})),
        ("get_recent_transcript", json!({"window_seconds": "abc"})),
        ("get_recent_transcript", json!({"window_seconds": 1.5, "since_segment_id": "x", "include_refined": "maybe"})),
        ("get_recent_transcript", json!({"include_refined": 2})),
        ("get_recent_transcript", json!({"include_refined": 0.5})),
        ("get_recent_transcript", json!({"window_seconds": 1e300})),
        ("get_recent_transcript", json!({"since_segment_id": "null", "include_refined": "null"})),
        ("get_recent_transcript", json!({"window_seconds": "1_000.0", "since_segment_id": [1]})),
        ("await_copilot_event", json!({"timeout_seconds": 0})),
        ("await_copilot_event", json!({"timeout_seconds": -5})),
        ("await_copilot_event", json!({"timeout_seconds": "0"})),
        ("await_copilot_event", json!({"timeout_seconds": false})),
        ("await_copilot_event", json!({"timeout_seconds": 1.5})),
        ("await_copilot_event", json!({"timeout_seconds": "1.5"})),
        ("await_copilot_event", json!({"timeout_seconds": null})),
        ("await_copilot_event", json!({"timeout_seconds": "null"})),
        ("await_copilot_event", json!({"timeout_seconds": "[1]"})),
        ("await_copilot_event", json!({"timeout_seconds": "y".repeat(60)})),
        ("nope", json!({})),
    ]
}

/// The events the fake bridge pushes, in order.
fn bridge_events() -> Vec<String> {
    let identity = json!({"local_user_name": "Имя {{GOAL}}", "local_user_aliases": ["Имя {{GOAL}}", "Alias \"q\""],
                          "local_user_profile_revision": "rev-1", "local_user_identity_known": true});
    let mut events: Vec<Value> = vec![
        json!({"type": "meeting_started", "meeting_id": "2026-11-08_parity", "meeting_title": "Синк {проекта}"}),
        json!({"type": "copilot_user_message", "request_id": "ext-1", "text": "что нового?",
               "meeting_id": "2026-11-08_parity", "created_at": 1_730_000_000_123.456_f64}),
        json!({"type": "transcript_tick", "meeting_id": "2026-11-08_parity",
               "new_segments": [{"speaker": "Alice", "text": "a"}, {"speaker": "Bob", "text": "b"}],
               "recent_segments": [{"speaker": "Alice", "text": "a"}, {"speaker": "Bob", "text": "b"}],
               "recent_context": "Alice: a\nBob: b", "copilot_instructions": "Be brief {x}",
               "copilot_policy_revision": "p-2"}),
        json!({"type": "user_profile_updated", "local_user_name": "Me", "local_user_aliases": [],
               "local_user_identity_known": false, "local_user_profile_revision": "rev-2"}),
        json!({"type": "copilot_policy_updated", "copilot_instructions": "", "copilot_policy_revision": "p-3"}),
        json!({"type": "transcript_tick", "new_segments": 5}),
        json!({"type": "meeting_ended", "meeting_id": "2026-11-08_parity"}),
    ];
    for event in &mut events[1..3] {
        event.as_object_mut().unwrap().extend(identity.as_object().unwrap().clone());
    }
    let mut texts: Vec<String> = events.iter().map(Value::to_string).collect();
    texts.extend([
        r#"{"type": "something_else", "x": 1}"#.to_owned(),
        "not json".to_owned(),
        "[1, 2]".to_owned(),
        r#"{"type": "get_recent_transcript_response", "request_id": "snapshot_unknown"}"#.to_owned(),
        r#"{"type": "meeting_started", "meeting_title": NaN}"#.to_owned(),
    ]);
    texts
}

/// The whole conversation with one channel.
async fn conversation(target: &mut Target) -> Transcript {
    let mut t = Transcript { steps: Vec::new(), sessions: Vec::new(), address: target.address.to_string() };

    // The bridge handshake and the first session count.
    let hello = target.bridge.next(20).await;
    t.record("hello", hello);
    let count = target.bridge.next_of("claude_sessions", 10).await;
    t.record("first claude_sessions", count);

    // HTTP edges before any session.
    let (o, _) = observe(&mut t, rpc(target, "0123456789abcdef0123456789abcdef", &json!({})).await).await;
    t.record("unknown session", o);
    for path in ["/mcp/", "/other"] {
        let stream = tokio::net::TcpStream::connect(target.address).await.unwrap();
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await.unwrap();
        tokio::spawn(connection);
        let request = hyper::Request::builder()
            .uri(path)
            .header("host", target.address.to_string())
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        let location = response.headers().get("location").map(|v| v.to_str().unwrap().to_owned());
        let status = response.status().as_u16();
        let body = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        t.record(path, json!({"status": status, "location": location, "body": body}));
    }
    let (o, _) = observe(&mut t, request(target.address, "PUT", &[], Bytes::new()).await).await;
    t.record("PUT", o);
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
    let (o, _) = observe(&mut t, post_raw(target, &[ACCEPT], ping.clone()).await).await;
    t.record("no content type", o);
    let (o, _) = observe(&mut t, post_raw(target, &[ACCEPT, JSON, ("host", "evil.example")], ping.clone()).await).await;
    t.record("bad host", o);
    let (o, _) =
        observe(&mut t, post_raw(target, &[ACCEPT, JSON, ("origin", "http://evil.example")], ping.clone()).await).await;
    t.record("bad origin", o);

    // A session before initialization.
    let (o, early) = observe(&mut t, post_raw(target, &[ACCEPT, JSON], ping.clone()).await).await;
    t.record("request without session", o);
    step(
        &mut t,
        target,
        "tools/list before initialize",
        &early,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    step(&mut t, target, "ping before initialize", &early, &json!({"jsonrpc": "2.0", "id": 3, "method": "ping"})).await;

    // The session the rest of the conversation uses.
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "parity", "version": "1"}}});
    let (o, session) =
        observe(&mut t, post_raw(target, &[ACCEPT, JSON, ("origin", "http://localhost:3000")], init.to_string()).await)
            .await;
    t.record("initialize", o);
    step(&mut t, target, "initialize the early session", &early, &init).await;
    step(
        &mut t,
        target,
        "tools/list after initialize",
        &early,
        &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}),
    )
    .await;
    step(&mut t, target, "initialized", &session, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    let methods = [
        ("tools/list", Value::Null),
        ("resources/list", Value::Null),
        ("prompts/list", Value::Null),
        ("resources/templates/list", Value::Null),
        ("ping", Value::Null),
        ("prompts/get", json!({"name": "x"})),
        ("resources/read", json!({"uri": "file:///x"})),
        ("resources/read", json!({"uri": "no-scheme"})),
        (
            "completion/complete",
            json!({"ref": {"type": "ref/prompt", "name": "x"}, "argument": {"name": "a", "value": "b"}}),
        ),
        ("logging/setLevel", json!({"level": "info"})),
        ("resources/subscribe", json!({"uri": "file:///x"})),
        ("tasks/list", Value::Null),
        ("foo/bar", Value::Null),
        ("tools/call", json!({})),
        ("tools/call", json!({"name": "send_copilot_message", "arguments": [1]})),
        (
            "initialize",
            json!({"protocolVersion": "1999-01-01", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        ),
        (
            "initialize",
            json!({"protocolVersion": "2026-07-28", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        ),
        ("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}})),
    ];
    for (i, (method, params)) in methods.iter().enumerate() {
        let mut body = json!({"jsonrpc": "2.0", "id": 100 + i, "method": method});
        if !params.is_null() {
            body["params"] = params.clone();
        }
        step(&mut t, target, &format!("{method} {params}"), &session, &body).await;
    }
    step(&mut t, target, "string id", &session, &json!({"jsonrpc": "2.0", "id": "abc", "method": "ping"})).await;
    for (label, body) in [
        ("client response", json!({"jsonrpc": "2.0", "id": 1, "result": {}})),
        ("unknown notification", json!({"jsonrpc": "2.0", "method": "notifications/whatever"})),
        ("null id", json!({"jsonrpc": "2.0", "id": null, "method": "ping"})),
        ("float id", json!({"jsonrpc": "2.0", "id": 1.5, "method": "ping"})),
        ("bool id", json!({"jsonrpc": "2.0", "id": true, "method": "ping"})),
    ] {
        step(&mut t, target, label, &session, &body).await;
    }
    for (label, body) in
        [("empty object", "{}"), ("list", "[1]"), ("old version", r#"{"jsonrpc": "1.0", "id": 1, "method": "ping"}"#)]
    {
        // The pydantic union error text is not reproduced: status and code only.
        let response = post_raw(target, &[ACCEPT, JSON, ("mcp-session-id", &session)], body).await;
        let status = response.status;
        let error: Value = serde_json::from_str(&response.text().await).unwrap();
        let message = error["error"]["message"].as_str().unwrap();
        t.record(
            label,
            json!({"status": status, "code": error["error"]["code"], "id": error["id"],
                   "prefix": message.starts_with("Validation error: ")}),
        );
    }
    for (label, headers, body) in [
        ("bad json", vec![ACCEPT, JSON], Bytes::from_static(b"{nope")),
        ("empty body", vec![ACCEPT, JSON], Bytes::new()),
        ("not utf-8", vec![ACCEPT, JSON], Bytes::from_static(b"\xff")),
        ("bad accept", vec![("accept", "application/json"), JSON], Bytes::from(ping.clone())),
        ("star accept", vec![("accept", "*/*"), JSON], Bytes::from(ping.clone())),
        ("json-x content type", vec![ACCEPT, ("content-type", "application/json-x")], Bytes::from(ping.clone())),
        ("bad protocol", vec![ACCEPT, JSON, ("mcp-protocol-version", "2099-01-01")], Bytes::from(ping.clone())),
        ("no protocol header", vec![ACCEPT, JSON], Bytes::from(ping.clone())),
    ] {
        let mut headers = headers;
        headers.push(("mcp-session-id", &session));
        let (o, _) = observe(&mut t, post_raw(target, &headers, body).await).await;
        t.record(label, o);
    }

    // Tool calls that do not block.
    for (i, (name, arguments)) in argument_cases().into_iter().enumerate() {
        step(
            &mut t,
            target,
            &format!("call {name} {arguments}"),
            &session,
            &tool_call(200 + i as i64, name, arguments),
        )
        .await;
    }
    let frames: Vec<Value> = target.bridge.drain().iter().map(normalise_frame).collect();
    t.record("frames after the argument cases", Value::Array(frames));

    // Frames to the bridge.
    for (i, arguments) in [
        json!({"kind": "manual", "text": "Алиса спросила про метрики", "request_id": "req-42", "meeting_id": "m-1"}),
        json!({"kind": "proactive", "text": "Заметка"}),
        json!({"kind": "reply", "text": "t", "request_id": "null"}),
        json!({"kind": "reply", "text": "t", "request_id": "\"quoted\""}),
    ]
    .into_iter()
    .enumerate()
    {
        step(
            &mut t,
            target,
            &format!("send {arguments}"),
            &session,
            &tool_call(300 + i as i64, "send_copilot_message", arguments),
        )
        .await;
        let frame = normalise_frame(&target.bridge.next_of("copilot_channel_reply", 5).await);
        t.record("reply frame", frame);
    }
    step(
        &mut t,
        target,
        "status",
        &session,
        &tool_call(310, "set_copilot_status", json!({"request_id": "req-42", "status": "done"})),
    )
    .await;
    t.record("status frame", target.bridge.next_of("copilot_channel_status", 5).await);

    // Transcript snapshots: answered, with coerced arguments echoed back, then silent.
    for (i, arguments) in [
        json!({}),
        json!({"window_seconds": "30", "since_segment_id": 7, "include_refined": "no"}),
        json!({"window_seconds": 5.0, "since_segment_id": "null", "include_refined": 0}),
        json!({"window_seconds": true}),
    ]
    .into_iter()
    .enumerate()
    {
        step(
            &mut t,
            target,
            &format!("snapshot {arguments}"),
            &session,
            &tool_call(400 + i as i64, "get_recent_transcript", arguments),
        )
        .await;
        let request = target.bridge.next_of("get_recent_transcript_request", 5).await;
        t.record("snapshot request", request);
    }
    *target.bridge.answer_snapshots.lock().unwrap() = false;
    step(&mut t, target, "snapshot without answer", &session, &tool_call(410, "get_recent_transcript", json!({})))
        .await;
    *target.bridge.answer_snapshots.lock().unwrap() = true;
    target.bridge.drain();

    // The standing GET stream, the session count, pushes and the long-poll.
    let (head, get) = GetStream::open(target, &session).await;
    t.record("GET", head);
    let count = target.bridge.next_of("claude_sessions", 10).await;
    t.record("claude_sessions with a GET stream", count);
    let second = request(
        target.address,
        "GET",
        &[("accept", "text/event-stream"), ("mcp-session-id", &session), MCP_PROTOCOL],
        Bytes::new(),
    )
    .await;
    let (o, _) = observe(&mut t, second).await;
    t.record("second GET", o);
    let (o, _) = observe(
        &mut t,
        request(
            target.address,
            "GET",
            &[("accept", "text/event-stream"), ("mcp-session-id", &session), ("last-event-id", "5")],
            Bytes::new(),
        )
        .await,
    )
    .await;
    t.record("GET with Last-Event-ID", o);
    let (o, _) =
        observe(&mut t, request(target.address, "GET", &[("mcp-session-id", &session)], Bytes::new()).await).await;
    t.record("GET without accept", o);

    for event in bridge_events() {
        target.bridge.push.send(event).unwrap();
    }
    t.record("pushes", Value::Array(get.wait_events(7).await));
    for i in 0..8 {
        let timeout = if i == 7 { 1 } else { 2 };
        step(
            &mut t,
            target,
            "await",
            &session,
            &tool_call(500 + i, "await_copilot_event", json!({"timeout_seconds": timeout})),
        )
        .await;
    }

    // Privacy on: the queued and the pushed are masked, the snapshot too.
    std::fs::write(target.data_dir().join("privacy.json"), r#"{"enabled": true}"#).unwrap();
    let before = get.events().len();
    for event in bridge_events().into_iter().take(3) {
        target.bridge.push.send(event).unwrap();
    }
    t.record("pushes with privacy", Value::Array(get.wait_events(before + 3).await[before..].to_vec()));
    for i in 0..3 {
        step(
            &mut t,
            target,
            "await with privacy",
            &session,
            &tool_call(600 + i, "await_copilot_event", json!({"timeout_seconds": 2})),
        )
        .await;
    }
    step(&mut t, target, "snapshot with privacy", &session, &tool_call(610, "get_recent_transcript", json!({}))).await;
    target.bridge.drain();
    std::fs::write(target.data_dir().join("privacy.json"), r#"{"enabled": "yes"}"#).unwrap();
    target.bridge.push.send(bridge_events()[0].clone()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    t.record("pushes with a broken privacy file", json!(get.events().len() - before - 3));
    step(
        &mut t,
        target,
        "await with a broken privacy file",
        &session,
        &tool_call(620, "await_copilot_event", json!({"timeout_seconds": 2})),
    )
    .await;
    std::fs::remove_file(target.data_dir().join("privacy.json")).unwrap();

    // Overflow: 52 events into a queue of 50, a tick dropped first.
    let before = get.events().len();
    for n in 0..52 {
        let event = if n % 10 == 3 {
            json!({"type": "copilot_user_message", "text": format!("question {n}")})
        } else {
            json!({"type": "transcript_tick", "n": n})
        };
        target.bridge.push.send(event.to_string()).unwrap();
    }
    t.record("overflow pushes", json!(get.wait_events(before + 52).await.len() - before));
    let mut drained = Vec::new();
    for i in 0..51 {
        let response =
            rpc(target, &session, &tool_call(700 + i, "await_copilot_event", json!({"timeout_seconds": 1}))).await;
        drained.push(response.events().await);
    }
    t.record("overflow drained", Value::Array(drained.into_iter().flatten().collect()));

    // Cancellation answers the call and leaves the queue alone.
    let pending = rpc(target, &session, &tool_call(800, "await_copilot_event", json!({"timeout_seconds": 30}))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    step(
        &mut t,
        target,
        "cancel",
        &session,
        &json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                                                     "params": {"requestId": 800, "reason": "test"}}),
    )
    .await;
    let cancelled =
        tokio::time::timeout(Duration::from_secs(5), pending.text()).await.expect("the cancelled call ends");
    t.record("cancelled call", json!(cancelled));
    target.bridge.push.send(r#"{"type": "meeting_ended"}"#.to_owned()).unwrap();
    step(
        &mut t,
        target,
        "await after cancel",
        &session,
        &tool_call(801, "await_copilot_event", json!({"timeout_seconds": 2})),
    )
    .await;
    step(
        &mut t,
        target,
        "cancel unknown",
        &session,
        &json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                                                             "params": {"requestId": 999}}),
    )
    .await;

    // sse-starlette pings every 15 s.
    let ping_due = get.opened + Duration::from_secs(16);
    tokio::time::sleep(ping_due.saturating_duration_since(Instant::now())).await;
    t.record("ping comment", json!(get.ping_seen()));

    // Closing the GET stream, then the session.
    get.close();
    target.bridge.drain();
    let count = target.bridge.next_of("claude_sessions", 10).await;
    t.record("claude_sessions after the GET closed", count);
    let (o, _) = observe(
        &mut t,
        request(target.address, "DELETE", &[("mcp-session-id", &session), MCP_PROTOCOL], Bytes::new()).await,
    )
    .await;
    t.record("DELETE", o);
    step(&mut t, target, "after DELETE", &session, &json!({"jsonrpc": "2.0", "id": 900, "method": "ping"})).await;
    let (o, _) = observe(
        &mut t,
        request(target.address, "GET", &[("accept", "text/event-stream"), ("mcp-session-id", &session)], Bytes::new())
            .await,
    )
    .await;
    t.record("GET after DELETE", o);

    // The bridge drops the link: the channel reconnects and re-announces.
    target.bridge.drop_connection.send(()).unwrap();
    let hello = target.bridge.next_of("copilot_channel_hello", 10).await;
    t.record("hello after reconnect", hello);
    let count = target.bridge.next_of("claude_sessions", 10).await;
    t.record("claude_sessions after reconnect", count);
    t
}

/// The log lines both should write, in order.
fn log_lines(text: &str) -> Vec<String> {
    let kept = ["MCP Streamable HTTP listening", "connected to", "reconnected to", "channel push", "event_queue"];
    text.lines()
        .filter_map(|line| line.strip_prefix("[copilot-channel] "))
        .filter(|line| kept.iter().any(|k| line.starts_with(k)))
        .map(|line| line.split(" on http://127.0.0.1:").next().unwrap().split(" to ws://").next().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_conversation_when_driven_then_python_and_rust_channels_answer_alike() {
    let Some(python) = oracle_python() else {
        eprintln!("no Python interpreter for the oracle (set RECOG_ORACLE_PYTHON): skipped");
        return;
    };
    // given
    let mut python_target = Target::start("python", &python).await;
    let mut rust_target = Target::start("rust", &python).await;
    // when
    let (python_run, rust_run) = tokio::join!(conversation(&mut python_target), conversation(&mut rust_target));
    // then
    let (p, r) = (python_run.normalised(), rust_run.normalised());
    assert_eq!(p.len(), r.len());
    let mut differences = Vec::new();
    for ((label, python), (_, rust)) in p.iter().zip(&r) {
        if rust != python {
            differences.push(format!("step {label:?} differs\nrust:   {rust}\npython: {python}"));
        }
    }
    assert!(differences.is_empty(), "{} of {} steps differ:\n{}", differences.len(), p.len(), differences.join("\n\n"));
    eprintln!("{} steps identical", p.len());
    let (python_log, rust_log) = (log_lines(&python_target.log_text()), log_lines(&rust_target.log_text()));
    assert_eq!(
        rust_log,
        python_log,
        "the log lines differ\npython:\n{}\nrust:\n{}",
        python_target.log_text(),
        rust_target.log_text()
    );
    eprintln!("{} log lines identical", python_log.len());
    let _ = (python_target.name, rust_target.name);
}

/// The official Python SDK client's view of one channel.
async fn sdk_client_run(target: &mut Target, python: &Path) -> Vec<String> {
    target.bridge.next_of("copilot_channel_hello", 20).await;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle/channel_client.py");
    let mut child = tokio::process::Command::new(python)
        .arg(script)
        .arg(format!("http://{}/mcp", target.address))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let mut out = Vec::new();
    let mut sessions = Vec::new();
    while let Some(line) = tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await.unwrap().unwrap() {
        if line == r#"{"ready": true}"# {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let event = json!({"type": "copilot_user_message", "request_id": "ext-7", "text": "Есть вопросы?",
                               "meeting_id": "2026-11-08_parity", "local_user_name": "Alice"});
            target.bridge.push.send(event.to_string()).unwrap();
            stdin.write_all(b"go\n").await.unwrap();
            continue;
        }
        out.push(line);
    }
    let status = child.wait().await.unwrap();
    assert!(status.success(), "the SDK client failed against the {} channel", target.name);
    let frames: Vec<String> = target.bridge.drain().iter().map(|f| normalise_frame(f).to_string()).collect();
    out.extend(frames.into_iter().filter(|f| !f.contains("claude_sessions")));
    sessions.dedup();
    let mut out: Vec<String> = out.iter().map(|line| normalise_text(line, &sessions)).collect();
    // The push and the long-poll's answer travel different streams.
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn official_python_client_when_talking_to_each_channel_then_same_messages() {
    let Some(python) = oracle_python() else {
        eprintln!("no Python interpreter for the oracle (set RECOG_ORACLE_PYTHON): skipped");
        return;
    };
    // given
    let mut python_target = Target::start("python", &python).await;
    let mut rust_target = Target::start("rust", &python).await;
    // when
    let (python_run, rust_run) =
        tokio::join!(sdk_client_run(&mut python_target, &python), sdk_client_run(&mut rust_target, &python));
    // then
    assert!(python_run.iter().any(|line| line.contains("notifications/claude/channel")), "{python_run:#?}");
    assert_eq!(rust_run, python_run);
    eprintln!("{} SDK client messages identical", python_run.len());
}
