//! Small pieces the Python server takes from asyncio and the standard library.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use recog_core::time::{self, Clock as _, PyDateTime, SystemClock};
use tokio::sync::{mpsc, watch};

/// A boxed future, so callbacks can be trait objects.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// `asyncio.Event`: set, clear, and wait until set.
#[derive(Clone)]
pub struct Event {
    tx: Arc<watch::Sender<bool>>,
}

impl Default for Event {
    fn default() -> Self {
        Self::new()
    }
}

impl Event {
    #[must_use]
    pub fn new() -> Self {
        Self { tx: Arc::new(watch::channel(false).0) }
    }

    pub fn set(&self) {
        self.tx.send_replace(true);
    }

    pub fn clear(&self) {
        self.tx.send_replace(false);
    }

    #[must_use]
    pub fn is_set(&self) -> bool {
        *self.tx.borrow()
    }

    pub async fn wait(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|set| *set).await;
    }

    /// `asyncio.wait_for(event.wait(), timeout)`, the timeout swallowed.
    pub async fn wait_timeout(&self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.wait()).await;
    }
}

/// One connected WebSocket client: the frames queued for its writer task. A send
/// fails once the connection is gone, as `websocket.send_text` raises.
#[derive(Clone, Debug)]
pub struct Client {
    id: u64,
    tx: mpsc::UnboundedSender<String>,
}

impl PartialEq for Client {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Client {
    /// A client and the receiving end its writer drains.
    #[must_use]
    pub fn new() -> (Client, mpsc::UnboundedReceiver<String>) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let (tx, rx) = mpsc::unbounded_channel();
        (Client { id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), tx }, rx)
    }

    /// `send_text`: whether the frame was queued to a live connection.
    pub fn send_text(&self, message: &str) -> bool {
        self.tx.send(message.to_owned()).is_ok()
    }
}

/// `log_rate_limited(key, msg)` of `recog/log_utils.py`: the first message per key at
/// once, repeats within 30 s counted and reported with the next one.
pub fn log_rate_limited(key: &str, msg: &str) {
    static STATE: OnceLock<Mutex<HashMap<String, (Instant, u64)>>> = OnceLock::new();
    let interval = Duration::from_secs(30);
    let now = Instant::now();
    let suppressed = {
        let mut state = STATE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.get_mut(key) {
            Some((last, count)) if now.duration_since(*last) < interval => {
                *count += 1;
                return;
            }
            Some(entry) => {
                let suppressed = entry.1;
                *entry = (now, 0);
                suppressed
            }
            None => {
                state.insert(key.to_owned(), (now, 0));
                0
            }
        }
    };
    if suppressed > 0 {
        eprintln!("{msg} (+{suppressed} suppressed)");
    } else {
        eprintln!("{msg}");
    }
}

/// `datetime.now().isoformat()`: local wall time, naive.
#[must_use]
pub fn local_now_isoformat() -> String {
    let now = time::now_utc(&SystemClock);
    PyDateTime { wall_micros: now.local_wall(&SystemClock), offset_micros: None }.isoformat()
}

/// `datetime.now(timezone.utc).isoformat()`.
#[must_use]
pub fn utc_now_isoformat() -> String {
    PyDateTime::utc(SystemClock.now_micros()).isoformat()
}

/// `time.time()`.
#[must_use]
pub fn unix_time() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// `round(x, digits)`: Python rounds the exact binary value half to even, which
/// formatting with that many decimals and reading back reproduces.
#[must_use]
pub fn py_round(x: f64, digits: usize) -> f64 {
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// Whether a meeting or prompt ID would leave its directory.
#[must_use]
pub fn invalid_id(id: &str) -> bool {
    id.contains("..") || id.contains('/') || id.contains('\\')
}
