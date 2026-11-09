//! `_handle_client`'s read loop and its `finally`, on fake connections: primers that
//! cannot be sent, a peer's error, binary frames, JSON that is invalid or not an
//! object, a handler's error, and the cleanup errors of a subscriber's hello and of
//! the disconnect, each logged with Python's prefix while the loop goes on.

mod common;

use common::Harness;
use futures_util::StreamExt;
use recog_bridge::testing::{FakeConnection, RecordingCapture, RecordingSink, SendMode};
use recog_bridge::{Connection, Inbound, Role};
use tokio::task::JoinHandle;

const ADDRESS: &str = "('127.0.0.1', 12345)";

/// A connection whose sends fail, handled by the bridge.
async fn connect_dead(h: &Harness) -> (FakeConnection, JoinHandle<()>) {
    let sink = RecordingSink::with_mode(SendMode::Fail("broken pipe".into()));
    let (fake, connection) = FakeConnection::with_sink(sink, ADDRESS, None);
    let bridge = h.bridge.clone();
    let task = tokio::spawn(async move { bridge.handle_client(connection).await });
    fake.idle().await;
    (fake, task)
}

#[tokio::test]
async fn given_dead_socket_when_client_connects_then_primer_failures_logged_and_frames_still_served() {
    // given
    let h = Harness::new();
    let (_channel, _c) = h.add(Role::Channel);
    // when
    let (fake, task) = connect_dead(&h).await;
    // then
    assert!(h.logged("[KTalk Bridge] Failed to send channel_connected primer to new client: broken pipe"));
    assert!(h.logged("[KTalk Bridge] Failed to send agent_state primer to new client: broken pipe"));
    // when: a frame after the failed primers
    fake.send_text(r#"{"type": "clock_sync", "browser_time": 1783159199000}"#);
    fake.idle().await;
    // then
    assert!((h.bridge.clock_offset() - 1.0).abs() < 1e-9);
    fake.close();
    task.await.unwrap();
    assert!(h.logged("[KTalk Bridge] Client disconnected"));
}

#[tokio::test]
async fn given_connected_client_when_peer_fails_then_connection_error_logged_and_client_cleaned_up() {
    // given
    let h = Harness::new();
    let (fake, task) = h.connect(None).await;
    assert_eq!(h.bridge.clients_in(Role::Extension).len(), 1);
    // when
    fake.fail("no close frame received or sent");
    task.await.unwrap();
    // then
    assert!(h.logged("[KTalk Bridge] Connection error: no close frame received or sent"), "{:?}", h.logs());
    assert!(h.logged("[KTalk Bridge] Client disconnected"));
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
}

#[tokio::test]
async fn given_connected_client_when_bad_frames_arrive_then_each_is_logged_and_the_loop_goes_on() {
    // given
    let h = Harness::new();
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text("[1, 2]");
    fake.send_text("{bad");
    fake.send_text(&"x".repeat(150));
    fake.send_text(r#"{"type": "speaker_update", "timestamp": "late", "speaker": null}"#);
    fake.send_text(r#"{"type": "clock_sync", "browser_time": 1783159199000}"#);
    fake.idle().await;
    // then
    let logs = h.logs();
    assert!(logs.contains(&"[KTalk Bridge] Error processing message: 'list' object has no attribute 'get'".to_owned()));
    assert!(logs.contains(&"[KTalk Bridge] Invalid JSON: {bad".to_owned()), "{logs:?}");
    assert!(logs.contains(&format!("[KTalk Bridge] Invalid JSON: {}", "x".repeat(100))), "{logs:?}");
    assert!(
        logs.contains(
            &"[KTalk Bridge] Error processing message: unsupported operand type(s) for /: 'str' and 'int'".to_owned()
        ),
        "{logs:?}"
    );
    assert!((h.bridge.clock_offset() - 1.0).abs() < 1e-9);
    fake.close();
    task.await.unwrap();
}

#[tokio::test]
async fn given_binary_frames_when_received_then_parsed_like_text_and_invalid_ones_skipped() {
    // given
    let h = Harness::new();
    let messages = vec![
        Ok(Inbound::Binary(b"{bad".to_vec())),
        Ok(Inbound::Binary(br#"{"type": "clock_sync", "browser_time": 1783159198000}"#.to_vec())),
    ];
    let connection = Connection {
        sink: RecordingSink::new(),
        inbound: futures_util::stream::iter(messages).boxed(),
        remote_address: ADDRESS.to_owned(),
        origins: None,
    };
    // when
    h.bridge.handle_client(connection).await;
    // then
    assert!(h.logs().iter().any(|l| l.starts_with("[KTalk Bridge] Invalid JSON: ") && l.contains("{bad")));
    assert!((h.bridge.clock_offset() - 2.0).abs() < 1e-9);
    assert!(h.logged("[KTalk Bridge] Client disconnected"));
}

#[tokio::test]
async fn given_capture_that_cannot_stop_when_hello_and_close_leave_no_extension_then_both_errors_logged() {
    // given
    let h = Harness::new();
    let capture = RecordingCapture::new();
    *capture.stop_error.lock().unwrap() = Some("device busy".into());
    h.bridge.set_meeting_capture(capture.clone());
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text(r#"{"type": "subscriber_hello"}"#);
    fake.idle().await;
    // then
    assert!(h.logged(&format!("[KTalk Bridge] Subscriber registered from {ADDRESS}")));
    assert!(h.logged("[KTalk Bridge] Error processing message: device busy"), "{:?}", h.logs());
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert!(h.logged("[KTalk Bridge] Subscriber disconnected"));
    assert!(h.logged("[KTalk Bridge] Connection error: device busy"), "{:?}", h.logs());
    assert_eq!(*capture.calls.lock().unwrap(), [false, false]);
}

#[tokio::test]
async fn given_each_role_when_it_disconnects_then_named_in_the_log() {
    for (hello, name) in [
        (Some(r#"{"type": "supervisor_hello"}"#), "Copilot supervisor"),
        (Some(r#"{"type": "copilot_channel_hello"}"#), "Copilot channel"),
        (Some(r#"{"type": "subscriber_hello"}"#), "Subscriber"),
        (None, "Client"),
    ] {
        // given
        let h = Harness::new();
        let (fake, task) = h.connect(None).await;
        if let Some(hello) = hello {
            fake.send_text(hello);
            fake.idle().await;
        }
        // when
        fake.close();
        task.await.unwrap();
        // then
        assert!(h.logged(&format!("[KTalk Bridge] {name} disconnected")), "{name}: {:?}", h.logs());
        let others =
            ["Copilot supervisor", "Copilot channel", "Subscriber", "Client"].into_iter().filter(|n| *n != name);
        for other in others {
            assert!(!h.logged(&format!("[KTalk Bridge] {other} disconnected")), "{name}: {:?}", h.logs());
        }
    }
}

#[tokio::test]
async fn given_two_origin_values_when_client_connects_then_refused_before_registration() {
    // given
    let h = Harness::new();
    let sink = RecordingSink::new();
    let (_fake, connection) = FakeConnection::with_sink(
        sink.clone(),
        ADDRESS,
        Some(vec!["https://test.ktalk.ru".into(), "https://test.ktalk.ru".into()]),
    );
    // when
    h.bridge.handle_client(connection).await;
    // then
    assert!(h.logged(&format!("[KTalk Bridge] Rejected WS connection: duplicate Origin headers ({ADDRESS})")));
    assert_eq!(*sink.closes.lock().unwrap(), [(1008, "origin not allowed".to_owned())]);
    assert!(sink.frames().is_empty());
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
}

#[tokio::test]
async fn given_last_channel_with_requests_when_it_disconnects_then_live_ones_dropped_and_tabs_told() {
    // given
    let h = Harness::new();
    let (extension, _e) = h.add(Role::Extension);
    let (fake, task) = h.connect(None).await;
    fake.send_text(r#"{"type": "copilot_channel_hello"}"#);
    fake.idle().await;
    h.bridge.set_pending_request("r-live", "m1");
    h.bridge.set_pending_request("r-stale", "stale:m0");
    let () = h.bridge.with_agent(|a| a.claude_sessions = Some(1));
    extension.clear();
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert_eq!(h.bridge.pending_requests(), [("r-stale".to_owned(), "stale:m0".to_owned())]);
    assert!(h.logged("[KTalk Bridge] Channel disconnected with 1 in-flight request(s); dropped bookkeeping"));
    assert_eq!(h.bridge.with_agent(|a| a.claude_sessions), None);
    let sent: Vec<String> = extension.sent.lock().unwrap().clone();
    assert_eq!(
        sent,
        [
            r#"{"type": "copilot_status", "request_id": "", "status": "channel_disconnected", "message": "Copilot channel server disconnected"}"#
        ]
    );
}
