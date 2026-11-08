//! The per-meeting watcher behind `/api/meetings/{id}/watch` — the port of
//! `recog/web/transcript_watcher.py`: one tail per transcript file, dropped with the
//! meeting's last client.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

use crate::tail::{self, TailState};
use crate::util::Client;

#[derive(Default)]
struct Inner {
    clients: HashMap<String, Vec<Client>>,
    /// Keyed by the transcript path, as Python keys `file_positions`.
    tails: HashMap<String, TailState>,
}

#[derive(Default)]
pub struct TranscriptWatcher {
    inner: Mutex<Inner>,
    reading: AsyncMutex<()>,
}

impl TranscriptWatcher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn register(&self, meeting_id: &str, client: Client) {
        let mut inner = self.lock();
        let clients = inner.clients.entry(meeting_id.to_owned()).or_default();
        if !clients.contains(&client) {
            clients.push(client);
        }
    }

    /// The last client of a meeting takes the meeting's tail state with it.
    pub fn unregister(&self, meeting_id: &str, client: &Client) {
        let mut inner = self.lock();
        let Some(clients) = inner.clients.get_mut(meeting_id) else { return };
        clients.retain(|c| c != client);
        if clients.is_empty() {
            inner.clients.remove(meeting_id);
            let suffix = format!("{meeting_id}/transcript.jsonl");
            inner.tails.retain(|key, _| !key.ends_with(&suffix));
        }
    }

    /// The tail of one transcript file, for tests.
    #[must_use]
    pub fn tail(&self, path: &Path) -> Option<TailState> {
        self.lock().tails.get(&path.to_string_lossy().into_owned()).cloned()
    }

    /// `on_modified`.
    pub fn on_modified(self: &Arc<Self>, path: &Path, is_directory: bool) {
        if is_directory || path.file_name().and_then(|n| n.to_str()) != Some("transcript.jsonl") {
            return;
        }
        let meeting_id =
            path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !self.lock().clients.contains_key(&meeting_id) {
            return;
        }
        let this = Arc::clone(self);
        let path = path.to_path_buf();
        tokio::spawn(async move { this.notify_clients(&meeting_id, &path).await });
    }

    /// `_notify_clients`: new rows of the meeting's transcript to its clients.
    pub async fn notify_clients(&self, meeting_id: &str, transcript_path: &Path) {
        if !transcript_path.exists() {
            return;
        }
        let key = transcript_path.to_string_lossy().into_owned();
        let segments = {
            let _reading = self.reading.lock().await;
            let mut tail = self.lock().tails.get(&key).cloned().unwrap_or_default();
            let result = tail::read_new_segments(transcript_path, &mut tail);
            self.lock().tails.insert(key, tail);
            match result {
                Ok(segments) => segments,
                Err(e) => {
                    println!("Error reading transcript: {e}");
                    return;
                }
            }
        };
        if segments.is_empty() {
            return;
        }
        let message = tail::segments_frame(segments);
        let clients = self.lock().clients.get(meeting_id).cloned().unwrap_or_default();
        let dead: Vec<Client> = clients.into_iter().filter(|c| !c.send_text(&message)).collect();
        if !dead.is_empty()
            && let Some(clients) = self.lock().clients.get_mut(meeting_id)
        {
            clients.retain(|c| !dead.contains(c));
        }
    }
}
