//! The live-stream side of the file watcher — the port of `recog/web/live_watcher.py`.
//!
//! It follows the current meeting: `index.json` changes announce a new one
//! (`meeting_change`), a `meta.json` change of the current one resends the refine flag
//! when its source flipped in place (`refine_state`), and `transcript.jsonl` appends
//! go to every live-stream client as `segments`, deduplicated across atomic rewrites
//! by the seen-ID watermark ([`crate::tail`]).

use std::path::Path;
use std::sync::{Arc, Mutex};

use recog_core::meeting_storage::{MeetingStorage, StorageError};
use recog_core::pyjson::{self, Value};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};

use crate::json;
use crate::tail::{self, TailState};
use crate::util::Client;

/// `get_storage()`.
pub type StorageFactory = Arc<dyn Fn() -> Result<MeetingStorage, StorageError> + Send + Sync>;

/// The current meeting and the tail of its transcript.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveState {
    pub current_meeting_id: Option<String>,
    /// The source last announced for the current meeting.
    pub current_meeting_source: Option<Value>,
    pub tail: TailState,
}

pub struct LiveWatcher {
    clients: Mutex<Vec<Client>>,
    state: Mutex<LiveState>,
    /// Serializes `_notify_new_segments` the way the single asyncio thread does.
    reading: AsyncMutex<()>,
    meeting_read_lock: AsyncMutex<()>,
    storage: StorageFactory,
}

fn source_is_ktalk(source: Option<&Value>) -> bool {
    source.and_then(Value::as_str) == Some("ktalk")
}

impl LiveWatcher {
    #[must_use]
    pub fn new(storage: StorageFactory) -> Self {
        Self {
            clients: Mutex::new(Vec::new()),
            state: Mutex::new(LiveState::default()),
            reading: AsyncMutex::new(()),
            meeting_read_lock: AsyncMutex::new(()),
            storage,
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, LiveState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_clients(&self) -> std::sync::MutexGuard<'_, Vec<Client>> {
        self.clients.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A copy of the state, for tests and the delete route.
    #[must_use]
    pub fn state(&self) -> LiveState {
        self.lock_state().clone()
    }

    #[must_use]
    pub fn current_meeting_id(&self) -> Option<String> {
        self.lock_state().current_meeting_id.clone()
    }

    pub fn register(&self, client: Client) {
        let mut clients = self.lock_clients();
        if !clients.contains(&client) {
            clients.push(client);
        }
    }

    pub fn unregister(&self, client: &Client) {
        self.lock_clients().retain(|c| c != client);
    }

    #[must_use]
    pub fn client_count(&self) -> usize {
        self.lock_clients().len()
    }

    /// `set_current_meeting`: a different ID resets the tail and the watermark and
    /// takes the given source; the same ID keeps everything.
    pub fn set_current_meeting(&self, meeting_id: Option<&str>, source: Option<Value>) {
        let mut state = self.lock_state();
        if state.current_meeting_id.as_deref() != meeting_id {
            state.current_meeting_id = meeting_id.map(str::to_owned);
            state.tail = TailState::default();
            state.current_meeting_source = source;
        }
    }

    /// `seed_post_init_watermark`: the integer IDs already delivered in `init`.
    pub fn seed_post_init_watermark<'a>(&self, known: impl IntoIterator<Item = &'a Value>) {
        let mut state = self.lock_state();
        for id in known {
            match id {
                Value::Int(n) => {
                    state.tail.seen.insert(*n);
                }
                Value::Bool(b) => {
                    state.tail.seen.insert(i128::from(*b));
                }
                _ => {}
            }
        }
    }

    /// `read_active_meetings`: the active meetings read off the runtime, with the read
    /// lock still held so that the caller applies its selection before a newer read
    /// can.
    ///
    /// # Errors
    /// What the storage raises.
    pub async fn read_active_meetings(
        &self,
        storage: MeetingStorage,
    ) -> (MutexGuard<'_, ()>, Result<Vec<Value>, StorageError>) {
        let guard = self.meeting_read_lock.lock().await;
        let rows = tokio::task::spawn_blocking(move || storage.list_active_meetings())
            .await
            .unwrap_or_else(|e| Err(StorageError::Os(e.to_string())));
        (guard, rows)
    }

    /// `on_modified` of one path: what changed decides which check runs.
    pub fn on_modified(self: &Arc<Self>, path: &Path, is_directory: bool) {
        if is_directory {
            return;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let parent = path.parent().and_then(Path::file_name).and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        match name {
            "index.json" => {
                let this = Arc::clone(self);
                tokio::spawn(async move { this.check_current_meeting().await });
            }
            "meta.json" if self.current_meeting_id().as_deref() == Some(parent.as_str()) => {
                let this = Arc::clone(self);
                tokio::spawn(async move { this.check_source_change(&parent).await });
            }
            "transcript.jsonl" if self.current_meeting_id().as_deref() == Some(parent.as_str()) => {
                let this = Arc::clone(self);
                let path = path.to_path_buf();
                tokio::spawn(async move { this.notify_new_segments(&path).await });
            }
            _ => {}
        }
    }

    /// `_check_current_meeting`: announce a new current meeting.
    pub async fn check_current_meeting(&self) {
        let storage = match (self.storage)() {
            Ok(storage) => storage,
            Err(e) => {
                println!("Error checking current meeting: {e}");
                return;
            }
        };
        let (guard, rows) = self.read_active_meetings(storage).await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(e) => {
                println!("Error checking current meeting: {e}");
                return;
            }
        };
        let Some(current) = rows.into_iter().next() else { return };
        let id = current.as_object().and_then(|m| m.get("id")).map(Value::py_str).unwrap_or_default();
        if self.current_meeting_id().as_deref() == Some(id.as_str()) {
            return;
        }
        let source = current.as_object().and_then(|m| m.get("source")).filter(|v| !v.is_null()).cloned();
        let refine = source_is_ktalk(source.as_ref());
        self.set_current_meeting(Some(&id), source);
        drop(guard);
        let message = json::object([
            ("type", Value::from("meeting_change")),
            ("meeting", current),
            ("refine_enabled", Value::Bool(refine)),
        ]);
        self.broadcast(&json::ascii(&message));
    }

    /// `_check_source_change`: resend the refine flag after an in-place source flip.
    pub async fn check_source_change(&self, meeting_id: &str) {
        if self.current_meeting_id().as_deref() != Some(meeting_id) {
            return;
        }
        let meta_path = match (self.storage)() {
            Ok(storage) => storage.base_path().join(meeting_id).join("meta.json"),
            Err(e) => {
                println!("Error checking source change: {e}");
                return;
            }
        };
        if !meta_path.exists() {
            return;
        }
        let source = match read_source(&meta_path) {
            Ok(source) => source,
            Err(e) => {
                println!("Error checking source change: {e}");
                return;
            }
        };
        {
            let mut state = self.lock_state();
            if state.current_meeting_source == source {
                return;
            }
            state.current_meeting_source.clone_from(&source);
        }
        let message = json::object([
            ("type", Value::from("refine_state")),
            ("meeting_id", Value::from(meeting_id)),
            ("refine_enabled", Value::Bool(source_is_ktalk(source.as_ref()))),
        ]);
        self.broadcast(&json::ascii(&message));
    }

    /// `_notify_new_segments`: new rows of the current transcript to every client.
    pub async fn notify_new_segments(&self, transcript_path: &Path) {
        if !transcript_path.exists() {
            return;
        }
        let segments = {
            let _reading = self.reading.lock().await;
            let mut tail = self.lock_state().tail.clone();
            let result = tail::read_new_segments(transcript_path, &mut tail);
            self.lock_state().tail = tail;
            match result {
                Ok(segments) => segments,
                Err(e) => {
                    println!("Error reading transcript: {e}");
                    return;
                }
            }
        };
        if !segments.is_empty() {
            self.broadcast(&tail::segments_frame(segments));
        }
    }

    /// `_broadcast`: to every client, dropping those whose connection is gone.
    pub fn broadcast(&self, message: &str) {
        let clients: Vec<Client> = self.lock_clients().clone();
        let dead: Vec<Client> = clients.into_iter().filter(|c| !c.send_text(message)).collect();
        if !dead.is_empty() {
            self.lock_clients().retain(|c| !dead.contains(c));
        }
    }
}

/// `json.loads(meta_path.read_text()).get("source")`.
fn read_source(meta_path: &Path) -> Result<Option<Value>, String> {
    let raw = std::fs::read(meta_path).map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&raw).map_err(|e| pyjson::utf8_error_message(&raw, &e))?;
    let meta = pyjson::loads(&text.replace("\r\n", "\n").replace('\r', "\n")).map_err(|e| e.0)?;
    match meta {
        Value::Object(map) => Ok(map.get("source").filter(|v| !v.is_null()).cloned()),
        other => Err(format!("'{}' object has no attribute 'get'", other.type_name())),
    }
}
