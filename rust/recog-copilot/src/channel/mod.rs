//! `recog-copilot-channel`, the port of `recog/copilot_channel.py`: the local end of
//! the Copilot channel, serving two roles at once.
//!
//! 1. An MCP Streamable HTTP server on `127.0.0.1:8767/mcp` ([`transport`], with the
//!    protocol on rmcp, [`handler`]). The Copilot Claude session connects to it and
//!    gets the tools `send_copilot_message`, `set_copilot_status`,
//!    `get_recent_transcript` and `await_copilot_event` ([`tools`]); bridge events are
//!    pushed to it as `notifications/claude/channel` ([`context`]).
//! 2. A WebSocket client of the bridge (`ws://127.0.0.1:8766`), which announces itself
//!    with `copilot_channel_hello`, relays the tools' frames, queues the bridge's
//!    events for the long-poll ([`link`]) and reports the number of reachable MCP
//!    sessions as `claude_sessions` for the bridge's status merge.
//!
//! Names never leave unmasked while privacy mode is on: every event and snapshot goes
//! through `recog-core`'s `CopilotPrivacy` at delivery.

pub mod context;
pub mod handler;
pub mod link;
pub mod pydantic;
pub mod tools;
pub mod transport;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use recog_core::privacy::PrivacyError;
use recog_core::protocol;
use recog_core::pyjson::{self, Map, Value};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use self::link::{BridgeLink, SendFuture, Socket};
use self::transport::McpServer;

pub use crate::log::line as log_line;

pub(crate) mod log {
    pub use crate::log::line;

    /// The prefix of every line the channel logs, which people grep the logs for.
    pub const PREFIX: &str = "[copilot-channel]";
}

pub const DEFAULT_BRIDGE_URL: &str = "ws://127.0.0.1:8766";
pub const DEFAULT_MCP_HOST: &str = "127.0.0.1";
pub const DEFAULT_MCP_PORT: u16 = 8767;

pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
pub const LOG_THROTTLE: Duration = Duration::from_secs(60);

/// The `websockets` client defaults the Python link runs with.
pub const MAX_MESSAGE_SIZE: usize = 1 << 20;
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
pub const PING_INTERVAL: Duration = Duration::from_secs(20);
pub const PING_TIMEOUT: Duration = Duration::from_secs(20);

/// The session-count poll: 5 s matches the bridge's debounce before a steady
/// `claude_sessions == 0` turns the chip offline.
pub const SESSION_COUNT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// A Claude session killed by `tmux kill-session` never sends the DELETE that ends its
/// MCP session. A session without a standing GET stream for 3 sweeps 60 s apart
/// (about 3 minutes, long enough for a new client to open its stream or a live one to
/// reconnect it) is reaped.
pub const ZOMBIE_REAP_INTERVAL: Duration = Duration::from_secs(60);
pub const ZOMBIE_REAP_STRIKES: u32 = 3;

/// The bridge events relayed to Claude.
pub const EVENT_TYPES: [&str; 6] = [
    protocol::COPILOT_USER_MESSAGE,
    protocol::TRANSCRIPT_TICK,
    protocol::MEETING_STARTED,
    protocol::MEETING_ENDED,
    protocol::USER_PROFILE_UPDATED,
    protocol::COPILOT_POLICY_UPDATED,
];

/// The Python exception class of a privacy failure, for the log line.
fn privacy_error_class(error: &PrivacyError) -> &'static str {
    match error {
        PrivacyError::Invalid(message) if message == "Invalid privacy setting" => "ValueError",
        PrivacyError::Invalid(message) if message.starts_with("'utf-8' codec") => "UnicodeDecodeError",
        PrivacyError::Invalid(_) => "JSONDecodeError",
        PrivacyError::Io(message) if message.starts_with("[Errno 13]") => "PermissionError",
        PrivacyError::Io(message) if message.starts_with("[Errno 21]") => "IsADirectoryError",
        PrivacyError::Io(message) if message.starts_with("[Errno 20]") => "NotADirectoryError",
        PrivacyError::Io(_) => "OSError",
    }
}

/// `_handle_inbound`: a bridge frame. A transcript snapshot answers the waiting
/// `get_recent_transcript`; an event is queued for the long-poll (making room by
/// dropping a tick first) and pushed to every reachable MCP session.
pub async fn handle_inbound(raw: &[u8], link: &BridgeLink, server: Option<&McpServer>) {
    let Ok(text) = std::str::from_utf8(raw) else { return };
    let Ok(Value::Object(data)) = pyjson::loads(text) else { return };
    let msg_type = data.get("type").cloned().unwrap_or(Value::Null);
    if msg_type == Value::from(protocol::GET_RECENT_TRANSCRIPT_RESPONSE) {
        if let Some(request_id) = data.get("request_id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            link.resolve_pending(request_id, data.clone());
        }
        return;
    }
    let Some(msg_type) = msg_type.as_str().filter(|t| EVENT_TYPES.contains(t)) else { return };
    if let Some(dropped) = link.events.push(data.clone()) {
        let dropped = dropped.get("type").cloned().unwrap_or(Value::Null);
        log::line(&format!("{} event_queue full — dropped oldest {}", log::PREFIX, dropped.py_repr()));
    }
    let Some(server) = server else { return };
    let repr = recog_core::py::repr(msg_type);
    let formatted = match link.apply_privacy(&data) {
        Ok(event) => context::format_channel_event(&event),
        Err(error) => Err(format!("{}: {error}", privacy_error_class(&error))),
    };
    match formatted {
        Ok((content, meta)) => {
            let sent = server.broadcast(&content, &meta);
            log::line(&format!("{} channel push {repr} → {sent} session(s)", log::PREFIX));
        }
        Err(error) => log::line(&format!("{} channel push failed for {repr}: {error}", log::PREFIX)),
    }
}

/// `SessionCountReporter`: reports the reachable MCP sessions to the bridge as
/// `claude_sessions` when the count changes, and once after every (re)connect so that
/// the bridge, which forgets across restarts, learns it before the first change. The
/// count is always known here, never `null`.
pub struct SessionCountReporter {
    server: Arc<McpServer>,
    last_count: Option<usize>,
    reported_epoch: u64,
}

impl SessionCountReporter {
    pub fn new(server: Arc<McpServer>) -> Self {
        SessionCountReporter { server, last_count: None, reported_epoch: 0 }
    }

    /// One poll: the send result when a frame went out.
    pub async fn poll_once(&mut self) -> Option<Map> {
        let link = &self.server.link;
        if !link.is_connected() {
            return None;
        }
        let epoch = link.connection_epoch();
        let count = self.server.reachable_count();
        if epoch == self.reported_epoch && Some(count) == self.last_count {
            return None;
        }
        let mut frame = Map::new();
        frame.insert("type", Value::from(protocol::CLAUDE_SESSIONS));
        frame.insert("count", Value::Int(i128::try_from(count).unwrap_or(i128::MAX)));
        let result = link.send(&frame).await;
        self.last_count = Some(count);
        self.reported_epoch = epoch;
        Some(result)
    }
}

/// `ZombieSessionReaper`: ends sessions whose client died without terminating them.
pub struct ZombieSessionReaper {
    server: Arc<McpServer>,
    threshold: u32,
    strikes: HashMap<String, u32>,
}

impl ZombieSessionReaper {
    pub fn new(server: Arc<McpServer>, strikes_to_reap: u32) -> Self {
        ZombieSessionReaper { server, threshold: strikes_to_reap, strikes: HashMap::new() }
    }

    /// One sweep; returns the reaped session ids.
    pub fn sweep_once(&mut self) -> Vec<String> {
        let mut reaped = Vec::new();
        for session in self.server.session_list() {
            if session.channel_reachable() {
                self.strikes.remove(&session.id);
                continue;
            }
            let strikes = self.strikes.entry(session.id.clone()).or_insert(0);
            *strikes += 1;
            if *strikes < self.threshold {
                continue;
            }
            self.strikes.remove(&session.id);
            if let Some(session) = self.server.remove_session(&session.id) {
                session.terminate();
            }
            reaped.push(session.id.clone());
        }
        if !reaped.is_empty() {
            log::line(&format!("{} reaped {} zombie MCP session(s): {}", log::PREFIX, reaped.len(), reaped.join(", ")));
        }
        let alive: Vec<String> = self.server.session_list().iter().map(|s| s.id.clone()).collect();
        self.strikes.retain(|id, _| alive.contains(id));
        reaped
    }
}

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;

/// The bridge WebSocket's writing half as the link's socket.
struct WsSocket(Arc<AsyncMutex<WsSink>>);

impl Socket for WsSocket {
    fn send(&self, text: String) -> SendFuture {
        let sink = self.0.clone();
        Box::pin(async move { sink.lock().await.send(WsMessage::text(text)).await.map_err(|e| ws_error(&e)) })
    }
}

/// A WebSocket error in the `<Class>: <message>` form the Python link reports.
fn ws_error(error: &tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error;
    let class = match error {
        Error::ConnectionClosed | Error::AlreadyClosed => "ConnectionClosed",
        Error::Io(_) => "OSError",
        Error::Protocol(_) => "ProtocolError",
        _ => "WebSocketException",
    };
    format!("{class}: {error}")
}

/// `_ws_loop`: keeps the bridge connection with reconnect and backoff (1 s doubling to
/// 30 s), announcing itself with `copilot_channel_hello` on every connect.
pub async fn ws_loop(bridge_url: String, server: Arc<McpServer>, push: bool) {
    let link = server.link.clone();
    let mut delay = INITIAL_BACKOFF;
    let mut failure_streak = 0u32;
    let mut last_failure_log: Option<Instant> = None;
    let mut connected_once = false;
    // The limits of the `websockets` client the Python channel connects with: 1 MiB
    // messages, 10 s to open, a keepalive ping every 20 s answered within 20 s.
    let config =
        WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE_SIZE)).max_frame_size(Some(MAX_MESSAGE_SIZE));
    loop {
        let connect = tokio_tungstenite::connect_async_with_config(bridge_url.as_str(), Some(config), false);
        let connected = match tokio::time::timeout(OPEN_TIMEOUT, connect).await {
            Ok(connected) => connected.map_err(|e| ws_error(&e)),
            Err(_) => Err("TimeoutError: timed out during opening handshake".to_owned()),
        };
        match connected {
            Ok((ws, _)) => {
                let (mut sink, mut stream) = ws.split();
                if let Err(e) =
                    sink.send(WsMessage::text(format!(r#"{{"type": "{}"}}"#, protocol::COPILOT_CHANNEL_HELLO))).await
                {
                    log::line(&format!("{} copilot_channel_hello send failed: {}", log::PREFIX, ws_error(&e)));
                }
                let sink = Arc::new(AsyncMutex::new(sink));
                link.set_socket(Some(Arc::new(WsSocket(sink.clone()))));
                failure_streak = 0;
                let word = if connected_once { "reconnected" } else { "connected" };
                connected_once = true;
                log::line(&format!("{} {word} to {bridge_url}", log::PREFIX));
                delay = INITIAL_BACKOFF;
                let mut failure = None;
                let mut next_ping = tokio::time::Instant::now() + PING_INTERVAL;
                let mut pong_due: Option<tokio::time::Instant> = None;
                loop {
                    let deadline = pong_due.unwrap_or(next_ping);
                    let frame = tokio::select! {
                        frame = stream.next() => frame,
                        () = tokio::time::sleep_until(deadline) => {
                            if pong_due.is_some() {
                                failure = Some(
                                    "ConnectionClosedError: sent 1011 (internal error) keepalive ping timeout; \
                                     no close frame received"
                                        .to_owned(),
                                );
                                break;
                            }
                            let payload = uuid::Uuid::new_v4().as_bytes()[..4].to_vec();
                            if let Err(e) = sink.lock().await.send(WsMessage::Ping(payload.into())).await {
                                failure = Some(ws_error(&e));
                                break;
                            }
                            pong_due = Some(tokio::time::Instant::now() + PING_TIMEOUT);
                            next_ping = tokio::time::Instant::now() + PING_INTERVAL;
                            continue;
                        }
                    };
                    match frame {
                        Some(Ok(WsMessage::Text(text))) => {
                            handle_inbound(text.as_bytes(), &link, push.then_some(&*server)).await;
                        }
                        Some(Ok(WsMessage::Binary(bytes))) => {
                            handle_inbound(&bytes, &link, push.then_some(&*server)).await;
                        }
                        Some(Ok(WsMessage::Pong(_))) => pong_due = None,
                        Some(Ok(WsMessage::Close(_))) | None => break,
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            failure = Some(ws_error(&e));
                            break;
                        }
                    }
                }
                link.set_socket(None);
                if let Some(error) = failure {
                    failure_streak += 1;
                    log_failure(&mut last_failure_log, failure_streak, &error, delay);
                }
            }
            Err(error) => {
                failure_streak += 1;
                log_failure(&mut last_failure_log, failure_streak, &error, delay);
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_BACKOFF);
    }
}

fn log_failure(last: &mut Option<Instant>, streak: u32, error: &str, delay: Duration) {
    let now = Instant::now();
    if streak == 1 || last.is_none_or(|t| now.duration_since(t) >= LOG_THROTTLE) {
        log::line(&format!("{} bridge unavailable ({error}); retry in {:.1}s", log::PREFIX, delay.as_secs_f64()));
        *last = Some(now);
    }
}

/// `_session_count_poll_loop`: a poll every 5 s, the first at once.
pub async fn session_count_loop(mut reporter: SessionCountReporter) {
    loop {
        reporter.poll_once().await;
        tokio::time::sleep(SESSION_COUNT_POLL_INTERVAL).await;
    }
}

/// `_zombie_reaper_loop`: a sweep every 60 s, the first after one interval.
pub async fn zombie_reaper_loop(mut reaper: ZombieSessionReaper) {
    loop {
        tokio::time::sleep(ZOMBIE_REAP_INTERVAL).await;
        reaper.sweep_once();
    }
}

/// `_serve`: the MCP endpoint, the bridge link, the session-count reporter and the
/// zombie reaper, until the endpoint or the link stops.
///
/// # Errors
/// The MCP port cannot be bound or accepting fails.
pub async fn serve(bridge_url: &str, link: Arc<BridgeLink>, host: &str, port: u16) -> std::io::Result<()> {
    log::line(&format!("{} MCP Streamable HTTP listening on http://{host}:{port}/mcp", log::PREFIX));
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    serve_on(bridge_url, link, host, listener).await
}

/// [`serve`] on a bound listener.
///
/// # Errors
/// Accepting a connection failed.
pub async fn serve_on(
    bridge_url: &str,
    link: Arc<BridgeLink>,
    host: &str,
    listener: tokio::net::TcpListener,
) -> std::io::Result<()> {
    let server = Arc::new(McpServer::new(link, host));
    let ws = tokio::spawn(ws_loop(bridge_url.to_owned(), server.clone(), true));
    let poll = tokio::spawn(session_count_loop(SessionCountReporter::new(server.clone())));
    let reap = tokio::spawn(zombie_reaper_loop(ZombieSessionReaper::new(server.clone(), ZOMBIE_REAP_STRIKES)));
    let result = server.serve(listener).await;
    for task in [ws, poll, reap] {
        task.abort();
    }
    result
}
