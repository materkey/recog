//! One connection to `ktalk_bridge`: the hello, frames in, statuses out.
//!
//! The behaviour of the Python client (`websockets.connect` with its defaults) is kept
//! where the bridge can tell: a 10 s opening handshake, 1 MiB messages, a keepalive
//! ping every 20 s that drops a connection whose pong does not come within 20 s, and a
//! close with code 1000/1001 counted as a clean end rather than a failure.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use recog_core::protocol;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message};

use super::glue::Supervisor;
use super::tmux::Runner;
use crate::py;

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(20);
const PING_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_MESSAGE: usize = 1 << 20;

/// Why a connection ended badly, named like the Python exception it stands for.
#[derive(Debug)]
pub struct BridgeError {
    pub kind: &'static str,
    pub message: String,
}

impl BridgeError {
    fn of(error: tungstenite::Error) -> Self {
        let kind = match &error {
            tungstenite::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
            tungstenite::Error::Io(_) => "OSError",
            tungstenite::Error::Url(_) => "InvalidURI",
            tungstenite::Error::Http(_) | tungstenite::Error::HttpFormat(_) => "InvalidStatus",
            _ => "ConnectionClosedError",
        };
        BridgeError { kind, message: error.to_string() }
    }
}

/// A frame as `Supervisor._decode` accepts it: a JSON object in a text frame, or in a
/// binary frame holding UTF-8.
pub fn decode(message: &Message) -> Option<Value> {
    let text = match message {
        Message::Text(text) => text.as_str(),
        Message::Binary(bytes) => std::str::from_utf8(bytes).ok()?,
        _ => return None,
    };
    serde_json::from_str::<Value>(text).ok().filter(Value::is_object)
}

/// Connects, says hello and serves frames until the connection ends. `delay` goes back
/// to the initial backoff once connected, as the Python loop resets it.
pub async fn serve<R: Runner + 'static>(sup: &Supervisor<R>, delay: &mut f64) -> Result<(), BridgeError> {
    let (mut sink, stream) = connect(&sup.bridge_url).await?.split();

    // Announce ourselves: the bridge moves this socket into its supervisor set and
    // replies with `meeting_state`. `current_meeting_id` lets a restarted bridge re-arm
    // its manual mirror from the one runtime owner of that truth, us (D5).
    let hello = json!({"type": protocol::SUPERVISOR_HELLO, "current_meeting_id": sup.live_meeting_id()});
    sink.send(Message::text(py::json_dumps(&hello))).await.map_err(BridgeError::of)?;
    let (tx, rx) = mpsc::unbounded_channel::<Message>();
    sup.set_ws(Some(tx));
    sup.on_connected();
    *delay = sup.tunables().initial_backoff_s;

    let ping_sent: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let broken = Rc::new(Notify::new());
    let writer = tokio::task::spawn_local(write_frames(sink, rx, ping_sent.clone(), broken.clone()));
    let result = read_frames(sup, stream, &ping_sent, &broken).await;
    sup.set_ws(None);
    writer.abort();
    result
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the bridge WebSocket within the 10 s handshake, with 1 MiB messages.
async fn connect(url: &str) -> Result<Socket, BridgeError> {
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    let connect = tokio_tungstenite::connect_async_with_config(url, Some(config), false);
    let (socket, _) = tokio::time::timeout(OPEN_TIMEOUT, connect)
        .await
        .map_err(|_| BridgeError { kind: "TimeoutError", message: "timed out during opening handshake".into() })?
        .map_err(BridgeError::of)?;
    Ok(socket)
}

/// The writing half: the statuses queued by the supervisor, and a keepalive ping
/// every 20 s; a pong not back within 20 s, or a ping that cannot be sent, wakes
/// `broken`.
async fn write_frames(
    mut sink: futures_util::stream::SplitSink<Socket, Message>,
    mut rx: mpsc::UnboundedReceiver<Message>,
    ping_sent: Rc<Cell<Option<Instant>>>,
    broken: Rc<Notify>,
) {
    let mut ping = tokio::time::interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
    loop {
        tokio::select! {
            message = rx.recv() => {
                let Some(message) = message else { break };
                if let Err(e) = sink.send(message).await {
                    crate::log::line(&format!("{} status send failed: {e}", crate::log::SUPERVISOR_PREFIX));
                }
            }
            _ = ping.tick() => {
                if ping_sent.get().is_some_and(|sent| sent.elapsed() >= PING_TIMEOUT) {
                    broken.notify_one();
                    break;
                }
                if ping_sent.get().is_none() {
                    ping_sent.set(Some(Instant::now()));
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        broken.notify_one();
                        break;
                    }
                }
            }
        }
    }
}

/// The reading half: frames to the supervisor until the connection ends.
async fn read_frames<R: Runner + 'static>(
    sup: &Supervisor<R>,
    mut stream: futures_util::stream::SplitStream<Socket>,
    ping_sent: &Cell<Option<Instant>>,
    broken: &Notify,
) -> Result<(), BridgeError> {
    loop {
        let next = tokio::select! {
            next = stream.next() => next,
            _ = broken.notified() => return Err(BridgeError {
                kind: "ConnectionClosedError",
                message: "keepalive ping timeout".into(),
            }),
        };
        match next {
            None => return Ok(()),
            Some(Err(e)) => return Err(BridgeError::of(e)),
            Some(Ok(Message::Close(frame))) => return close_result(frame),
            Some(Ok(Message::Pong(_))) => ping_sent.set(None),
            Some(Ok(message)) => {
                if let Some(data) = decode(&message) {
                    sup.handle_frame(&data).await;
                }
            }
        }
    }
}

/// A close with code 1000/1001 (or none) is a clean end, any other a failure.
fn close_result(frame: Option<tungstenite::protocol::CloseFrame>) -> Result<(), BridgeError> {
    let clean = frame.as_ref().is_none_or(|f| matches!(f.code, CloseCode::Normal | CloseCode::Away));
    if clean {
        return Ok(());
    }
    let frame = frame.map(|f| format!("received {} ({})", u16::from(f.code), f.reason)).unwrap_or_default();
    Err(BridgeError { kind: "ConnectionClosedError", message: frame })
}
