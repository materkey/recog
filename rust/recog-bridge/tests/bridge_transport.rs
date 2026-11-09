//! The real WebSocket transport under a connection: pings and pongs, binary frames,
//! the peer's close, a message over 1 MiB, a protocol error and the keepalive
//! timeout, each as the log and the peer see it (what `websockets` does for the
//! Python server).

mod common;

use std::time::Duration;

use common::{Harness, wait_for};
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The bridge accepting on a free port; a client without Origin, as the Python
/// daemons connect.
async fn connect(h: &Harness) -> Socket {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let bridge = h.bridge.clone();
    tokio::spawn(async move { bridge.accept(listener).await });
    tokio_tungstenite::connect_async(url).await.unwrap().0
}

/// The next message that `keep` accepts, skipping the others.
async fn next_matching<T>(ws: &mut Socket, keep: impl Fn(Message) -> Option<T>) -> T {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap();
        if let Some(found) = keep(message) {
            return found;
        }
    }
}

fn close_of(message: Message) -> Option<(u16, String)> {
    match message {
        Message::Close(Some(frame)) => Some((u16::from(frame.code), frame.reason.as_str().to_owned())),
        _ => None,
    }
}

#[tokio::test]
async fn given_a_connection_when_the_peer_pings_then_its_payload_comes_back_as_pong() {
    // given
    let h = Harness::new();
    let mut ws = connect(&h).await;
    // when
    ws.send(Message::Ping(b"are you there".to_vec().into())).await.unwrap();
    // then
    let pong = next_matching(&mut ws, |m| match m {
        Message::Pong(payload) => Some(payload.to_vec()),
        _ => None,
    })
    .await;
    assert_eq!(pong, b"are you there");
}

#[tokio::test]
async fn given_a_connection_when_a_binary_frame_arrives_then_it_is_read_as_json_text() {
    // given
    let h = Harness::new();
    let mut ws = connect(&h).await;
    // when
    ws.send(Message::Binary(b"{not json".to_vec().into())).await.unwrap();
    // then
    assert!(
        wait_for(|| h.logs().iter().any(|l| l.starts_with("[KTalk Bridge] Invalid JSON: ") && l.contains("{not json")))
            .await,
        "{:?}",
        h.logs()
    );
}

#[tokio::test]
async fn given_a_connection_when_the_peer_closes_with_1008_then_the_close_is_logged_as_a_connection_error() {
    // given
    let h = Harness::new();
    let mut ws = connect(&h).await;
    // when
    ws.close(Some(CloseFrame { code: CloseCode::Policy, reason: "bye".into() })).await.unwrap();
    // then
    let expected =
        "[KTalk Bridge] Connection error: received 1008 (policy violation) bye; then sent 1008 (policy violation) bye";
    assert!(wait_for(|| h.logged(expected)).await, "{:?}", h.logs());
    assert!(wait_for(|| h.logged("[KTalk Bridge] Client disconnected")).await);
}

#[tokio::test]
async fn given_a_connection_when_the_peer_closes_normally_then_no_connection_error() {
    // given
    let h = Harness::new();
    let mut ws = connect(&h).await;
    // when
    ws.close(None).await.unwrap();
    // then
    assert!(wait_for(|| h.logged("[KTalk Bridge] Client disconnected")).await, "{:?}", h.logs());
    assert!(!h.logs().iter().any(|l| l.contains("Connection error")), "{:?}", h.logs());
}

#[tokio::test]
async fn given_a_connection_when_a_message_over_one_mib_arrives_then_closed_with_1009() {
    // given
    let h = Harness::new();
    let mut ws = connect(&h).await;
    // when
    ws.send(Message::text("x".repeat((1 << 20) + 1))).await.unwrap();
    // then
    assert_eq!(next_matching(&mut ws, close_of).await.0, 1009);
    let expected = "[KTalk Bridge] Connection error: sent 1009 (message too big); no close frame received";
    assert!(wait_for(|| h.logged(expected)).await, "{:?}", h.logs());
}

#[tokio::test]
async fn given_a_connection_when_the_client_sends_an_unmasked_frame_then_the_protocol_error_is_logged() {
    // given
    let h = Harness::new();
    let mut ws = connect(&h).await;
    // when: a text frame "hi" without the mask every client frame must carry
    let tcp = match ws.get_mut() {
        MaybeTlsStream::Plain(tcp) => tcp,
        _ => unreachable!("ws:// is plain TCP"),
    };
    tcp.write_all(&[0x81, 0x02, b'h', b'i']).await.unwrap();
    // then
    let expected = "[KTalk Bridge] Connection error: WebSocket protocol error: Received an unmasked frame from client";
    assert!(wait_for(|| h.logged(expected)).await, "{:?}", h.logs());
}

#[tokio::test(start_paused = true)]
async fn given_a_peer_that_never_answers_pings_when_two_intervals_pass_then_closed_with_1011() {
    // given
    let h = Harness::new();
    let mut ws = connect(&h).await;
    // when: nothing is read, so no pong goes back
    tokio::time::sleep(Duration::from_secs(41)).await;
    // then
    let pings = std::sync::atomic::AtomicUsize::new(0);
    let close = next_matching(&mut ws, |m| {
        if matches!(m, Message::Ping(_)) {
            pings.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        close_of(m)
    })
    .await;
    assert_eq!(close, (1011, "keepalive ping timeout".to_owned()));
    assert_eq!(pings.into_inner(), 1);
    let expected =
        "[KTalk Bridge] Connection error: sent 1011 (internal error) keepalive ping timeout; no close frame received";
    assert!(wait_for(|| h.logged(expected)).await, "{:?}", h.logs());
}
