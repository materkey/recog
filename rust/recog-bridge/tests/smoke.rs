//! The whole path once: a real WebSocket server, an extension joining a meeting in
//! a real storage, and the frames it gets back.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use recog_bridge::testing::{RecordingLive, SwitchablePolicy, collecting_log};
use recog_bridge::{Bridge, BridgeConfig};
use recog_core::meeting_storage::{MeetingStorage, SessionMode, StorageOptions};
use recog_core::pyjson::{self, Value};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

async fn next_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap();
        if let Message::Text(text) = message {
            return pyjson::loads(text.as_str()).unwrap();
        }
    }
}

fn kind(frame: &Value) -> &str {
    frame.as_object().unwrap().get("type").unwrap().as_str().unwrap()
}

#[test]
fn given_real_server_when_extension_joins_then_meeting_started_and_history_arrive() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(MeetingStorage::new(&dir.path().join("meetings"), StorageOptions::new(dir.path())).unwrap());
    storage.start_session(&SessionMode { ktalk: true, ..SessionMode::default() }).unwrap();
    let config = BridgeConfig { host: "127.0.0.1".into(), port: 0, debug_file: None, ..BridgeConfig::default() };
    let (log, lines) = collecting_log();
    let bridge =
        Bridge::with_clock(config, SwitchablePolicy::enabled(), Arc::new(recog_bridge::hooks::SystemClock), log)
            .unwrap();
    bridge.set_meeting_storage(storage.clone());
    let live = RecordingLive::new();
    bridge.set_live_events(Some(live.clone()));
    let addrs = bridge.start().unwrap();
    let url = format!("ws://{}", addrs[0]);
    // when
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let frames = runtime.block_on(async {
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert("Origin", "https://test.ktalk.ru".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let primer = next_frame(&mut ws).await;
        let join = r#"{"type": "meeting_join", "url": "https://test.ktalk.ru/room-1", "title": "Weekly", "platform": "ktalk"}"#;
        ws.send(Message::text(join)).await.unwrap();
        let started = next_frame(&mut ws).await;
        let history = next_frame(&mut ws).await;
        ws.close(None).await.unwrap();
        vec![primer, started, history]
    });
    // then
    assert_eq!(
        frames.iter().map(kind).collect::<Vec<_>>(),
        ["copilot_status", "meeting_started", "transcript_history"]
    );
    std::thread::sleep(Duration::from_millis(300));
    bridge.stop();
    assert!(live.names().contains(&"meeting_changed".to_owned()));
    let lines = lines.lock().unwrap().clone();
    assert!(lines.iter().any(|l| l.starts_with("[KTalk Bridge] Joined ktalk meeting: https://test.ktalk.ru/room-1")));
    assert!(lines.iter().any(|l| l == "[KTalk Bridge] Ended KTalk meeting due to client disconnect"), "{lines:?}");
}

#[test]
fn given_disallowed_origin_when_connecting_then_closed_with_1008() {
    // given
    let config = BridgeConfig { host: "127.0.0.1".into(), port: 0, debug_file: None, ..BridgeConfig::default() };
    let (log, _lines) = collecting_log();
    let bridge =
        Bridge::with_clock(config, SwitchablePolicy::enabled(), Arc::new(recog_bridge::hooks::SystemClock), log)
            .unwrap();
    let addrs = bridge.start().unwrap();
    // when
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let close = runtime.block_on(async {
        let mut request = format!("ws://{}", addrs[0]).into_client_request().unwrap();
        request.headers_mut().insert("Origin", "https://evil.example".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap() {
                Some(Ok(Message::Close(frame))) => break frame,
                Some(Ok(_)) => {}
                other => panic!("{other:?}"),
            }
        }
    });
    bridge.stop();
    // then
    let close = close.unwrap();
    assert_eq!(u16::from(close.code), 1008);
    assert_eq!(close.reason.as_str(), "origin not allowed");
}
