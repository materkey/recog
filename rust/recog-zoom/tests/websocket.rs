//! The real WebSocket client against a local server: what the bridge sees of it.
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use recog_zoom::bridge::{Connector, Incoming, WsConnector};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

async fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    (listener, url)
}

#[tokio::test]
async fn given_a_bridge_when_the_source_connects_then_no_origin_is_sent_and_frames_flow_both_ways() {
    let (listener, url) = listener().await;
    let headers = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&headers);
    let bridge = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        // The callback's error type is tungstenite's, not ours to shrink.
        #[allow(clippy::result_large_err)]
        let record = |request: &Request, response: Response| {
            seen.lock().unwrap().extend(request.headers().keys().map(|name| name.as_str().to_owned()));
            Ok(response)
        };
        let mut socket = tokio_tungstenite::accept_hdr_async(stream, record).await.unwrap();
        let frame = socket.next().await.unwrap().unwrap();
        socket
            .send(Message::text(r#"{"type": "meeting_join_rejected", "reason": "another_tab_active"}"#))
            .await
            .unwrap();
        socket.close(None).await.unwrap();
        frame
    });

    let mut link = WsConnector::default().connect(&url).await.unwrap();
    link.outgoing.send(r#"{"type":"meeting_join"}"#.into()).await.unwrap();
    let message = link.incoming.recv().await.unwrap();
    let closed = link.incoming.recv().await.unwrap();

    assert_eq!(bridge.await.unwrap(), Message::text(r#"{"type":"meeting_join"}"#));
    assert!(!headers.lock().unwrap().iter().any(|name| name == "origin"), "{:?}", headers.lock().unwrap());
    assert_eq!(
        message,
        Incoming::Message(br#"{"type": "meeting_join_rejected", "reason": "another_tab_active"}"#.to_vec())
    );
    assert!(matches!(closed, Incoming::Lost(_)), "{closed:?}");
}

#[tokio::test]
async fn given_no_bridge_when_the_source_connects_then_the_reason_is_returned() {
    let (listener, url) = listener().await;
    drop(listener);

    let error = WsConnector::default().connect(&url).await.err().unwrap();

    assert!(!error.is_empty());
}

#[tokio::test]
async fn given_a_bridge_that_stops_answering_when_pings_go_unanswered_then_the_link_is_lost() {
    let (listener, url) = listener().await;
    let bridge = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        // Hung: holds the connection, reads nothing, answers no ping.
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(socket);
    });
    let keepalive = WsConnector {
        open_timeout: Duration::from_secs(5),
        ping_interval: Duration::from_millis(50),
        ping_timeout: Duration::from_millis(100),
    };

    let mut link = keepalive.connect(&url).await.unwrap();
    let lost = tokio::time::timeout(Duration::from_secs(3), link.incoming.recv()).await.unwrap();

    assert_eq!(lost, Some(Incoming::Lost("keepalive ping timeout".into())));
    bridge.abort();
}

#[tokio::test]
async fn given_a_bridge_when_the_source_is_done_then_it_closes_the_link() {
    let (listener, url) = listener().await;
    let bridge = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.next().await
    });

    let link = WsConnector::default().connect(&url).await.unwrap();
    drop(link);

    let received = tokio::time::timeout(Duration::from_secs(3), bridge).await.unwrap().unwrap();
    assert!(matches!(received, Some(Ok(Message::Close(_))) | None), "{received:?}");
}
