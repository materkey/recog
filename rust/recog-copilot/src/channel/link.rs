//! `BridgeLink`: the live WebSocket to the bridge as the tools see it, the requests
//! waiting for the bridge's answer, and the backlog of bridge events for the
//! `await_copilot_event` long-poll.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use recog_core::privacy::{CopilotPrivacy, PrivacyError, PrivacyStore};
use recog_core::pyjson::{self, Map, Value};
use tokio::sync::{Notify, oneshot};

/// How many events wait for the long-poll: about 12 minutes of transcript ticks at the
/// 15 s cadence, more than a polling Claude could miss before catching up.
pub const EVENT_QUEUE_SIZE: usize = 50;

/// What a send returns when it fails: the error text the tool reports.
pub type SendFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// The bridge end of the link: the WebSocket in the service, a fake in tests.
pub trait Socket: Send + Sync {
    /// Sends one text frame.
    fn send(&self, text: String) -> SendFuture;
}

/// Why a request to the bridge got no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionDropped;

impl std::fmt::Display for ConnectionDropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bridge connection dropped")
    }
}

type Answer = Result<Map, ConnectionDropped>;

#[derive(Default)]
struct State {
    socket: Option<Arc<dyn Socket>>,
    epoch: u64,
    pending: HashMap<String, oneshot::Sender<Answer>>,
}

/// The bounded backlog of bridge → Claude events. On overflow the oldest
/// `transcript_tick` goes first — ticks are periodic and superseded by the next one,
/// while a typed question or a lifecycle marker is not — else the oldest event.
pub struct EventQueue {
    items: Mutex<VecDeque<Map>>,
    capacity: usize,
    added: Notify,
}

impl EventQueue {
    pub fn new(capacity: usize) -> Self {
        EventQueue { items: Mutex::new(VecDeque::new()), capacity, added: Notify::new() }
    }

    fn items(&self) -> MutexGuard<'_, VecDeque<Map>> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends `event`, making room first when full; returns what was dropped.
    pub fn push(&self, event: Map) -> Option<Map> {
        let dropped = {
            let mut items = self.items();
            let dropped = if items.len() >= self.capacity {
                let tick = items.iter().position(|e| e.get("type") == Some(&Value::from("transcript_tick")));
                match tick {
                    Some(at) => items.remove(at),
                    None => items.pop_front(),
                }
            } else {
                None
            };
            items.push_back(event);
            dropped
        };
        self.added.notify_one();
        dropped
    }

    /// The oldest event, without waiting.
    pub fn try_get(&self) -> Option<Map> {
        self.items().pop_front()
    }

    /// Waits for the oldest event.
    pub async fn get(&self) -> Map {
        loop {
            let added = self.added.notified();
            tokio::pin!(added);
            added.as_mut().enable();
            if let Some(event) = self.try_get() {
                // Another waiter may be parked on an event this one did not take.
                if !self.items().is_empty() {
                    self.added.notify_one();
                }
                return event;
            }
            added.await;
        }
    }

    pub fn len(&self) -> usize {
        self.items().len()
    }

    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }
}

/// The link the tools share with the WebSocket loop. While the socket is down `send`
/// answers with an error instead of failing, so a tool reports a structured result.
pub struct BridgeLink {
    state: Mutex<State>,
    /// Sends are serialised, as the Python link holds a lock around `ws.send`.
    send_lock: tokio::sync::Mutex<()>,
    pub events: EventQueue,
    pub privacy: Mutex<CopilotPrivacy<PrivacyStore>>,
}

impl BridgeLink {
    pub fn new(privacy: PrivacyStore) -> Self {
        Self::with_queue(privacy, EVENT_QUEUE_SIZE)
    }

    pub fn with_queue(privacy: PrivacyStore, capacity: usize) -> Self {
        BridgeLink {
            state: Mutex::new(State::default()),
            send_lock: tokio::sync::Mutex::new(()),
            events: EventQueue::new(capacity),
            privacy: Mutex::new(CopilotPrivacy::new(privacy)),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `set_socket(ws)`: a new socket starts a new connection epoch; losing the socket
    /// fails every request still waiting, since its answer can never arrive.
    pub fn set_socket(&self, socket: Option<Arc<dyn Socket>>) {
        let mut state = self.state();
        let lost = socket.is_none();
        if socket.is_some() {
            state.epoch += 1;
        }
        state.socket = socket;
        if lost {
            for (_, waiter) in state.pending.drain() {
                let _ = waiter.send(Err(ConnectionDropped));
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        self.state().socket.is_some()
    }

    /// Bumped on every successful (re)connect.
    pub fn connection_epoch(&self) -> u64 {
        self.state().epoch
    }

    /// `send(payload)`: `{"ok": true}`, or `{"ok": false, "error": ...}`.
    pub async fn send(&self, payload: &Map) -> Map {
        let Some(socket) = self.state().socket.clone() else {
            return result(false, Some("bridge not connected"));
        };
        let text = pyjson::dumps(&Value::Object(payload.clone()), None);
        let _guard = self.send_lock.lock().await;
        match socket.send(text).await {
            Ok(()) => result(true, None),
            Err(error) => result(false, Some(&error)),
        }
    }

    /// Registers a request waiting for the bridge's answer.
    pub fn register_pending(&self, request_id: &str) -> oneshot::Receiver<Answer> {
        let (tx, rx) = oneshot::channel();
        self.state().pending.insert(request_id.to_owned(), tx);
        rx
    }

    pub fn unregister_pending(&self, request_id: &str) {
        self.state().pending.remove(request_id);
    }

    /// How many requests wait for an answer.
    pub fn pending_count(&self) -> usize {
        self.state().pending.len()
    }

    /// Hands the bridge's answer to the waiting request; false when none waits.
    pub fn resolve_pending(&self, request_id: &str, answer: Map) -> bool {
        match self.state().pending.remove(request_id) {
            Some(waiter) => waiter.send(Ok(answer)).is_ok(),
            None => false,
        }
    }

    /// The payload as it may leave for the Copilot (`privacy.apply`).
    ///
    /// # Errors
    /// The privacy setting cannot be read.
    pub fn apply_privacy(&self, payload: &Map) -> Result<Map, PrivacyError> {
        self.privacy.lock().unwrap_or_else(PoisonError::into_inner).apply(payload)
    }
}

/// `{"ok": ok}` with an optional `"error"`.
pub fn result(ok: bool, error: Option<&str>) -> Map {
    let mut out = Map::new();
    out.insert("ok", Value::Bool(ok));
    if let Some(error) = error {
        out.insert("error", Value::from(error));
    }
    out
}
