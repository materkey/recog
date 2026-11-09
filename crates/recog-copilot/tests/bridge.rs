//! The supervisor against an in-process mock bridge over a real WebSocket
//! (tests/test_copilot_supervisor.py, "Bridge connection" and "Startup manual-session
//! re-adoption"). Real time: the paused clock would fire the handshake timeout while
//! the socket waits for the network.
#![forbid(unsafe_code)]

mod common;

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use recog_copilot::supervisor::{
    MANUAL_MEETING_ID, Runner, State, Supervisor, TMUX_SESSION, TMUX_SESSION_MANUAL, Tunables,
};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

type Socket = WebSocketStream<TcpStream>;

/// Serves every connection with `handler`; returns the bridge URL.
async fn mock_bridge<F, Fut>(handler: F) -> String
where
    F: Fn(Socket) -> Fut + 'static,
    Fut: Future<Output = ()> + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let handler = Rc::new(handler);
    tokio::task::spawn_local(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let handler = handler.clone();
            tokio::task::spawn_local(async move {
                if let Ok(socket) = tokio_tungstenite::accept_async(stream).await {
                    handler(socket).await;
                }
            });
        }
    });
    url
}

async fn recv_json(socket: &mut Socket) -> Option<Value> {
    let message = tokio::time::timeout(Duration::from_secs(1), socket.next()).await.ok()??.ok()?;
    serde_json::from_str(message.to_text().ok()?).ok()
}

async fn send_json(socket: &mut Socket, value: Value) {
    socket.send(Message::text(value.to_string())).await.unwrap();
}

/// Records the first frame of every connection, then holds the socket open.
fn hello_recorder(
    received: Rc<RefCell<Vec<Value>>>,
    reply: Option<Value>,
) -> impl Fn(Socket) -> std::pin::Pin<Box<dyn Future<Output = ()>>> {
    move |mut socket| {
        let received = received.clone();
        let reply = reply.clone();
        Box::pin(async move {
            if let Some(first) = recv_json(&mut socket).await {
                received.borrow_mut().push(first);
                if let Some(reply) = reply {
                    send_json(&mut socket, reply).await;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        })
    }
}

async fn wait(condition: impl Fn() -> bool) -> bool {
    for _ in 0..100 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    condition()
}

fn fake_at(url: &str, autospawn: bool) -> (FakeSupervisor, Rc<FakeRunner>) {
    let runner = FakeRunner::new();
    let sup = Supervisor::new(url, runner.clone(), TestSettings::enabled(), autospawn, Tunables::new(10.0));
    (sup, runner)
}

fn tmux_at(
    url: &str,
    fake: FakeTmux,
    autospawn: bool,
) -> Rc<Supervisor<recog_copilot::supervisor::TmuxRunner<FakeTmux>>> {
    Supervisor::new(url, Rc::new(tmux_runner(fake)), TestSettings::enabled(), autospawn, Tunables::new(10.0))
}

#[tokio::test]
async fn idle_supervisor_connecting_sends_a_hello_with_a_null_meeting_first() {
    local(async {
        // given
        let received = Rc::new(RefCell::new(Vec::new()));
        let url = mock_bridge(hello_recorder(received.clone(), None)).await;
        let (sup, _runner) = fake_at(&url, true);
        // when
        sup.start();
        // then
        assert!(wait(|| !received.borrow().is_empty()).await, "no frame");
        assert_eq!(received.borrow()[0], json!({"type": "supervisor_hello", "current_meeting_id": null}));
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn hello_after_a_manual_spawn_reports_the_manual_session() {
    local(async {
        // given
        let received = Rc::new(RefCell::new(Vec::new()));
        let url = mock_bridge(hello_recorder(received.clone(), None)).await;
        let (sup, _runner) = fake_at(&url, false);
        sup.handle_frame(&frame(r#"{"type": "copilot_spawn_request", "request_id": "r1"}"#)).await;
        sup.with_lifecycle(|l| l.session_ready());
        // when
        sup.start();
        // then
        assert!(wait(|| !received.borrow().is_empty()).await, "no frame");
        assert_eq!(received.borrow()[0], json!({"type": "supervisor_hello", "current_meeting_id": "manual"}));
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn meeting_state_answering_the_hello_spawns_for_the_active_meeting() {
    local(async {
        // given
        let received = Rc::new(RefCell::new(Vec::new()));
        let reply = json!({"type": "meeting_state", "active": true, "meeting_id": "mtg-1", "meeting_title": "Standup"});
        let url = mock_bridge(hello_recorder(received.clone(), Some(reply))).await;
        let (sup, runner) = fake_at(&url, true);
        // when
        sup.start();
        // then
        assert!(wait(|| !runner.spawned().is_empty()).await);
        assert_eq!(received.borrow()[0]["type"], "supervisor_hello");
        assert_eq!(runner.spawned(), ids(&["mtg-1"]));
        assert_eq!(sup.state(), State::Spawning);
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn unreachable_bridge_keeps_the_loop_retrying_without_crashing() {
    local(async {
        // given
        let (sup, _runner) = fake_at("ws://127.0.0.1:1", true);
        sup.tunables().initial_backoff_s = 0.05;
        let capture = recog_copilot::log::Capture::start();
        // when
        sup.start();
        tokio::time::sleep(Duration::from_millis(200)).await;
        // then
        assert!(sup.is_running());
        assert!(capture.text().contains("bridge unavailable (ConnectionRefusedError"), "{}", capture.text());
        assert_eq!(capture.text().matches("bridge unavailable").count(), 1); // throttled after the first
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn garbage_before_a_valid_frame_is_ignored_and_the_frame_still_spawns() {
    local(async {
        // given
        let url = mock_bridge(|mut socket: Socket| async move {
            let _hello = recv_json(&mut socket).await;
            socket.send(Message::text("not-json")).await.unwrap();
            socket.send(Message::text(r#"["not", "a", "dict"]"#)).await.unwrap();
            socket.send(Message::binary(b"\xff\xfe".to_vec())).await.unwrap();
            socket.send(Message::text(r#"{"type": "meeting_started", "meeting_id": "A"}"#)).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        })
        .await;
        let (sup, runner) = fake_at(&url, true);
        // when
        sup.start();
        // then
        assert!(wait(|| !runner.spawned().is_empty()).await);
        assert_eq!(runner.spawned(), ids(&["A"]));
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn live_manual_named_session_at_startup_is_readopted_and_the_hello_reports_it() {
    local(async {
        // given: a full restart during a manual session
        let received = Rc::new(RefCell::new(Vec::new()));
        let reply = json!({"type": "meeting_state", "active": true, "meeting_id": MANUAL_MEETING_ID});
        let url = mock_bridge(hello_recorder(received.clone(), Some(reply))).await;
        let sup = tmux_at(&url, FakeTmux::named(&[TMUX_SESSION_MANUAL]), true);
        // when
        sup.start();
        // then
        assert!(wait(|| !received.borrow().is_empty()).await);
        assert_eq!(received.borrow()[0], json!({"type": "supervisor_hello", "current_meeting_id": "manual"}));
        tokio::time::sleep(Duration::from_millis(200)).await; // the reply is reconciled
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Running, Some(MANUAL_MEETING_ID)));
        assert_eq!(sup.runner.session_name(), TMUX_SESSION_MANUAL);
        assert!(sup.runner.command_runner().kill_targets().is_empty());
        assert!(sup.runner.command_runner().new_session_names().is_empty());
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn live_conference_named_session_at_startup_survives_the_meeting_rejoin() {
    local(async {
        // given: a full restart mid-conference
        let received = Rc::new(RefCell::new(Vec::new()));
        let recorder = received.clone();
        let url = mock_bridge(move |mut socket: Socket| {
            let received = recorder.clone();
            async move {
                if let Some(first) = recv_json(&mut socket).await {
                    received.borrow_mut().push(first);
                    send_json(&mut socket, json!({"type": "meeting_state", "active": false, "meeting_id": null})).await;
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    send_json(&mut socket, json!({"type": "meeting_started", "meeting_id": "A"})).await;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        })
        .await;
        let sup = tmux_at(&url, FakeTmux::named(&[TMUX_SESSION]), true);
        // when
        sup.start();
        // then
        assert!(wait(|| !received.borrow().is_empty()).await);
        assert_eq!(received.borrow()[0]["current_meeting_id"], Value::Null);
        assert!(wait(|| sup.state() == State::Running).await);
        assert_eq!(sup.meeting_id().as_deref(), Some("A"));
        assert_eq!(sup.runner.session_name(), TMUX_SESSION);
        let fake = sup.runner.command_runner();
        assert!(fake.kill_targets().is_empty());
        assert!(fake.new_session_names().is_empty());
        assert!(fake.live().contains(TMUX_SESSION));
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn both_sessions_alive_at_startup_kill_the_manual_one_and_adopt_nothing() {
    local(async {
        // given
        let received = Rc::new(RefCell::new(Vec::new()));
        let url = mock_bridge(hello_recorder(received.clone(), None)).await;
        let sup = tmux_at(&url, FakeTmux::named(&[TMUX_SESSION, TMUX_SESSION_MANUAL]), true);
        // when
        sup.start();
        // then
        assert!(wait(|| !received.borrow().is_empty()).await);
        assert_eq!(received.borrow()[0]["current_meeting_id"], Value::Null);
        assert_eq!(sup.state(), State::Idle);
        let fake = sup.runner.command_runner();
        assert_eq!(fake.kill_targets(), [TMUX_SESSION_MANUAL]);
        assert_eq!(fake.live(), [TMUX_SESSION.to_owned()].into());
        assert_eq!(sup.runner.session_name(), TMUX_SESSION);
        assert!(fake.new_session_names().is_empty());
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn no_session_at_startup_costs_one_probe_of_the_manual_name_and_stays_idle() {
    local(async {
        // given
        let received = Rc::new(RefCell::new(Vec::new()));
        let url = mock_bridge(hello_recorder(received.clone(), None)).await;
        let sup = tmux_at(&url, FakeTmux::named(&[]), true);
        // when
        sup.start();
        // then
        assert!(wait(|| !received.borrow().is_empty()).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(received.borrow()[0]["current_meeting_id"], Value::Null);
        assert_eq!(sup.state(), State::Idle);
        let fake = sup.runner.command_runner();
        assert_eq!(fake.subcommands(), ["has-session"]);
        assert_eq!(fake.calls.borrow()[0][4..], ["-t", "=copilot-manual"]);
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn manual_session_at_startup_is_readopted_with_autospawn_off_too() {
    local(async {
        // given
        let received = Rc::new(RefCell::new(Vec::new()));
        let url = mock_bridge(hello_recorder(received.clone(), None)).await;
        let sup = tmux_at(&url, FakeTmux::named(&[TMUX_SESSION_MANUAL]), false);
        // when
        sup.start();
        // then
        assert!(wait(|| !received.borrow().is_empty()).await);
        assert_eq!(received.borrow()[0], json!({"type": "supervisor_hello", "current_meeting_id": "manual"}));
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Running, Some(MANUAL_MEETING_ID)));
        assert_eq!(sup.runner.session_name(), TMUX_SESSION_MANUAL);
        assert!(sup.runner.command_runner().kill_targets().is_empty());
        assert!(sup.runner.command_runner().new_session_names().is_empty());
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn connected_supervisor_tracks_the_socket_runs_the_watchdog_and_stop_tears_it_down() {
    local(async {
        // given
        let fake = FakeTmux::with_session(true);
        fake.respond("capture-pane", 0, REPL_PROMPT_SCREEN);
        let url = mock_bridge(|mut socket: Socket| async move {
            let _hello = recv_json(&mut socket).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        })
        .await;
        let sup = tmux_at(&url, fake, true);
        // when
        sup.start();
        // then
        assert!(wait(|| sup.connected()).await);
        assert!(sup.watchdog_running());
        sup.stop();
        assert!(!sup.watchdog_running());
    })
    .await;
}

#[tokio::test]
async fn bridge_closing_the_socket_cleanly_is_reconnected_with_a_fresh_hello() {
    local(async {
        // given: the first connection is closed with 1001 after the hello
        let received = Rc::new(RefCell::new(Vec::new()));
        let recorder = received.clone();
        let url = mock_bridge(move |mut socket: Socket| {
            let received = recorder.clone();
            async move {
                if let Some(first) = recv_json(&mut socket).await {
                    let count = {
                        let mut r = received.borrow_mut();
                        r.push(first);
                        r.len()
                    };
                    if count == 1 {
                        let _ = socket.close(None).await;
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        })
        .await;
        let (sup, _runner) = fake_at(&url, true);
        let capture = recog_copilot::log::Capture::start();
        // when
        sup.start();
        // then: reconnected after the initial 1 s backoff, no failure logged
        assert!(wait(|| received.borrow().len() >= 2).await);
        assert!(received.borrow().iter().all(|f| f["type"] == "supervisor_hello"));
        let text = capture.text();
        assert!(
            text.contains(&format!("connected to {url}")) && text.contains(&format!("reconnected to {url}")),
            "{text}"
        );
        assert!(!text.contains("bridge unavailable"), "{text}");
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn binary_frame_then_close_with_an_error_code_spawns_and_logs_the_close_as_a_failure() {
    local(async {
        // given: the first connection sends its frame as binary, then closes with 1011
        let url = mock_bridge(|mut socket: Socket| async move {
            if recv_json(&mut socket).await.is_some() {
                let frame = json!({"type": "meeting_started", "meeting_id": "mtg-b"}).to_string();
                socket.send(Message::Binary(frame.into_bytes().into())).await.unwrap();
                socket.send(Message::Pong(Vec::new().into())).await.unwrap();
                let close = tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Error,
                    reason: "boom".into(),
                };
                let _ = socket.close(Some(close)).await;
            }
        })
        .await;
        let (sup, runner) = fake_at(&url, true);
        let capture = recog_copilot::log::Capture::start();
        // when
        sup.start();
        // then
        assert!(wait(|| capture.text().contains("bridge unavailable")).await, "{}", capture.text());
        assert_eq!(runner.spawned(), ids(&["mtg-b"]));
        let text = capture.text();
        assert!(
            text.contains(
                "[copilot-supervisor] bridge unavailable (ConnectionClosedError: received 1011 (boom)); retry in 1.0s"
            ),
            "{text}"
        );
        sup.stop();
    })
    .await;
}

#[tokio::test]
async fn bridge_dropping_the_connection_without_a_close_logs_a_failure() {
    local(async {
        // given: the first connection is dropped after the hello, no close frame
        let url = mock_bridge(|mut socket: Socket| async move {
            let _ = recv_json(&mut socket).await;
            drop(socket);
        })
        .await;
        let (sup, _runner) = fake_at(&url, true);
        let capture = recog_copilot::log::Capture::start();
        // when
        sup.start();
        // then
        assert!(wait(|| capture.text().contains("bridge unavailable")).await, "{}", capture.text());
        let text = capture.text();
        assert!(text.contains("[copilot-supervisor] bridge unavailable (ConnectionClosedError: "), "{text}");
        assert!(text.contains("); retry in 1.0s"), "{text}");
        sup.stop();
    })
    .await;
}
