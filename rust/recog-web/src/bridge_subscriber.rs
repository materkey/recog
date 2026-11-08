//! The bidirectional WebSocket link to the bridge — the port of
//! `recog/web/bridge_subscriber.py`.
//!
//! Downstream it forwards the frames whose `type` is in [`FORWARD_TYPES`] verbatim to
//! a handler; everything else is dropped (transcripts reach the web UI through the
//! JSONL tail). Upstream, [`BridgeSubscriber::send`] is best effort: false whenever the
//! link is down, nothing queued across reconnects. The connection announces itself
//! with `subscriber_hello` before it is published for sending, so an upstream frame is
//! never the bridge's first frame. Reconnects back off 1, 2, 4 … 30 s; failures are
//! logged once per streak and then at most once a minute.
//!
//! Like the Python client (`websockets`), frames over 1 MiB end the connection and a
//! ping goes out every 20 s, the connection counting as lost when its pong does not
//! come within 20 s.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::stream::{SplitSink, StreamExt as _};
use futures_util::{SinkExt as _, stream::SplitStream};
use recog_core::pyjson::{self, Value};
use tokio::net::TcpStream;
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::util::BoxFuture;

/// Frame types relayed from the bridge.
pub const FORWARD_TYPES: [&str; 8] = [
    "transcript_refine",
    "copilot_reply",
    "copilot_chat_echo",
    "copilot_status",
    "user_profile_applied",
    "user_profile_error",
    "ai_settings_applied",
    "ai_settings_error",
];

pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
const LOG_THROTTLE: Duration = Duration::from_secs(60);
const PING_INTERVAL: Duration = Duration::from_secs(20);
const MAX_MESSAGE: usize = 1 << 20;

/// Sends a raw frame upstream: true once it is written to the transport.
pub trait RawSender: Send + Sync {
    fn send(&self, raw: String) -> BoxFuture<'_, bool>;
}

/// What the subscriber reports: forwarded frames and the link going up and down.
pub trait BridgeHandler: Send + Sync {
    fn forward(&self, raw: String) -> BoxFuture<'_, ()>;
    /// The link is up; `send` writes to it.
    fn connected(&self, _send: Arc<dyn RawSender>) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn disconnected(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Shared {
    url: String,
    prefix: String,
    sink: AsyncMutex<Option<SplitSink<Socket, Message>>>,
    /// Whether a connection is published; read without the sink lock so that a send
    /// during the hello fails fast instead of waiting behind it.
    published: AtomicBool,
}

impl Shared {
    async fn send(&self, raw: String) -> bool {
        if !self.published.load(Ordering::SeqCst) {
            return false;
        }
        let mut sink = self.sink.lock().await;
        let Some(ws) = sink.as_mut() else { return false };
        match ws.send(Message::text(raw)).await {
            Ok(()) => true,
            Err(e) => {
                eprintln!("{} send failed: {e}", self.prefix);
                false
            }
        }
    }
}

impl RawSender for Shared {
    fn send(&self, raw: String) -> BoxFuture<'_, bool> {
        Box::pin(Shared::send(self, raw))
    }
}

/// `BridgeSubscriber(bridge_url, forward_callback, connected_callback,
/// disconnected_callback)`.
pub struct BridgeSubscriber {
    shared: Arc<Shared>,
    handler: Arc<dyn BridgeHandler>,
    initial_backoff: Duration,
    task: Mutex<Option<(watch::Sender<bool>, JoinHandle<()>)>>,
}

impl BridgeSubscriber {
    #[must_use]
    pub fn new(url: &str, handler: Arc<dyn BridgeHandler>) -> Self {
        Self::with_backoff(url, handler, INITIAL_BACKOFF)
    }

    /// With a shorter first backoff, for tests (`sub.INITIAL_BACKOFF_S = 0.05`).
    #[must_use]
    pub fn with_backoff(url: &str, handler: Arc<dyn BridgeHandler>, initial_backoff: Duration) -> Self {
        Self {
            shared: Arc::new(Shared {
                url: url.to_owned(),
                prefix: "[bridge-subscriber]".to_owned(),
                sink: AsyncMutex::new(None),
                published: AtomicBool::new(false),
            }),
            handler,
            initial_backoff,
            task: Mutex::new(None),
        }
    }

    /// Whether a connection is published for sending (`sub._ws is not None`).
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.shared.published.load(Ordering::SeqCst)
    }

    /// Whether the run loop is alive (`not sub._task.done()`).
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|(_, t)| !t.is_finished())
    }

    /// `send(raw)`: best effort, never queued.
    pub async fn send(&self, raw: &str) -> bool {
        self.shared.send(raw.to_owned()).await
    }

    /// A handle that sends through this subscriber.
    #[must_use]
    pub fn sender(&self) -> Arc<dyn RawSender> {
        Arc::clone(&self.shared) as Arc<dyn RawSender>
    }

    /// `start()`: the run loop on the current runtime; a second call does nothing.
    pub fn start(&self) {
        let mut task = self.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if task.is_some() {
            return;
        }
        let (stop_tx, stop_rx) = watch::channel(false);
        let shared = Arc::clone(&self.shared);
        let handler = Arc::clone(&self.handler);
        let initial = self.initial_backoff;
        *task = Some((stop_tx, tokio::spawn(run(shared, handler, initial, stop_rx))));
    }

    /// `stop()`: the loop is told to stop (its disconnect callback still runs) and
    /// waited for at most 5 s.
    pub async fn stop(&self) {
        let task = self.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let Some((stop, mut handle)) = task else { return };
        let _ = stop.send(true);
        if tokio::time::timeout(Duration::from_secs(5), &mut handle).await.is_err() {
            eprintln!("{} stop timed out; detaching task", self.shared.prefix);
            handle.abort();
        }
    }
}

struct Failures {
    streak: u32,
    last_log: Option<Instant>,
}

impl Failures {
    fn failed(&mut self, prefix: &str, error: &str, delay: Duration) {
        self.streak += 1;
        let now = Instant::now();
        let throttle_over = self.last_log.is_none_or(|last| now.duration_since(last) >= LOG_THROTTLE);
        if self.streak == 1 || throttle_over {
            eprintln!("{prefix} bridge unavailable ({error}); retry in {:.1}s", delay.as_secs_f64());
            self.last_log = Some(now);
        }
    }
}

/// `_handle_message`: a frame of a forwarded type, verbatim; anything else dropped.
fn forwarded(message: Message) -> Option<String> {
    let raw = match message {
        Message::Text(text) => text.as_str().to_owned(),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok()?,
        _ => return None,
    };
    let Ok(Value::Object(map)) = pyjson::loads(&raw) else { return None };
    let kind = map.get("type")?.as_str()?;
    FORWARD_TYPES.contains(&kind).then_some(raw)
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await.map(|_| ());
}

fn is_stopped(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow()
}

enum Ended {
    Stopped,
    /// The connection closed or failed; the error, if any, is logged.
    Lost(Option<String>),
}

async fn receive(
    shared: &Shared,
    handler: &dyn BridgeHandler,
    mut stream: SplitStream<Socket>,
    stop: &mut watch::Receiver<bool>,
) -> Ended {
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    let mut awaiting_pong: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            _ = stopped(stop) => return Ended::Stopped,
            _ = ping.tick() => {
                if let Some(sent) = awaiting_pong
                    && sent.elapsed() >= PING_INTERVAL
                {
                    return Ended::Lost(Some("keepalive ping timeout".into()));
                }
                let mut sink = shared.sink.lock().await;
                if let Some(ws) = sink.as_mut()
                    && ws.send(Message::Ping(Vec::new().into())).await.is_ok()
                    && awaiting_pong.is_none()
                {
                    awaiting_pong = Some(tokio::time::Instant::now());
                }
            }
            message = stream.next() => match message {
                None => return Ended::Lost(None),
                Some(Err(e)) => return Ended::Lost(Some(e.to_string())),
                Some(Ok(Message::Close(_))) => return Ended::Lost(None),
                Some(Ok(Message::Pong(_))) => awaiting_pong = None,
                Some(Ok(message)) => {
                    if let Some(raw) = forwarded(message) {
                        handler.forward(raw).await;
                    }
                }
            },
        }
    }
}

async fn run(shared: Arc<Shared>, handler: Arc<dyn BridgeHandler>, initial: Duration, mut stop: watch::Receiver<bool>) {
    let mut delay = initial;
    let mut connected_once = false;
    let mut failures = Failures { streak: 0, last_log: None };
    let config = WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE));
    loop {
        if is_stopped(&stop) {
            return;
        }
        let connect = tokio_tungstenite::connect_async_with_config(shared.url.as_str(), Some(config), false);
        let outcome = tokio::select! {
            _ = stopped(&mut stop) => return,
            outcome = connect => outcome,
        };
        let error = match outcome {
            Err(e) => Some(e.to_string()),
            Ok((socket, _)) => {
                let (mut sink, stream) = socket.split();
                let hello = sink.send(Message::text(r#"{"type": "subscriber_hello"}"#)).await;
                if let Err(e) = hello {
                    eprintln!("{} subscriber_hello send failed: {e}", shared.prefix);
                }
                *shared.sink.lock().await = Some(sink);
                shared.published.store(true, Ordering::SeqCst);
                failures.streak = 0;
                if connected_once {
                    eprintln!("{} reconnected to {}", shared.prefix, shared.url);
                } else {
                    connected_once = true;
                    eprintln!("{} connected to {}", shared.prefix, shared.url);
                }
                handler.connected(Arc::clone(&shared) as Arc<dyn RawSender>).await;
                delay = initial;
                let ended = receive(&shared, handler.as_ref(), stream, &mut stop).await;
                shared.published.store(false, Ordering::SeqCst);
                if let Some(mut sink) = shared.sink.lock().await.take() {
                    let _ = sink.close().await;
                }
                handler.disconnected().await;
                match ended {
                    Ended::Stopped => return,
                    Ended::Lost(error) => error,
                }
            }
        };
        if let Some(error) = error {
            failures.failed(&shared.prefix, &error, delay);
        }
        if is_stopped(&stop) {
            return;
        }
        if tokio::time::timeout(delay, stopped(&mut stop)).await.is_ok() {
            return;
        }
        delay = (delay * 2).min(MAX_BACKOFF);
    }
}
