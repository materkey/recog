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
use recog_core::protocol;
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
    protocol::TRANSCRIPT_REFINE,
    protocol::COPILOT_REPLY,
    protocol::COPILOT_CHAT_ECHO,
    protocol::COPILOT_STATUS,
    protocol::USER_PROFILE_APPLIED,
    protocol::USER_PROFILE_ERROR,
    protocol::AI_SETTINGS_APPLIED,
    protocol::AI_SETTINGS_ERROR,
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
        let ended = tokio::select! {
            _ = stopped(stop) => Some(Ended::Stopped),
            _ = ping.tick() => keepalive_tick(shared, &mut awaiting_pong).await,
            message = stream.next() => on_message(handler, message, &mut awaiting_pong).await,
        };
        if let Some(ended) = ended {
            return ended;
        }
    }
}

/// A keepalive tick: the connection is lost when the last ping's pong is overdue;
/// otherwise a ping goes out, and the first one sent starts the pong wait.
async fn keepalive_tick(shared: &Shared, awaiting_pong: &mut Option<tokio::time::Instant>) -> Option<Ended> {
    if let Some(sent) = *awaiting_pong
        && sent.elapsed() >= PING_INTERVAL
    {
        return Some(Ended::Lost(Some("keepalive ping timeout".into())));
    }
    let mut sink = shared.sink.lock().await;
    if let Some(ws) = sink.as_mut()
        && ws.send(Message::Ping(Vec::new().into())).await.is_ok()
        && awaiting_pong.is_none()
    {
        *awaiting_pong = Some(tokio::time::Instant::now());
    }
    None
}

/// A read from the bridge: the end of the connection, or None after forwarding a
/// frame of a forwarded type (a pong ends the pong wait).
async fn on_message(
    handler: &dyn BridgeHandler,
    message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    awaiting_pong: &mut Option<tokio::time::Instant>,
) -> Option<Ended> {
    match message {
        None | Some(Ok(Message::Close(_))) => return Some(Ended::Lost(None)),
        Some(Err(e)) => return Some(Ended::Lost(Some(e.to_string()))),
        Some(Ok(Message::Pong(_))) => *awaiting_pong = None,
        Some(Ok(message)) => {
            if let Some(raw) = forwarded(message) {
                handler.forward(raw).await;
            }
        }
    }
    None
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
                failures.streak = 0;
                delay = initial;
                let ended = session(&shared, handler.as_ref(), socket, connected_once, &mut stop).await;
                connected_once = true;
                let Ended::Lost(error) = ended else { return };
                error
            }
        };
        if let Some(error) = error {
            failures.failed(&shared.prefix, &error, delay);
        }
        if !backoff(&mut stop, delay).await {
            return;
        }
        delay = (delay * 2).min(MAX_BACKOFF);
    }
}

/// One connection: the hello, the link published to the handler until it is lost
/// or the subscriber stops, then withdrawn.
async fn session(
    shared: &Arc<Shared>,
    handler: &dyn BridgeHandler,
    socket: Socket,
    reconnect: bool,
    stop: &mut watch::Receiver<bool>,
) -> Ended {
    let (mut sink, stream) = socket.split();
    let hello = sink.send(Message::text(format!(r#"{{"type": "{}"}}"#, protocol::SUBSCRIBER_HELLO))).await;
    if let Err(e) = hello {
        eprintln!("{} subscriber_hello send failed: {e}", shared.prefix);
    }
    *shared.sink.lock().await = Some(sink);
    shared.published.store(true, Ordering::SeqCst);
    let verb = if reconnect { "reconnected" } else { "connected" };
    eprintln!("{} {verb} to {}", shared.prefix, shared.url);
    handler.connected(Arc::clone(shared) as Arc<dyn RawSender>).await;
    let ended = receive(shared, handler, stream, stop).await;
    shared.published.store(false, Ordering::SeqCst);
    if let Some(mut sink) = shared.sink.lock().await.take() {
        let _ = sink.close().await;
    }
    handler.disconnected().await;
    ended
}

/// Waits out the backoff; false when the subscriber stops before or during it.
async fn backoff(stop: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    !is_stopped(stop) && tokio::time::timeout(delay, stopped(stop)).await.is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl BridgeHandler for Recorder {
        fn forward(&self, raw: String) -> BoxFuture<'_, ()> {
            self.0.lock().unwrap().push(raw);
            Box::pin(async {})
        }
    }

    fn unlinked() -> Shared {
        Shared {
            url: "ws://127.0.0.1:1".into(),
            prefix: "[test]".into(),
            sink: AsyncMutex::new(None),
            published: AtomicBool::new(false),
        }
    }

    #[tokio::test]
    async fn a_pong_overdue_when_the_keepalive_ticks_then_the_connection_is_lost_to_the_timeout() {
        // given
        let mut awaiting_pong = tokio::time::Instant::now().checked_sub(PING_INTERVAL);
        // when
        let ended = keepalive_tick(&unlinked(), &mut awaiting_pong).await;
        // then
        assert!(matches!(ended, Some(Ended::Lost(Some(e))) if e == "keepalive ping timeout"));
    }

    #[tokio::test]
    async fn no_socket_when_the_keepalive_ticks_then_no_pong_is_awaited_and_the_connection_stays() {
        // given
        let mut awaiting_pong = None;
        // when
        let ended = keepalive_tick(&unlinked(), &mut awaiting_pong).await;
        // then
        assert!(ended.is_none());
        assert!(awaiting_pong.is_none());
    }

    #[tokio::test]
    async fn a_pong_when_read_then_the_pong_wait_ends_and_reading_goes_on() {
        // given
        let handler = Recorder::default();
        let mut awaiting_pong = Some(tokio::time::Instant::now());
        // when
        let ended = on_message(&handler, Some(Ok(Message::Pong(Vec::new().into()))), &mut awaiting_pong).await;
        // then
        assert!(ended.is_none());
        assert!(awaiting_pong.is_none());
        assert!(handler.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_end_of_the_stream_a_close_or_an_error_when_read_then_the_connection_is_lost() {
        // given
        let handler = Recorder::default();
        let mut awaiting_pong = None;
        let error = tokio_tungstenite::tungstenite::Error::ConnectionClosed;
        let text = error.to_string();
        // when / then
        assert!(matches!(on_message(&handler, None, &mut awaiting_pong).await, Some(Ended::Lost(None))));
        let close = Some(Ok(Message::Close(None)));
        assert!(matches!(on_message(&handler, close, &mut awaiting_pong).await, Some(Ended::Lost(None))));
        let failed = on_message(&handler, Some(Err(error)), &mut awaiting_pong).await;
        assert!(matches!(failed, Some(Ended::Lost(Some(e))) if e == text));
    }

    #[tokio::test]
    async fn frames_when_read_then_only_a_forwarded_type_reaches_the_handler() {
        // given
        let handler = Recorder::default();
        let mut awaiting_pong = None;
        let refine = r#"{"type": "transcript_refine", "updates": []}"#;
        // when
        let first = on_message(&handler, Some(Ok(Message::text(refine))), &mut awaiting_pong).await;
        let second =
            on_message(&handler, Some(Ok(Message::text(r#"{"type": "transcript"}"#))), &mut awaiting_pong).await;
        // then
        assert!(first.is_none() && second.is_none());
        assert_eq!(*handler.0.lock().unwrap(), [refine.to_owned()]);
    }
}
