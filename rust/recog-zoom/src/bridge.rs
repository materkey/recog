//! The link to the bridge: snapshots in from the helper, frames out to the bridge.
//!
//! The source connects as the extension does, but sends no `Origin` header, so the
//! bridge treats it as a local client (`recog/ws_origin.py`); its disconnect ends the
//! meeting. The WebSocket runs in a task of its own behind two channels, which keeps
//! the pump's wait on "a helper line or a bridge message" cancel-safe.

use std::future::Future;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::helper_link::HelperLines;
use crate::log::{Logger, StatusLog};
use crate::py::{self, PyError};
use crate::session::ZoomAppSession;

pub const BRIDGE_URL: &str = "ws://127.0.0.1:8766";

/// What the bridge sent, or why the link is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Message(Vec<u8>),
    Lost(String),
}

/// An open link: messages from the bridge, and text frames to it.
pub struct BridgeLink {
    pub incoming: mpsc::Receiver<Incoming>,
    pub outgoing: mpsc::Sender<String>,
}

/// Opens links to the bridge; tests open channels instead.
pub trait Connector {
    fn connect(&self, url: &str) -> impl Future<Output = Result<BridgeLink, String>> + Send;
}

/// The WebSocket client, with the defaults of Python's `websockets.connect`: ten
/// seconds to open, a ping every 20 s answered within 20 s.
#[derive(Debug, Clone)]
pub struct WsConnector {
    pub open_timeout: Duration,
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
}

impl Default for WsConnector {
    fn default() -> Self {
        Self {
            open_timeout: Duration::from_secs(10),
            ping_interval: Duration::from_secs(20),
            ping_timeout: Duration::from_secs(20),
        }
    }
}

impl Connector for WsConnector {
    async fn connect(&self, url: &str) -> Result<BridgeLink, String> {
        let (socket, _) = tokio::time::timeout(self.open_timeout, tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| "timed out during opening handshake".to_string())?
            .map_err(|error| error.to_string())?;
        let (incoming_tx, incoming) = mpsc::channel(16);
        let (outgoing, outgoing_rx) = mpsc::channel(16);
        tokio::spawn(drive(socket, incoming_tx, outgoing_rx, self.clone()));
        Ok(BridgeLink { incoming, outgoing })
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Own the WebSocket until either side is done; the reason it ended goes to `incoming`.
async fn drive(
    mut socket: Socket,
    incoming: mpsc::Sender<Incoming>,
    mut outgoing: mpsc::Receiver<String>,
    keepalive: WsConnector,
) {
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + keepalive.ping_interval, keepalive.ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut awaiting_pong: Option<tokio::time::Instant> = None;
    let lost = loop {
        let pong_deadline = async {
            match awaiting_pong {
                Some(sent) => tokio::time::sleep_until(sent + keepalive.ping_timeout).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            message = socket.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if incoming.send(Incoming::Message(text.as_bytes().to_vec())).await.is_err() { break None; }
                }
                Some(Ok(Message::Binary(bytes))) => {
                    if incoming.send(Incoming::Message(bytes.to_vec())).await.is_err() { break None; }
                }
                Some(Ok(Message::Pong(_))) => awaiting_pong = None,
                Some(Ok(Message::Close(frame))) => {
                    break Some(frame.map_or_else(|| "connection closed".into(), |f| format!("received {} ({})", u16::from(f.code), f.reason)));
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => break Some(error.to_string()),
                None => break Some("connection closed".into()),
            },
            frame = outgoing.recv() => match frame {
                Some(text) => if let Err(error) = socket.send(Message::text(text)).await { break Some(error.to_string()); },
                // The pump is done with the link: close it.
                None => {
                    let _ = socket.close(None).await;
                    break None;
                }
            },
            _ = ping.tick(), if awaiting_pong.is_none() => {
                if let Err(error) = socket.send(Message::Ping(Vec::new().into())).await { break Some(error.to_string()); }
                awaiting_pong = Some(tokio::time::Instant::now());
            }
            () = pong_deadline => break Some("keepalive ping timeout".into()),
        }
    };
    if let Some(reason) = lost {
        let _ = incoming.send(Incoming::Lost(reason)).await;
    }
}

/// Why the pump stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpError {
    /// The helper closed its stream.
    HelperGone,
    /// The bridge link is gone, or reading the helper failed (Python's `OSError`, which
    /// its serve loop counts as a lost link too); the serve loop reconnects.
    Link(String),
    /// A line or a bridge message Python would raise on; ends the run.
    Fatal(String),
}

impl std::fmt::Display for PumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HelperGone => f.write_str("the Zoom helper disconnected"),
            Self::Link(reason) => write!(f, "bridge link lost: {reason}"),
            Self::Fatal(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for PumpError {}

fn fatal(error: PyError) -> PumpError {
    PumpError::Fatal(error.0)
}

/// Seconds since the epoch, `time.time()`.
pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// What one helper line does: frames for the bridge, and the log notes.
pub fn on_line(
    raw: &[u8],
    session: &mut ZoomAppSession,
    log: &mut StatusLog,
    at: f64,
) -> Result<Vec<String>, PumpError> {
    let text = std::str::from_utf8(raw).map_err(|error| PumpError::Fatal(error.to_string()))?;
    let snapshot: Value = serde_json::from_str(text).map_err(|error| PumpError::Fatal(error.to_string()))?;
    let frames = session.frames(&snapshot, at).map_err(fatal)?;
    log.note(session.state.as_ref()).map_err(fatal)?;
    log.note_unread(&session.unread);
    log.note_own_mic_unknown(session.own_mic_unknown);
    Ok(frames.iter().map(crate::frame::Frame::to_json).collect())
}

/// What one bridge message does: a refused join silences the source for a while.
pub fn on_message(raw: &[u8], session: &mut ZoomAppSession, logger: &Logger, at: f64) -> Result<(), PumpError> {
    let message: Value = serde_json::from_slice(raw).map_err(|error| PumpError::Fatal(error.to_string()))?;
    let message = message.as_object().ok_or_else(|| PumpError::Fatal("a bridge message is not an object".into()))?;
    if message.get("type").and_then(Value::as_str) == Some("meeting_join_rejected") {
        logger.line(&format!("join rejected: {}", py::display_opt(message.get("reason"))));
        session.on_rejected(at);
    }
    Ok(())
}

/// Forward helper snapshots to the bridge until either side closes.
pub async fn pump(
    link: &mut BridgeLink,
    lines: &mut HelperLines,
    session: &mut ZoomAppSession,
    log: &mut StatusLog,
    logger: &Logger,
) -> PumpError {
    loop {
        tokio::select! {
            // A message that arrived with a line is handled first, as in Python.
            biased;
            message = link.incoming.recv() => match message {
                Some(Incoming::Message(raw)) => {
                    if let Err(error) = on_message(&raw, session, logger, now()) { return error; }
                }
                Some(Incoming::Lost(reason)) => return PumpError::Link(reason),
                None => return PumpError::Link("connection closed".into()),
            },
            line = lines.next_line() => match line {
                Ok(Some(raw)) => match on_line(&raw, session, log, now()) {
                    Ok(frames) => {
                        for frame in frames {
                            if link.outgoing.send(frame).await.is_err() {
                                return lost_reason(link).await;
                            }
                        }
                    }
                    Err(error) => return error,
                },
                Ok(None) => return PumpError::HelperGone,
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => return PumpError::Fatal(error.to_string()),
                Err(error) => return PumpError::Link(error.to_string()),
            },
        }
    }
}

async fn lost_reason(link: &mut BridgeLink) -> PumpError {
    match link.incoming.recv().await {
        Some(Incoming::Lost(reason)) => PumpError::Link(reason),
        _ => PumpError::Link("connection closed".into()),
    }
}

/// Forward the helper's snapshots, reconnecting to the bridge, until the helper is
/// gone or a line fails; returns why.
pub async fn serve(
    connector: &impl Connector,
    bridge_url: &str,
    lines: &mut HelperLines,
    session: &mut ZoomAppSession,
    log: &mut StatusLog,
    logger: &Logger,
    reconnect: Duration,
) -> PumpError {
    loop {
        match connector.connect(bridge_url).await {
            Ok(mut link) => match pump(&mut link, lines, session, log, logger).await {
                PumpError::Link(reason) => logger.line(&format!("bridge link lost: {reason}")),
                other => return other,
            },
            Err(reason) => logger.line(&format!("bridge link lost: {reason}")),
        }
        session.on_disconnect();
        tokio::time::sleep(reconnect).await;
    }
}
