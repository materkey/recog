//! The WebSocket side: `_handle_client` (the Origin gate, roles, the read loop and the
//! disconnect cleanup), the listener with its periodic duties (`_run_server`), and the
//! background thread of `start`/`stop`.
//!
//! The transport keeps what the Python server (`websockets.serve` with its defaults)
//! shows a peer: 1 MiB messages, a ping every 20 s that closes a connection whose pong
//! does not come within 20 s (1011), pongs to the clients' own pings, text and binary
//! JSON, a close with 1000 when the handler ends, and a handshake with two Origin
//! headers refused with 400. One server serves every role, as
//! in Python: the extension, the webapp's subscriber, the supervisor and the Copilot
//! channel all connect to `ws://localhost:8766` and tell apart by their hello.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{SplitSink, StreamExt};
use futures_util::{SinkExt, future};
use recog_core::protocol;
use recog_core::pyjson::{self, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::bridge::Bridge;
use crate::client::{Client, ClientSink, Connection, Inbound, SendError, python_address};
use crate::hooks::BoxFuture;
use crate::obj;

const MAX_MESSAGE: usize = 1 << 20;
const PING_INTERVAL: Duration = Duration::from_secs(20);

type WsSink = SplitSink<WebSocketStream<TcpStream>, Message>;

/// The sending half of a real connection; sends wait for the socket to take them.
struct WebSocketSink(Arc<tokio::sync::Mutex<WsSink>>);

fn closed_text(error: &tungstenite::Error) -> String {
    match error {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            "sent 1000 (OK); then received 1000 (OK)".to_owned()
        }
        other => other.to_string(),
    }
}

impl ClientSink for WebSocketSink {
    fn send(&self, text: String) -> BoxFuture<Result<(), SendError>> {
        let sink = self.0.clone();
        Box::pin(
            async move { sink.lock().await.send(Message::text(text)).await.map_err(|e| SendError(closed_text(&e))) },
        )
    }

    fn close(&self, code: u16, reason: String) -> BoxFuture<Result<(), SendError>> {
        let sink = self.0.clone();
        Box::pin(async move {
            let frame = CloseFrame { code: CloseCode::from(code), reason: reason.into() };
            match sink.lock().await.send(Message::Close(Some(frame))).await {
                Ok(()) | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => Ok(()),
                Err(e) => Err(SendError(e.to_string())),
            }
        })
    }
}

fn close_name(code: u16) -> &'static str {
    match code {
        1000 => "OK",
        1001 => "going away",
        1002 => "protocol error",
        1003 => "unsupported data",
        1007 => "invalid frame payload data",
        1008 => "policy violation",
        1009 => "message too big",
        1011 => "internal error",
        _ => "unknown",
    }
}

/// `str(ConnectionClosedError)` for a peer's close that is not a clean one.
fn peer_close_text(frame: Option<&CloseFrame>) -> Option<String> {
    let frame = frame?;
    let code = u16::from(frame.code);
    if matches!(code, 1000 | 1001) {
        return None;
    }
    let reason = if frame.reason.is_empty() { String::new() } else { format!(" {}", frame.reason) };
    Some(format!("received {code} ({}){reason}; then sent {code} ({}){reason}", close_name(code), close_name(code)))
}

/// Reads the socket into `inbound`, answers pings, sends keepalive pings.
async fn pump(
    mut stream: futures_util::stream::SplitStream<WebSocketStream<TcpStream>>,
    sink: Arc<tokio::sync::Mutex<WsSink>>,
    inbound: mpsc::UnboundedSender<Result<Inbound, String>>,
) {
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    let mut awaiting_pong = false;
    loop {
        tokio::select! {
            message = stream.next() => if !on_read(message, &sink, &inbound, &mut awaiting_pong).await {
                return;
            },
            _ = ticker.tick() => if !keepalive(&sink, &inbound, &mut awaiting_pong).await {
                return;
            }
        }
    }
}

type InboundTx = mpsc::UnboundedSender<Result<Inbound, String>>;

/// One read of the socket; false ends the reading.
async fn on_read(
    message: Option<Result<Message, tungstenite::Error>>,
    sink: &tokio::sync::Mutex<WsSink>,
    inbound: &InboundTx,
    awaiting_pong: &mut bool,
) -> bool {
    match message {
        Some(Ok(message)) => on_message(message, sink, inbound, awaiting_pong).await,
        Some(Err(error)) => {
            on_read_error(error, sink, inbound).await;
            false
        }
        None => false,
    }
}

/// A message the peer sent; false after its close.
async fn on_message(
    message: Message,
    sink: &tokio::sync::Mutex<WsSink>,
    inbound: &InboundTx,
    awaiting_pong: &mut bool,
) -> bool {
    match message {
        Message::Text(text) => {
            let _ = inbound.send(Ok(Inbound::Text(text.as_str().to_owned())));
        }
        Message::Binary(bytes) => {
            let _ = inbound.send(Ok(Inbound::Binary(bytes.to_vec())));
        }
        Message::Ping(_) => {
            // The pong is queued by the read; a flush sends it.
            let _ = sink.lock().await.flush().await;
        }
        Message::Pong(_) => *awaiting_pong = false,
        Message::Close(frame) => {
            if let Some(text) = peer_close_text(frame.as_ref()) {
                let _ = inbound.send(Err(text));
            }
            let _ = sink.lock().await.flush().await;
            return false;
        }
        Message::Frame(_) => {}
    }
    true
}

/// A read that failed: what the handler logs as the connection error, and the
/// close a too-big message gets.
async fn on_read_error(error: tungstenite::Error, sink: &tokio::sync::Mutex<WsSink>, inbound: &InboundTx) {
    let text = match error {
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => return,
        tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake) => {
            "no close frame received or sent".to_owned()
        }
        tungstenite::Error::Capacity(_) => {
            let frame = CloseFrame { code: CloseCode::Size, reason: "".into() };
            let _ = sink.lock().await.send(Message::Close(Some(frame))).await;
            "sent 1009 (message too big); no close frame received".to_owned()
        }
        e => e.to_string(),
    };
    let _ = inbound.send(Err(text));
}

/// A keepalive tick: a ping, or a 1011 close when the last one got no pong; false
/// ends the reading.
async fn keepalive(sink: &tokio::sync::Mutex<WsSink>, inbound: &InboundTx, awaiting_pong: &mut bool) -> bool {
    if *awaiting_pong {
        let frame = CloseFrame { code: CloseCode::Error, reason: "keepalive ping timeout".into() };
        let _ = sink.lock().await.send(Message::Close(Some(frame))).await;
        let _ =
            inbound.send(Err("sent 1011 (internal error) keepalive ping timeout; no close frame received".to_owned()));
        return false;
    }
    *awaiting_pong = true;
    sink.lock().await.send(Message::Ping(Vec::new().into())).await.is_ok()
}

async fn serve_connection(bridge: Arc<Bridge>, tcp: TcpStream, peer: SocketAddr) {
    let origins = Arc::new(Mutex::new(Vec::new()));
    let seen = origins.clone();
    #[allow(clippy::result_large_err)] // the signature tungstenite's callback has
    let callback = move |request: &Request, response: Response| {
        let values: Vec<String> = request
            .headers()
            .get_all("origin")
            .iter()
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .collect();
        // websockets refuses a second Origin at the handshake (RFC 6454 allows one),
        // before the bridge's handler sees the connection.
        if values.len() > 1 {
            let body = "Failed to open a WebSocket connection: invalid Origin header: multiple values.\n";
            let mut refusal = ErrorResponse::new(Some(body.to_owned()));
            *refusal.status_mut() = StatusCode::BAD_REQUEST;
            let headers = refusal.headers_mut();
            headers.insert("Connection", HeaderValue::from_static("close"));
            headers.insert("Content-Type", HeaderValue::from_static("text/plain; charset=utf-8"));
            return Err(refusal);
        }
        seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(values);
        Ok(response)
    };
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    let ws = match tokio_tungstenite::accept_hdr_async_with_config(tcp, callback, Some(config)).await {
        Ok(ws) => ws,
        Err(e) => {
            // websockets logs a refused handshake (with its traceback).
            if let tungstenite::Error::Http(response) = &e {
                let reason =
                    String::from_utf8_lossy(response.body().as_deref().unwrap_or_default()).trim_end().to_owned();
                let reason = reason.strip_prefix("Failed to open a WebSocket connection: ").unwrap_or(&reason);
                bridge.log(&format!("opening handshake failed: {}", reason.trim_end_matches('.')));
            }
            return;
        }
    };
    let (sink, stream) = ws.split();
    let sink = Arc::new(tokio::sync::Mutex::new(sink));
    let (tx, rx) = mpsc::unbounded_channel();
    let reader = tokio::spawn(pump(stream, sink.clone(), tx));
    let origins = std::mem::take(&mut *origins.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    let inbound = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) }).boxed();
    let connection = Connection {
        sink: Arc::new(WebSocketSink(sink.clone())),
        inbound,
        remote_address: python_address(&peer),
        origins: Some(origins),
    };
    bridge.handle_client(connection).await;
    // The handler is done: the server closes the connection, as websockets does.
    let frame = CloseFrame { code: CloseCode::Normal, reason: "".into() };
    let _ = sink.lock().await.send(Message::Close(Some(frame))).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), reader).await;
}

/// The head of a message that is not JSON, as the log shows it.
fn message_head(message: &Inbound) -> String {
    match message {
        Inbound::Text(text) => text.chars().take(100).collect::<String>(),
        Inbound::Binary(bytes) => format!("{:?}", String::from_utf8_lossy(&bytes[..bytes.len().min(100)])),
    }
}

/// What a socket that left the sets was, and what it left behind.
#[derive(Default)]
struct Departure {
    was_subscriber: bool,
    was_channel: bool,
    was_supervisor: bool,
    was_owner: bool,
    no_extensions: bool,
    no_channels: bool,
    no_supervisors: bool,
}

impl Departure {
    /// The role the disconnect log names.
    fn role(&self) -> &'static str {
        if self.was_supervisor {
            "Copilot supervisor"
        } else if self.was_channel {
            "Copilot channel"
        } else if self.was_subscriber {
            "Subscriber"
        } else {
            "Client"
        }
    }
}

impl Bridge {
    /// `_handle_client(websocket)`: one connection from accept to cleanup.
    pub async fn handle_client(self: &Arc<Self>, connection: Connection) {
        let Connection { sink, mut inbound, remote_address, origins } = connection;
        // WebSockets bypass CORS: refuse pages that are neither a conference host nor
        // the webapp before the socket is registered or a frame is read. The Python
        // clients send no Origin and pass.
        if let Some(reason) = self.origin_rejection(origins.as_deref()) {
            self.log_rate_limited(
                "bridge-ws-origin-reject",
                &format!("[KTalk Bridge] Rejected WS connection: {reason} ({remote_address})"),
            );
            let _ = sink.close(1008, "origin not allowed".to_owned()).await;
            return;
        }
        let (client, channels_present) = self.register_client(sink, remote_address);
        let golden = self.wired().golden;
        let golden_client = golden.as_ref().map(|tap| {
            let n = {
                let mut st = self.st();
                st.golden_clients += 1;
                st.golden_clients
            };
            tap.client_connected(n);
            n
        });
        self.send_primers(&client, channels_present).await;
        while let Some(message) = inbound.next().await {
            let message = match message {
                Ok(message) => message,
                Err(e) => {
                    self.log(&format!("[KTalk Bridge] Connection error: {e}"));
                    break;
                }
            };
            let Some(data) = self.parse_inbound(&message) else { continue };
            if let (Some(tap), Some(n)) = (&golden, golden_client) {
                tap.frame(n, &data);
            }
            self.handle_frame(&client, &data).await;
        }
        if let (Some(tap), Some(n)) = (&golden, golden_client) {
            tap.client_disconnected(n);
        }
        self.disconnect(&client).await;
    }

    /// Why a handshake's Origin headers refuse the connection, if they do: more than
    /// one value, or one the allowlist does not have (none passes).
    fn origin_rejection(&self, origins: Option<&[String]>) -> Option<String> {
        let mut origin = None;
        if let Some(values) = origins {
            match values {
                [] => {}
                [one] => origin = Some(one.as_str()),
                _ => return Some("duplicate Origin headers".to_owned()),
            }
        }
        if recog_core::ws_origin::origin_allowed(origin, &self.config.extra_origins) {
            return None;
        }
        let shown = origin.map_or_else(|| "None".to_owned(), recog_core::py::repr);
        Some(format!("disallowed origin {shown}"))
    }

    /// The new socket counts as an extension until its hello; true when a Copilot
    /// channel is connected.
    fn register_client(&self, sink: Arc<dyn ClientSink>, remote_address: String) -> (Arc<Client>, bool) {
        let client = Client::new(self.next_client_id(), sink, remote_address);
        let channels_present = {
            let mut st = self.st();
            st.clients.all.insert(client.id, client.clone());
            st.clients.connected.insert(client.id);
            // The extension restarts its counter on reload. The clock offset stays: a
            // subscriber reconnect must not wipe the extension's sync.
            st.last_seq = Value::Int(0);
            !st.clients.channels.is_empty()
        };
        self.log(&format!("[KTalk Bridge] Client connected from {}", client.remote_address));
        (client, channels_present)
    }

    /// The Copilot state a new socket must know: the channel when one is connected,
    /// and the agent chip.
    async fn send_primers(&self, client: &Arc<Client>, channels_present: bool) {
        if channels_present {
            let frame = obj! {"type" => protocol::COPILOT_STATUS, "request_id" => "", "status" => "channel_connected", "message" => "Copilot channel server connected"};
            if let Err(e) = client.sink.send(crate::json::dumps(&frame)).await {
                self.log(&format!("[KTalk Bridge] Failed to send channel_connected primer to new client: {e}"));
            }
        }
        // The chip broadcast is change-only: a reloaded tab learns the current state here.
        let agent_state = self.st().agent.last_effective.clone();
        let frame = obj! {"type" => protocol::COPILOT_STATUS, "request_id" => "", "status" => protocol::STATUS_AGENT_STATE, protocol::STATUS_AGENT_STATE => agent_state};
        if let Err(e) = client.sink.send(crate::json::dumps(&frame)).await {
            self.log(&format!("[KTalk Bridge] Failed to send agent_state primer to new client: {e}"));
        }
    }

    /// `json.loads(message)`, or None with the message's head logged.
    fn parse_inbound(&self, message: &Inbound) -> Option<Value> {
        let parsed = match message {
            Inbound::Text(text) => pyjson::loads(text),
            Inbound::Binary(bytes) => pyjson::loads_bytes(bytes),
        };
        if let Ok(data) = parsed {
            return Some(data);
        }
        self.log(&format!("[KTalk Bridge] Invalid JSON: {}", message_head(message)));
        None
    }

    /// One parsed frame: a subscriber's hello moves the socket, any other object goes
    /// to [`Bridge::process_message`]; an error is logged as Python's handler does.
    async fn handle_frame(self: &Arc<Self>, client: &Arc<Client>, data: &Value) {
        let Value::Object(map) = data else {
            self.log(&format!(
                "[KTalk Bridge] Error processing message: {}",
                crate::json::attribute_error(data, "get")
            ));
            return;
        };
        if map.get("type") == Some(&Value::from(protocol::SUBSCRIBER_HELLO)) {
            self.register_subscriber(client).await;
            return;
        }
        if let Err(e) = self.process_message(map, Some(client)).await {
            self.log(&format!("[KTalk Bridge] Error processing message: {e}"));
        }
    }

    /// `subscriber_hello`: the socket leaves the extensions for the subscribers.
    async fn register_subscriber(self: &Arc<Self>, client: &Arc<Client>) {
        let no_extensions = {
            let mut st = self.st();
            st.clients.connected.remove(&client.id);
            st.clients.subscribers.insert(client.id);
            st.clients.connected.is_empty()
        };
        self.log(&format!("[KTalk Bridge] Subscriber registered from {}", client.remote_address));
        // The socket counted as an extension until now: one that left meanwhile
        // kept the meeting open.
        if no_extensions && let Err(e) = self.end_meeting_on_extension_gone().await {
            self.log(&format!("[KTalk Bridge] Error processing message: {e}"));
        }
    }

    /// The `finally` of `_handle_client`: the socket leaves every set, its role's
    /// cleanup runs, and the meeting ends when its owner or the last extension left.
    async fn disconnect(self: &Arc<Self>, client: &Arc<Client>) {
        let gone = self.leave_client_sets(client);
        self.log(&format!("[KTalk Bridge] {} disconnected", gone.role()));
        if gone.was_channel && gone.no_channels {
            self.on_last_channel_gone().await;
        }
        if gone.was_supervisor && gone.no_supervisors {
            self.st().agent.supervisor_status = None;
            self.recompute_agent_state(None).await;
        }
        if (gone.no_extensions || gone.was_owner)
            && let Err(e) = self.end_meeting_on_extension_gone().await
        {
            self.log(&format!("[KTalk Bridge] Connection error: {e}"));
        }
    }

    /// Takes the socket out of every set (and the meeting's ownership).
    fn leave_client_sets(&self, client: &Arc<Client>) -> Departure {
        let mut st = self.st();
        let c = &mut st.clients;
        let mut gone = Departure {
            was_subscriber: c.subscribers.contains(&client.id),
            was_channel: c.channels.contains(&client.id),
            was_supervisor: c.supervisors.contains(&client.id),
            ..Departure::default()
        };
        c.connected.remove(&client.id);
        c.subscribers.remove(&client.id);
        c.channels.remove(&client.id);
        c.supervisors.remove(&client.id);
        c.timed_out.remove(&client.id);
        c.all.remove(&client.id);
        (gone.no_extensions, gone.no_channels, gone.no_supervisors) =
            (c.connected.is_empty(), c.channels.is_empty(), c.supervisors.is_empty());
        gone.was_owner = st.meeting_owner == Some(client.id);
        if gone.was_owner {
            st.meeting_owner = None;
        }
        gone
    }

    /// The last channel gone strands every in-flight request: drop them and tell the
    /// UIs, or they wait forever for Claude.
    async fn on_last_channel_gone(&self) {
        let stranded = {
            let mut st = self.st();
            st.agent.claude_sessions = None;
            let before = st.pending.len();
            st.pending.retain(|_, mid| mid.starts_with("stale:"));
            before - st.pending.len()
        };
        if stranded > 0 {
            self.log(&format!(
                "[KTalk Bridge] Channel disconnected with {stranded} in-flight request(s); dropped bookkeeping"
            ));
        }
        let frame = obj! {"type" => protocol::COPILOT_STATUS, "request_id" => "", "status" => "channel_disconnected", "message" => "Copilot channel server disconnected"};
        self.broadcast_copilot_frame(&frame).await;
    }

    /// Binds the configured host and port: every address the host resolves to.
    ///
    /// # Errors
    /// The host does not resolve or a port is taken.
    pub async fn bind(&self) -> std::io::Result<Vec<TcpListener>> {
        let mut listeners = Vec::new();
        for addr in tokio::net::lookup_host((self.config.host.as_str(), self.config.port)).await? {
            let socket =
                if addr.is_ipv4() { tokio::net::TcpSocket::new_v4()? } else { tokio::net::TcpSocket::new_v6()? };
            socket.set_reuseaddr(true)?;
            socket.bind(addr)?;
            listeners.push(socket.listen(100)?);
        }
        Ok(listeners)
    }

    /// `_run_server`: accepts on the listeners and runs the periodic duties (segment
    /// timeout and agent-state recompute every second, the debug file) until `stop`.
    /// Runs on a current-thread runtime, as the bridge's own loop.
    pub async fn serve(self: &Arc<Self>, listeners: Vec<TcpListener>) {
        self.attach_runtime(tokio::runtime::Handle::current());
        self.st().running = true;
        self.log(&format!("[KTalk Bridge] WebSocket server started on ws://{}:{}", self.config.host, self.config.port));
        let accepts: Vec<_> = listeners
            .into_iter()
            .map(|listener| {
                let bridge = self.clone();
                tokio::spawn(async move { bridge.accept(listener).await })
            })
            .collect();
        let mut ticker = tokio::time::interval(Duration::from_millis(100));
        ticker.tick().await;
        let mut count: u64 = 0;
        loop {
            tokio::select! {
                () = self.stop_signal.notified() => break,
                _ = ticker.tick() => {}
            }
            if !self.st().running {
                break;
            }
            count += 1;
            if count.is_multiple_of(10) {
                self.periodic().await;
            }
            self.write_debug_state();
        }
        for accept in accepts {
            accept.abort();
        }
        if let Err(e) = self.set_meeting_capture_active(false) {
            self.log(&format!("[KTalk Bridge] Server error: {e}"));
        }
        self.drain_background_tasks(Duration::from_secs(30)).await;
    }

    /// Accepts connections on one listener and serves each with
    /// [`Bridge::handle_client`], without the periodic duties of [`Bridge::serve`]
    /// (what `websockets.serve(bridge._handle_client, ...)` alone does).
    pub async fn accept(self: &Arc<Self>, listener: TcpListener) {
        while let Ok((tcp, peer)) = listener.accept().await {
            tokio::spawn(serve_connection(self.clone(), tcp, peer));
        }
    }

    /// The once-a-second duties of the server loop.
    pub async fn periodic(&self) {
        let now = self.now();
        let segment = self.st().speakers.check_segment_timeout(now);
        if let (Some(segment), Some(live)) = (segment, self.wired().live) {
            live.segment_completed(&segment);
        }
        // Supervisor silence must flip the chip with no frame to trigger the merge.
        self.recompute_agent_state(None).await;
    }

    /// `_drain_background_tasks`: in-flight increment generation gets `timeout`.
    pub async fn drain_background_tasks(&self, timeout: Duration) {
        let pending: Vec<_> =
            std::mem::take(&mut *self.background.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
                .into_iter()
                .filter(|t| !t.is_finished())
                .collect();
        if pending.is_empty() {
            return;
        }
        self.log(&format!("[KTalk Bridge] Draining {} background task(s)…", pending.len()));
        let aborts: Vec<_> = pending.iter().map(tokio::task::JoinHandle::abort_handle).collect();
        if tokio::time::timeout(timeout, future::join_all(pending)).await.is_err() {
            let cancelled = aborts.iter().filter(|a| !a.is_finished()).count();
            for abort in &aborts {
                abort.abort();
            }
            self.log(&format!(
                "[KTalk Bridge] {cancelled} background task(s) cancelled after {}s timeout",
                recog_core::py::float_repr(timeout.as_secs_f64())
            ));
        }
    }

    /// `start()`: the server on its own thread; returns the bound addresses.
    ///
    /// # Errors
    /// Binding failed (logged as Python's `Server error`).
    pub fn start(self: &Arc<Self>) -> std::io::Result<Vec<SocketAddr>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let bridge = self.clone();
        let thread = std::thread::Builder::new().name("ktalk-bridge".to_owned()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, async move {
                match bridge.bind().await {
                    Ok(listeners) => {
                        let addrs = listeners.iter().filter_map(|l| l.local_addr().ok()).collect();
                        let _ = tx.send(Ok(addrs));
                        bridge.serve(listeners).await;
                    }
                    Err(e) => {
                        bridge.log(&format!("[KTalk Bridge] Server error: {e}"));
                        let _ = tx.send(Err(e));
                    }
                }
            });
        })?;
        *self.thread.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(thread);
        rx.recv().unwrap_or_else(|_| Err(std::io::Error::other("bridge thread ended")))
    }

    /// `stop()`: ends the server loop and waits for the thread (which waits up to
    /// 30 s for background tasks).
    pub fn stop(&self) {
        self.st().running = false;
        self.stop_signal.notify_one();
        if let Err(e) = self.set_meeting_capture_active(false) {
            self.log(&format!("[KTalk Bridge] Server error: {e}"));
        }
        let thread = self.thread.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_each_close_code_when_named_then_websockets_wording() {
        let cases = [
            (1000, "OK"),
            (1001, "going away"),
            (1002, "protocol error"),
            (1003, "unsupported data"),
            (1007, "invalid frame payload data"),
            (1008, "policy violation"),
            (1009, "message too big"),
            (1011, "internal error"),
            (4000, "unknown"),
        ];
        for (code, name) in cases {
            assert_eq!(close_name(code), name, "{code}");
        }
    }

    #[test]
    fn given_peer_close_frames_when_described_then_only_unclean_ones_are_errors() {
        let frame =
            |code: u16, reason: &str| CloseFrame { code: CloseCode::from(code), reason: reason.to_owned().into() };
        assert_eq!(peer_close_text(None), None);
        assert_eq!(peer_close_text(Some(&frame(1000, ""))), None);
        assert_eq!(peer_close_text(Some(&frame(1001, "bye"))), None);
        assert_eq!(
            peer_close_text(Some(&frame(1011, ""))).as_deref(),
            Some("received 1011 (internal error); then sent 1011 (internal error)")
        );
        assert_eq!(
            peer_close_text(Some(&frame(1008, "bad origin"))).as_deref(),
            Some("received 1008 (policy violation) bad origin; then sent 1008 (policy violation) bad origin")
        );
        assert_eq!(
            peer_close_text(Some(&frame(4000, ""))).as_deref(),
            Some("received 4000 (unknown); then sent 4000 (unknown)")
        );
    }

    #[test]
    fn given_send_errors_when_described_then_closed_socket_reads_as_clean_close() {
        assert_eq!(closed_text(&tungstenite::Error::ConnectionClosed), "sent 1000 (OK); then received 1000 (OK)");
        assert_eq!(closed_text(&tungstenite::Error::AlreadyClosed), "sent 1000 (OK); then received 1000 (OK)");
        let io = tungstenite::Error::Io(std::io::Error::other("broken pipe"));
        assert_eq!(closed_text(&io), "IO error: broken pipe");
    }
}
