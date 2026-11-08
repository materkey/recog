//! The meeting lifecycle: `meeting_join` (one owner at a time, a second conference
//! refused), `meeting_leave`, the end of a meeting whose extension is gone, and the
//! transcript history a joining tab gets.
//!
//! Known differences from Python: a `url`, `title`, `platform` or `reason` that is
//! not a string reaches the storage as its `str()` (Python passes the value, and a
//! non-string url raises inside `urlparse`; here it is refused with an error).

use std::sync::Arc;

use recog_core::pyjson::{Map, Value};

use crate::bridge::Bridge;
use crate::client::Client;
use crate::copilot::TickState;
use crate::json::{self, dumps, dumps_ascii};
use crate::obj;

fn text_of(value: &Value) -> String {
    json::py_str(value)
}

impl Bridge {
    /// Marks pending Copilot requests of `meeting_id` stale; returns how many.
    fn mark_stale(&self, meeting_id: Option<&str>, skip_stale: bool) -> usize {
        let Some(meeting_id) = meeting_id else { return 0 };
        let mut st = self.st();
        let mut count = 0;
        for (_, mid) in &mut st.pending {
            if mid == meeting_id && !(skip_stale && mid.starts_with("stale:")) {
                *mid = format!("stale:{mid}");
                count += 1;
            }
        }
        count
    }

    fn clear_transcript_buffer(&self) {
        self.st().transcript.clear();
    }

    fn meeting_ended_frame(&self, meeting_id: Option<&str>) -> Value {
        obj! {"type" => "meeting_ended", "meeting_id" => meeting_id, "created_at" => Value::Float(self.now() * 1000.0)}
    }

    pub(crate) async fn on_meeting_join(
        self: &Arc<Self>,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        let url = data.get("url").cloned().unwrap_or(Value::Null);
        let title = data.get("title").cloned().unwrap_or(Value::Null);
        let platform = data.get("platform").map_or_else(|| "ktalk".to_owned(), text_of);
        if self.config.ktalk_only && platform != "ktalk" {
            if let Some(client) = client {
                let frame = obj! {"type" => "meeting_join_rejected", "reason" => "ktalk_only"};
                client.sink.send(dumps_ascii(&frame)).await.map_err(|e| e.0)?;
            }
            return Ok(());
        }
        let Some(storage) = self.storage() else { return Ok(()) };
        if !url.truthy() {
            return Ok(());
        }
        let owner_blocks = {
            let st = self.st();
            match (st.meeting_owner, client) {
                (Some(owner), c) => c.is_none_or(|c| c.id != owner) && st.clients.connected.contains(&owner),
                (None, _) => false,
            }
        };
        if owner_blocks && storage.current_meeting_path().is_some() {
            let owner_addr = {
                let st = self.st();
                st.meeting_owner.and_then(|o| st.clients.all.get(&o).map(|c| c.remote_address.clone()))
            };
            self.log(&format!(
                "[KTalk Bridge] Rejecting meeting_join from second tab (url={}); owner={} still active",
                text_of(&url),
                owner_addr.as_deref().unwrap_or("unknown")
            ));
            if let Some(client) = client {
                let frame = obj! {
                    "type" => "meeting_join_rejected",
                    "reason" => "another_tab_active",
                    "active_meeting_id" => storage.current_meeting_id(),
                };
                let _ = client.sink.send(dumps(&frame)).await;
            }
            return Ok(());
        }
        let Value::Str(url) = &url else {
            return Err(format!("'{}' object has no attribute 'decode'", url.type_name()));
        };
        let was_recording = self.is_recording_active();
        let old_path = storage.current_meeting_path();
        let prev_meeting_id = storage.current_meeting_id();
        let prev_source = if old_path.is_some() {
            storage.load_meta().ok().and_then(|m| match m {
                Value::Object(map) => map.get("source").cloned(),
                Value::Null => None,
                _ => None,
            })
        } else {
            None
        };
        self.clear_speaker_state();
        let live = self.wired().live;
        if let Some(live) = &live {
            live.before_meeting_switch();
        }
        let title_arg = match &title {
            Value::Null => None,
            other => Some(text_of(other)),
        };
        let meeting_id = storage.on_meeting_join(url, title_arg.as_deref(), &platform)?;
        self.log(&format!("[KTalk Bridge] Joined {platform} meeting: {url}"));
        let new_path = storage.current_meeting_path();
        let path_changed = new_path != old_path;
        let became_active =
            !path_changed && (prev_source == Some(Value::from("orphan")) || (self.config.ktalk_only && !was_recording));
        if self.config.ktalk_only {
            let started = (|| {
                if let Some(live) = &live {
                    live.meeting_changed(meeting_id.as_deref(), new_path.as_deref())?;
                }
                self.set_meeting_capture_active(true)
            })();
            if let Err(e) = started {
                self.st().meeting_owner = None;
                if let Err(cleanup) = self.set_meeting_capture_active(false) {
                    self.log(&format!("[KTalk Bridge] Capture cleanup failed: {cleanup}"));
                }
                self.log(&format!("[KTalk Bridge] Capture start failed: {e}"));
                if let Some(client) = client {
                    let frame = obj! {"type" => "meeting_join_rejected", "reason" => "capture_unavailable"};
                    self.send_bounded(client, &dumps_ascii(&frame)).await.map_err(|e| e.0)?;
                }
                return Ok(());
            }
            self.st().meeting_owner = client.map(|c| c.id);
        }
        if path_changed || became_active {
            if path_changed
                && !self.config.ktalk_only
                && let Some(live) = &live
            {
                live.meeting_changed(meeting_id.as_deref(), new_path.as_deref())?;
            }
            // A direct switch to another meeting strands the old meeting's requests:
            // their late replies must not reach the new meeting's Copilot tab.
            if prev_meeting_id.as_deref().is_some_and(|p| !p.is_empty()) && prev_meeting_id != meeting_id {
                let stale = self.mark_stale(prev_meeting_id.as_deref(), false);
                if stale > 0 {
                    self.log(&format!(
                        "[KTalk Bridge] Marked {stale} copilot request(s) stale on meeting_join transition"
                    ));
                }
            }
            // Segment ids restart at 1 per meeting.
            self.clear_transcript_buffer();
            let started = obj! {
                "type" => "meeting_started",
                "meeting_id" => meeting_id.as_deref(),
                "meeting_title" => title.clone(),
                "created_at" => Value::Float(self.now() * 1000.0),
            };
            // The manual mirror and its requests go before the first send can let a
            // late reply in; the webapp hears of it after the lifecycle.
            let preempted = self.preempt_manual_copilot(false).await;
            self.emit_lifecycle(started).await;
            if preempted {
                self.notify_manual_copilot_preempted().await;
            }
            self.st().tick = Some(TickState::new(meeting_id.clone(), title, self.now()));
        } else {
            // A reconnect keeps the tick counters, but re-arms them when a spurious
            // leave cleared them or they point at another meeting.
            let now = self.now();
            let mut st = self.st();
            if st.tick.as_ref().is_none_or(|t| t.meeting_id != meeting_id) {
                st.tick = Some(TickState::new(meeting_id.clone(), title, now));
            }
        }
        if let Some(client) = client {
            self.st().meeting_owner = Some(client.id);
        }
        // History to the joining tab only: a broadcast would move a sibling tab's
        // `currentMeetingId` into this meeting.
        self.send_transcript_history(client).await
    }

    pub(crate) async fn on_meeting_leave(
        self: &Arc<Self>,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        let is_owner = client.map(|c| c.id) == self.st().meeting_owner;
        if self.config.ktalk_only && !is_owner {
            return Ok(());
        }
        self.set_meeting_capture_active(false)?;
        let reason = data.get("reason").map_or_else(|| "user_left".to_owned(), text_of);
        if is_owner {
            self.st().meeting_owner = None;
        }
        // Before the generation bump: a chunk passing the new generation must not
        // find the old meeting's speakers.
        self.clear_speaker_state();
        let Some(storage) = self.storage() else { return Ok(()) };
        let prev_meeting_id = storage.current_meeting_id();
        let prev_meeting_path = storage.current_meeting_path();
        let live = self.wired().live;
        if let Some(live) = &live {
            live.before_meeting_switch();
        }
        let new_meeting_id = storage.on_ktalk_leave(&reason)?;
        self.log(&format!("[KTalk Bridge] Left meeting (reason: {reason}), new meeting: {new_meeting_id}"));
        self.start_increments(prev_meeting_id.as_deref(), prev_meeting_path);
        if let Some(live) = &live {
            live.meeting_changed(Some(&new_meeting_id), storage.current_meeting_path().as_deref())?;
        }
        let stale = self.mark_stale(prev_meeting_id.as_deref(), true);
        if stale > 0 {
            self.log(&format!("[KTalk Bridge] Marked {stale} copilot request(s) stale on meeting_leave"));
        }
        let ended = self.meeting_ended_frame(prev_meeting_id.as_deref());
        self.st().tick = None;
        self.clear_transcript_buffer();
        self.emit_lifecycle(ended).await;
        // `new_meeting` resets the tab's state: only the tab that left.
        let frame = obj! {"type" => "new_meeting", "meeting_id" => new_meeting_id.as_str()};
        match client {
            Some(client) => {
                let _ = client.sink.send(dumps(&frame)).await;
            }
            None => self.broadcast_message(&frame).await,
        }
        Ok(())
    }

    /// `_end_meeting_on_extension_gone`: a conference meeting ends when its owner's
    /// socket closed (idle tabs on conference hosts hold sockets with no call) or no
    /// extension is left; the same Copilot cleanup as an explicit leave. Idempotent.
    ///
    /// # Errors
    /// What the storage or a hook raised.
    pub async fn end_meeting_on_extension_gone(self: &Arc<Self>) -> Result<(), String> {
        self.set_meeting_capture_active(false)?;
        self.clear_speaker_state();
        let Some(storage) = self.storage() else { return Ok(()) };
        if storage.current_meeting_path().is_none() {
            return Ok(());
        }
        let meta = match storage.load_meta() {
            Ok(meta) => meta,
            Err(e) => {
                self.log(&format!("[KTalk Bridge] _load_meta failed during disconnect cleanup: {e}"));
                return Ok(());
            }
        };
        if !Self::is_conference_meta(storage.as_ref(), &meta) {
            return Ok(());
        }
        let prev_meeting_id = storage.current_meeting_id();
        let prev_meeting_path = storage.current_meeting_path();
        let live = self.wired().live;
        if let Some(live) = &live {
            live.before_meeting_switch();
        }
        let new_meeting_id = storage.on_ktalk_leave("disconnected")?;
        if let Some(live) = &live {
            live.meeting_changed(Some(&new_meeting_id), storage.current_meeting_path().as_deref())?;
        }
        self.start_increments(prev_meeting_id.as_deref(), prev_meeting_path);
        let stale = self.mark_stale(prev_meeting_id.as_deref(), true);
        if stale > 0 {
            self.log(&format!("[KTalk Bridge] Marked {stale} copilot request(s) stale on extension disconnect"));
        }
        self.st().tick = None;
        self.clear_transcript_buffer();
        if prev_meeting_id.as_deref().is_some_and(|m| !m.is_empty()) {
            let ended = self.meeting_ended_frame(prev_meeting_id.as_deref());
            self.emit_lifecycle(ended).await;
        }
        self.log("[KTalk Bridge] Ended KTalk meeting due to client disconnect");
        Ok(())
    }

    /// `_send_transcript_history(websocket)`: to that client, or to every extension
    /// without one.
    pub(crate) async fn send_transcript_history(&self, client: Option<&Arc<Client>>) -> Result<(), String> {
        let Some(storage) = self.storage() else { return Ok(()) };
        let segments = storage.get_current_transcript()?;
        let meeting_id = storage.current_meeting_id();
        let frame = obj! {
            "type" => "transcript_history",
            "meeting_id" => meeting_id.as_deref(),
            "segments" => Value::Array(segments),
        };
        match client {
            Some(client) => {
                let _ = client.sink.send(dumps(&frame)).await;
            }
            None => self.broadcast_message(&frame).await,
        }
        Ok(())
    }

    /// `_preempt_manual_copilot(notify=)`: a conference takes the Copilot over; the
    /// manual mirror clears and the manual requests go stale. True when it was armed.
    pub(crate) async fn preempt_manual_copilot(&self, notify: bool) -> bool {
        if !self.st().manual_copilot_active {
            return false;
        }
        self.set_manual_copilot_active(false);
        let stale = {
            let mut st = self.st();
            let mut count = 0;
            for (_, mid) in &mut st.pending {
                if mid.is_empty() {
                    *mid = "stale:".to_owned();
                    count += 1;
                }
            }
            count
        };
        if stale > 0 {
            self.log(&format!("[KTalk Bridge] Marked {stale} manual copilot request(s) stale on preemption"));
        }
        if notify {
            self.notify_manual_copilot_preempted().await;
        }
        true
    }

    /// `_notify_manual_copilot_preempted`: the webapp only.
    pub(crate) async fn notify_manual_copilot_preempted(&self) {
        let frame = obj! {
            "type" => "copilot_status",
            "request_id" => "",
            "status" => "manual_preempted",
            "message" => "Manual copilot session preempted by a conference meeting",
        };
        self.broadcast_to_subscribers(&frame).await;
    }
}
