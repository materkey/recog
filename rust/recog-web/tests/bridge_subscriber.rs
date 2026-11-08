//! The cases of tests/test_bridge_subscriber.py against a mock bridge on an ephemeral
//! port.
//!
//! Python's cases that replace the live connection object (`sub._ws = _DeadWS()`, a
//! patched `websockets.connect`) have no counterpart: the sink is private here. Their
//! guarantees, a failed send returning false without touching the run loop and the
//! hello going out before any upstream frame, are checked through the real socket.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use recog_web::bridge_subscriber::{BridgeHandler, BridgeSubscriber, RawSender};
use recog_web::util::BoxFuture;
use support::obj;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct Recorder {
    forwarded: Mutex<Vec<String>>,
    connected: Mutex<usize>,
    disconnected: Mutex<usize>,
}

impl BridgeHandler for Recorder {
    fn forward(&self, raw: String) -> BoxFuture<'_, ()> {
        self.forwarded.lock().unwrap().push(raw);
        Box::pin(async {})
    }
    fn connected(&self, _send: Arc<dyn RawSender>) -> BoxFuture<'_, ()> {
        *self.connected.lock().unwrap() += 1;
        Box::pin(async {})
    }
    fn disconnected(&self) -> BoxFuture<'_, ()> {
        *self.disconnected.lock().unwrap() += 1;
        Box::pin(async {})
    }
}

impl Recorder {
    fn forwarded(&self) -> Vec<String> {
        self.forwarded.lock().unwrap().clone()
    }
}

/// What a mock bridge does with one connection.
#[derive(Clone)]
enum Script {
    /// Send these frames, then keep the connection open for a while.
    Send(Vec<String>),
    /// Record every frame the subscriber sends.
    Record(Arc<Mutex<Vec<String>>>),
    /// Close at once, the first `n` connections; then send these frames.
    DropFirst(usize, Vec<String>),
}

struct MockBridge {
    port: u16,
    connections: Arc<Mutex<usize>>,
    task: tokio::task::JoinHandle<()>,
    /// Closed with the bridge, as `server.close()` closes them.
    sockets: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Drop for MockBridge {
    fn drop(&mut self) {
        self.task.abort();
        for socket in self.sockets.lock().unwrap().drain(..) {
            socket.abort();
        }
    }
}

async fn mock_bridge(script: Script, port: Option<u16>) -> MockBridge {
    let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0))).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(Mutex::new(0));
    let count = Arc::clone(&connections);
    let sockets = Arc::new(Mutex::new(Vec::new()));
    let handles = Arc::clone(&sockets);
    let task = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let script = script.clone();
            let n = {
                let mut c = count.lock().unwrap();
                *c += 1;
                *c
            };
            let socket = tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else { return };
                match script {
                    Script::Send(frames) => {
                        for frame in frames {
                            let _ = ws.send(Message::text(frame)).await;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    Script::Record(received) => {
                        while let Some(Ok(message)) = ws.next().await {
                            if let Message::Text(text) = message {
                                received.lock().unwrap().push(text.as_str().to_owned());
                            }
                        }
                    }
                    Script::DropFirst(dropped, frames) => {
                        if n <= dropped {
                            let _ = ws.send(Message::text(r#"{"type": "hello"}"#)).await;
                            let _ = ws.close(None).await;
                            return;
                        }
                        for frame in frames {
                            let _ = ws.send(Message::text(frame)).await;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            });
            handles.lock().unwrap().push(socket);
        }
    });
    MockBridge { port, connections, task, sockets }
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..240 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out: {what}");
}

fn subscriber(port: u16, recorder: &Arc<Recorder>) -> BridgeSubscriber {
    BridgeSubscriber::with_backoff(
        &format!("ws://127.0.0.1:{port}"),
        Arc::clone(recorder) as Arc<dyn BridgeHandler>,
        Duration::from_millis(50),
    )
}

fn refine(id: u32, text: &str) -> String {
    format!(
        r#"{{"type": "transcript_refine", "updates": [{{"segment_id": {id}, "text": "{text}"}}], "refined_at": "2026-04-24T12:00:00"}}"#
    )
}

#[tokio::test]
async fn refine_and_transcript_frames_when_received_then_only_the_refine_is_forwarded_verbatim() {
    let bridge = mock_bridge(
        Script::Send(vec![refine(7, "уточнённый"), r#"{"type": "transcript", "speaker": "X", "text": "y"}"#.into()]),
        None,
    )
    .await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();

    until("a forwarded frame", || !recorder.forwarded().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(recorder.forwarded(), [refine(7, "уточнённый")]);
    sub.stop().await;
}

#[tokio::test]
async fn bridge_that_drops_the_first_connection_when_running_then_the_subscriber_reconnects_and_forwards() {
    let bridge = mock_bridge(Script::DropFirst(1, vec![refine(1, "после reconnect")]), None).await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();

    until("a frame after the reconnect", || !recorder.forwarded().is_empty()).await;

    assert_eq!(obj(&recorder.forwarded()[0]), obj(&refine(1, "после reconnect")));
    assert!(*bridge.connections.lock().unwrap() >= 2);
    assert!(*recorder.disconnected.lock().unwrap() >= 1);
    sub.stop().await;
}

#[tokio::test]
async fn no_bridge_listening_when_started_then_the_loop_retries_in_the_background() {
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(1, &recorder);

    sub.start();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(recorder.forwarded().is_empty());
    assert!(sub.is_running());
    sub.stop().await;
    assert!(!sub.is_running());
}

#[tokio::test]
async fn new_connection_when_established_then_subscriber_hello_is_the_first_frame() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let bridge = mock_bridge(Script::Record(Arc::clone(&received)), None).await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();
    // Upstream frames sent from the very start: none may precede the hello.
    let sentinel = r#"{"type": "copilot_chat_message", "request_id": "early"}"#;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !sub.send(sentinel).await {
        assert!(std::time::Instant::now() < deadline, "no send was delivered");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    let frames = received.clone();
    until("two frames", move || frames.lock().unwrap().len() >= 2).await;
    let frames = received.lock().unwrap().clone();
    assert_eq!(obj(&frames[0]), obj(r#"{"type": "subscriber_hello"}"#));
    assert_eq!(frames[1], sentinel);
    assert_eq!(*recorder.connected.lock().unwrap(), 1);
    sub.stop().await;
}

#[tokio::test]
async fn malformed_frame_before_a_refine_when_received_then_only_the_refine_is_forwarded() {
    let bridge = mock_bridge(Script::Send(vec!["not-json".into(), refine(9, "ok")]), None).await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();

    until("the refine", || !recorder.forwarded().is_empty()).await;

    assert_eq!(recorder.forwarded().len(), 1);
    assert_eq!(obj(&recorder.forwarded()[0]), obj(&refine(9, "ok")));
    sub.stop().await;
}

#[tokio::test]
async fn copilot_reply_and_unknown_type_when_received_then_only_the_reply_is_forwarded() {
    let reply = r#"{"type": "copilot_reply", "request_id": "req-1", "text": "ответ копайлота"}"#;
    let bridge =
        mock_bridge(Script::Send(vec![r#"{"type": "totally_unknown", "x": 1}"#.into(), reply.into()]), None).await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();

    until("the reply", || !recorder.forwarded().is_empty()).await;

    assert_eq!(recorder.forwarded(), [reply]);
    sub.stop().await;
}

#[tokio::test]
async fn chat_echo_from_the_bridge_when_the_subscriber_runs_then_it_is_forwarded_verbatim() {
    let echo = r#"{"type": "copilot_chat_echo", "request_id": "req-1", "text": "вопрос", "created_at": 1.0}"#;
    let bridge = mock_bridge(Script::Send(vec![echo.into()]), None).await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();

    until("the echo", || !recorder.forwarded().is_empty()).await;

    assert_eq!(recorder.forwarded(), [echo]);
    sub.stop().await;
}

#[tokio::test]
async fn copilot_status_variants_when_received_then_every_one_is_forwarded() {
    let frames: Vec<String> = [
        r#"{"type": "copilot_status", "request_id": "req-2", "status": "working"}"#,
        r#"{"type": "copilot_status", "request_id": "", "status": "agent_state", "agent_state": "online"}"#,
        r#"{"type": "copilot_status", "request_id": "", "status": "channel_connected"}"#,
    ]
    .map(str::to_owned)
    .to_vec();
    let bridge = mock_bridge(Script::Send(frames.clone()), None).await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();

    until("three frames", || recorder.forwarded().len() >= 3).await;

    assert_eq!(recorder.forwarded(), frames);
    sub.stop().await;
}

#[tokio::test]
async fn connected_bridge_when_send_is_awaited_then_the_frame_arrives_verbatim_and_true_is_returned() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let bridge = mock_bridge(Script::Record(Arc::clone(&received)), None).await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();
    until("connected", || sub.is_connected()).await;

    let raw = r#"{"type": "copilot_chat_message", "request_id": "r1", "text": "hi"}"#;
    assert!(sub.send(raw).await);

    let frames = Arc::clone(&received);
    until("the frame", move || frames.lock().unwrap().len() >= 2).await;
    assert_eq!(received.lock().unwrap()[1], raw);
    sub.stop().await;
}

#[tokio::test]
async fn subscriber_never_started_when_send_is_awaited_then_false() {
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(1, &recorder);

    assert!(!sub.send(r#"{"type": "copilot_chat_message"}"#).await);
}

#[tokio::test]
async fn dead_bridge_when_send_is_awaited_then_false_and_the_loop_keeps_running() {
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(1, &recorder);
    sub.start();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(!sub.send(r#"{"type": "copilot_spawn_request"}"#).await);
    assert!(sub.is_running());
    sub.stop().await;
}

#[tokio::test]
async fn bridge_gone_and_back_when_sent_during_the_backoff_then_false_and_nothing_is_queued() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let bridge = mock_bridge(Script::Record(Arc::clone(&received)), None).await;
    let port = bridge.port;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(port, &recorder);
    sub.start();
    until("connected", || sub.is_connected()).await;

    drop(bridge);
    until("disconnected", || !sub.is_connected()).await;
    assert!(!sub.send(r#"{"type": "copilot_chat_message", "request_id": "lost"}"#).await);
    let received_again = Arc::new(Mutex::new(Vec::new()));
    let _bridge = mock_bridge(Script::Record(Arc::clone(&received_again)), Some(port)).await;
    until("reconnected", || sub.is_connected()).await;
    let sentinel = r#"{"type": "copilot_chat_message", "request_id": "sentinel"}"#;
    assert!(sub.send(sentinel).await);

    let frames = Arc::clone(&received_again);
    until("the sentinel", move || frames.lock().unwrap().len() >= 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let upstream: Vec<String> =
        received_again.lock().unwrap().iter().filter(|f| !f.contains("subscriber_hello")).cloned().collect();
    assert_eq!(upstream, [sentinel]);
    sub.stop().await;
}

#[tokio::test]
async fn frame_with_an_unhashable_type_when_received_then_dropped_and_the_connection_kept() {
    let bridge = mock_bridge(
        Script::Send(vec![r#"{"type": ["not", "hashable"]}"#.into(), refine(11, "после мусорного type")]),
        None,
    )
    .await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();

    until("the refine", || !recorder.forwarded().is_empty()).await;

    assert_eq!(recorder.forwarded().len(), 1);
    assert_eq!(*bridge.connections.lock().unwrap(), 1);
    sub.stop().await;
}

#[tokio::test]
async fn connected_subscriber_when_stopped_then_the_disconnect_callback_still_runs() {
    let received = Arc::new(Mutex::new(Vec::new()));
    let bridge = mock_bridge(Script::Record(received), None).await;
    let recorder = Arc::new(Recorder::default());
    let sub = subscriber(bridge.port, &recorder);
    sub.start();
    until("connected", || sub.is_connected()).await;

    sub.stop().await;

    assert_eq!(*recorder.disconnected.lock().unwrap(), 1);
    assert!(!sub.is_connected());
    assert!(!sub.send("{}").await);
}
