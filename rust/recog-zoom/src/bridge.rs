//! The link to the bridge: snapshots in from the helper, frames out to the bridge.
//!
//! The source connects as the extension does, but sends no `Origin` header, so the
//! bridge treats it as a local client (`recog/ws_origin.py`); its disconnect ends the
//! meeting. The WebSocket runs in a task of its own behind two channels, which keeps
//! the pump's wait on "a helper line or a bridge message" cancel-safe.

use std::future::Future;
use std::ops::ControlFlow;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use recog_core::protocol;
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
        let pong_deadline = pong_due(awaiting_pong, keepalive.ping_timeout);
        let flow = tokio::select! {
            message = socket.next() => on_socket_message(message, &incoming, &mut awaiting_pong).await,
            frame = outgoing.recv() => on_outgoing(frame, &mut socket).await,
            _ = ping.tick(), if awaiting_pong.is_none() => send_ping(&mut socket, &mut awaiting_pong).await,
            () = pong_deadline => ControlFlow::Break(Some("keepalive ping timeout".into())),
        };
        if let ControlFlow::Break(reason) = flow {
            break reason;
        }
    };
    if let Some(reason) = lost {
        let _ = incoming.send(Incoming::Lost(reason)).await;
    }
}

/// What a turn of [`drive`] decided: go on, or stop with the reason the link is lost
/// (None when the other end of the channels is gone).
type Flow = ControlFlow<Option<String>>;

/// Resolves when the ping sent at `awaiting` has gone unanswered for `timeout`.
async fn pong_due(awaiting: Option<tokio::time::Instant>, timeout: Duration) {
    match awaiting {
        Some(sent) => tokio::time::sleep_until(sent + timeout).await,
        None => std::future::pending().await,
    }
}

/// A message from the bridge: data to `incoming`, a pong ends the wait for one.
async fn on_socket_message(
    message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    incoming: &mpsc::Sender<Incoming>,
    awaiting_pong: &mut Option<tokio::time::Instant>,
) -> Flow {
    match message {
        Some(Ok(Message::Text(text))) => forward(incoming, text.as_bytes().to_vec()).await,
        Some(Ok(Message::Binary(bytes))) => forward(incoming, bytes.to_vec()).await,
        Some(Ok(Message::Pong(_))) => {
            *awaiting_pong = None;
            ControlFlow::Continue(())
        }
        Some(Ok(Message::Close(frame))) => {
            ControlFlow::Break(Some(frame.map_or_else(
                || "connection closed".into(),
                |f| format!("received {} ({})", u16::from(f.code), f.reason),
            )))
        }
        Some(Ok(_)) => ControlFlow::Continue(()),
        Some(Err(error)) => ControlFlow::Break(Some(error.to_string())),
        None => ControlFlow::Break(Some("connection closed".into())),
    }
}

/// Data for the pump; a pump that is gone stops the link quietly.
async fn forward(incoming: &mpsc::Sender<Incoming>, bytes: Vec<u8>) -> Flow {
    match incoming.send(Incoming::Message(bytes)).await {
        Ok(()) => ControlFlow::Continue(()),
        Err(_) => ControlFlow::Break(None),
    }
}

/// A frame from the pump to the bridge; the pump done with the link closes it.
async fn on_outgoing(frame: Option<String>, socket: &mut Socket) -> Flow {
    match frame {
        Some(text) => match socket.send(Message::text(text)).await {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(Some(error.to_string())),
        },
        None => {
            let _ = socket.close(None).await;
            ControlFlow::Break(None)
        }
    }
}

/// The keepalive ping; its pong is awaited from now.
async fn send_ping(socket: &mut Socket, awaiting_pong: &mut Option<tokio::time::Instant>) -> Flow {
    if let Err(error) = socket.send(Message::Ping(Vec::new().into())).await {
        return ControlFlow::Break(Some(error.to_string()));
    }
    *awaiting_pong = Some(tokio::time::Instant::now());
    ControlFlow::Continue(())
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
    if message.get("type").and_then(Value::as_str) == Some(protocol::MEETING_JOIN_REJECTED) {
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
        let stopped = tokio::select! {
            // A message that arrived with a line is handled first, as in Python.
            biased;
            message = link.incoming.recv() => on_incoming(message, session, logger),
            line = lines.next_line() => on_helper_line(line, link, session, log).await,
        };
        if let Some(error) = stopped {
            return error;
        }
    }
}

/// A bridge message, or the end of the link; Some stops the pump.
fn on_incoming(message: Option<Incoming>, session: &mut ZoomAppSession, logger: &Logger) -> Option<PumpError> {
    match message {
        Some(Incoming::Message(raw)) => on_message(&raw, session, logger, now()).err(),
        Some(Incoming::Lost(reason)) => Some(PumpError::Link(reason)),
        None => Some(PumpError::Link("connection closed".into())),
    }
}

/// A helper line, the helper's end, or a failed read; Some stops the pump.
async fn on_helper_line(
    line: std::io::Result<Option<Vec<u8>>>,
    link: &mut BridgeLink,
    session: &mut ZoomAppSession,
    log: &mut StatusLog,
) -> Option<PumpError> {
    match line {
        Ok(Some(raw)) => match on_line(&raw, session, log, now()) {
            Ok(frames) => send_frames(link, frames).await,
            Err(error) => Some(error),
        },
        Ok(None) => Some(PumpError::HelperGone),
        Err(error) => Some(read_error(&error)),
    }
}

/// An oversized line is one Python raises on; any other read error is a lost link.
fn read_error(error: &std::io::Error) -> PumpError {
    if error.kind() == std::io::ErrorKind::InvalidData {
        PumpError::Fatal(error.to_string())
    } else {
        PumpError::Link(error.to_string())
    }
}

/// The frames to the bridge; a bridge that stopped reading reports why.
async fn send_frames(link: &mut BridgeLink, frames: Vec<String>) -> Option<PumpError> {
    for frame in frames {
        if link.outgoing.send(frame).await.is_err() {
            return Some(lost_reason(link).await);
        }
    }
    None
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
