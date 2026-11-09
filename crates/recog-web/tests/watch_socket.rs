//! `/api/meetings/{id}/watch` over a real socket: a watching tab receives the new
//! rows of its meeting, its departure releases the meeting's tail, and a binary frame
//! ends the connection without a closing handshake, as Starlette's handler dies.

mod support;

use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use support::{App, get, obj, write};
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn watch(port: u16, meeting: &str) -> Ws {
    let url = format!("ws://127.0.0.1:{port}/api/meetings/{meeting}/watch");
    tokio_tungstenite::connect_async(url).await.unwrap().0
}

/// The first frame the tab receives: the tab is registered some time after the
/// handshake, so rows are appended and announced until one reaches it.
async fn first_frame(app: &App, ws: &mut Ws, path: &std::path::Path) -> Message {
    let mut rows = String::new();
    for id in 1..100 {
        rows.push_str(&format!("{{\"id\": {id}, \"text\": \"row {id}\"}}\n"));
        write(path, &rows);
        app.state.transcript_watcher.notify_clients("m", path).await;
        if let Ok(Some(Ok(message))) = tokio::time::timeout(Duration::from_millis(50), ws.next()).await {
            return message;
        }
    }
    panic!("the tab never received a row");
}

#[tokio::test]
async fn watching_tab_when_rows_are_appended_then_it_receives_them_and_its_departure_releases_the_tail() {
    let app = App::new();
    let path = app.meetings().join("m/transcript.jsonl");
    let mut ws = watch(app.serve().await, "m").await;
    let first = first_frame(&app, &mut ws, &path).await;
    assert_eq!(get(&obj(first.to_text().unwrap()), "type"), &recog_core::pyjson::Value::from("segments"));

    let mut rows = std::fs::read_to_string(&path).unwrap();
    rows.push_str("{\"id\": 500, \"text\": \"later\"}\n");
    write(&path, &rows);
    app.state.transcript_watcher.notify_clients("m", &path).await;
    let second = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.unwrap().unwrap().unwrap();
    assert!(second.to_text().unwrap().contains("\"later\""), "{second:?}");
    assert!(!second.to_text().unwrap().contains("\"row 1\""));

    ws.send(Message::Close(None)).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.unwrap();
    assert!(matches!(reply, Some(Ok(Message::Close(_)))), "{reply:?}");
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.state.transcript_watcher.tail(&path).is_some() {
        assert!(Instant::now() < deadline, "the tail outlived its last client");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn binary_frame_when_watching_then_the_connection_drops_without_a_close_frame() {
    let app = App::new();
    let path = app.meetings().join("m/transcript.jsonl");
    let mut ws = watch(app.serve().await, "m").await;
    first_frame(&app, &mut ws, &path).await;

    ws.send(Message::Binary(vec![1, 2, 3].into())).await.unwrap();

    let end = tokio::time::timeout(Duration::from_secs(2), ws.next()).await.unwrap();
    assert!(!matches!(end, Some(Ok(Message::Close(_)))), "{end:?}");
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.state.transcript_watcher.tail(&path).is_some() {
        assert!(Instant::now() < deadline, "the crashed tab stayed registered");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
