//! The meeting lifecycle: `meeting_join` (one owner at a time, a second conference
//! refused), `meeting_leave`, the end of a meeting whose extension is gone, and the
//! transcript history a joining tab gets.
//!
//! Known differences from Python: a `url`, `title`, `platform` or `reason` that is
//! not a string reaches the storage as its `str()` (Python passes the value, and a
//! non-string url raises inside `urlparse`; here it is refused with an error).

use std::sync::Arc;

use recog_core::protocol;
use recog_core::pyjson::{Map, Value};

use crate::bridge::Bridge;
use crate::client::Client;
use crate::copilot::TickState;
use crate::json::{self, dumps, dumps_ascii};
use crate::obj;
use crate::switch::{StoreHandle, SwitchError};

fn text_of(value: &Value) -> String {
    json::py_str(value)
}

/// `str(title)` of a title that is not None.
fn title_arg(title: &Value) -> Option<String> {
    match title {
        Value::Null => None,
        other => Some(text_of(other)),
    }
}

/// The `source` of a meta.json that is an object.
fn meta_source(meta: Result<Value, String>) -> Option<Value> {
    match meta {
        Ok(Value::Object(map)) => map.get("source").cloned(),
        _ => None,
    }
}

/// What a join changed in the storage, for the rest of the join.
struct JoinSwitch {
    meeting_id: Option<String>,
    prev_meeting_id: Option<String>,
    /// The storage moved to another meeting directory.
    path_changed: bool,
    /// The same directory, which only now becomes a live meeting (an orphan reused,
    /// or a `ktalk_only` installation that was not recording).
    became_active: bool,
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

    /// [`Bridge::mark_stale`], logged with `when` when it marked any.
    fn mark_stale_logged(&self, meeting_id: Option<&str>, skip_stale: bool, when: &str) {
        let stale = self.mark_stale(meeting_id, skip_stale);
        if stale > 0 {
            self.log(&format!("[KTalk Bridge] Marked {stale} copilot request(s) stale {when}"));
        }
    }

    fn clear_transcript_buffer(&self) {
        self.st().transcript.clear();
    }

    fn meeting_ended_frame(&self, meeting_id: Option<&str>) -> Value {
        obj! {"type" => protocol::MEETING_ENDED, "meeting_id" => meeting_id, "created_at" => Value::Float(self.now() * 1000.0)}
    }

    pub(crate) async fn on_meeting_join(
        self: &Arc<Self>,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        let url = data.get("url").cloned().unwrap_or(Value::Null);
        let title = data.get("title").cloned().unwrap_or(Value::Null);
        let platform = data.get("platform").map_or_else(|| "ktalk".to_owned(), text_of);
        if !self.config.recording_policy.accepts(&platform) {
            return self.reject_other_platform(client).await;
        }
        let Some(storage) = self.storage() else { return Ok(()) };
        if !url.truthy() || self.reject_second_tab(&storage, &url, client).await {
            return Ok(());
        }
        let Value::Str(url) = &url else {
            return Err(format!("'{}' object has no attribute 'decode'", url.type_name()));
        };
        let Some(switch) = self.join_stored_meeting(&storage, url, &title, &platform, client).await? else {
            return Ok(());
        };
        self.enter_meeting(&switch, title, client).await
    }

    /// The storage joins the meeting and, under `ktalk_only`, the capture starts.
    /// None when the join was refused: the live side could not move to the meeting,
    /// or the capture could not start.
    async fn join_stored_meeting(
        &self,
        storage: &StoreHandle,
        url: &str,
        title: &Value,
        platform: &str,
        client: Option<&Arc<Client>>,
    ) -> Result<Option<JoinSwitch>, String> {
        let gated = self.config.recording_policy.gated();
        let switch = match self.switch_join_meeting(storage, url, title, platform) {
            Ok(switch) => switch,
            Err(SwitchError::Writers(e)) if gated => {
                self.refuse_capture(&e, client).await?;
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        if gated && !self.start_ktalk_capture(client).await? {
            return Ok(None);
        }
        Ok(Some(switch))
    }

    /// `ktalk_only`: a conference of another platform is refused.
    async fn reject_other_platform(&self, client: Option<&Arc<Client>>) -> Result<(), String> {
        if let Some(client) = client {
            let frame =
                obj! {"type" => protocol::MEETING_JOIN_REJECTED, "reason" => self.config.recording_policy.name()};
            client.sink.send(dumps_ascii(&frame)).await.map_err(|e| e.0)?;
        }
        Ok(())
    }

    /// While the owner's tab is connected, another tab's join is refused (a second
    /// conference in the same browser). True when refused.
    async fn reject_second_tab(&self, storage: &StoreHandle, url: &Value, client: Option<&Arc<Client>>) -> bool {
        if !self.owner_blocks_join(client) || storage.current_meeting_path().is_none() {
            return false;
        }
        let owner_addr = {
            let st = self.st();
            st.meeting_owner.and_then(|o| st.clients.all.get(&o).map(|c| c.remote_address.clone()))
        };
        self.log(&format!(
            "[KTalk Bridge] Rejecting meeting_join from second tab (url={}); owner={} still active",
            text_of(url),
            owner_addr.as_deref().unwrap_or("unknown")
        ));
        if let Some(client) = client {
            let frame = obj! {
                "type" => protocol::MEETING_JOIN_REJECTED,
                "reason" => "another_tab_active",
                "active_meeting_id" => storage.current_meeting_id(),
            };
            let _ = client.sink.send(dumps(&frame)).await;
        }
        true
    }

    /// An owner is set, still connected, and `client` is not it.
    fn owner_blocks_join(&self, client: Option<&Arc<Client>>) -> bool {
        let st = self.st();
        match (st.meeting_owner, client) {
            (Some(owner), c) => c.is_none_or(|c| c.id != owner) && st.clients.connected.contains(&owner),
            (None, _) => false,
        }
    }

    /// The storage joins the meeting, the speakers of the previous one cleared first.
    fn switch_join_meeting(
        &self,
        storage: &StoreHandle,
        url: &str,
        title: &Value,
        platform: &str,
    ) -> Result<JoinSwitch, SwitchError> {
        let was_recording = self.is_recording_active();
        let prev_meeting_id = storage.current_meeting_id();
        let prev_source =
            if storage.current_meeting_path().is_some() { meta_source(storage.load_meta()) } else { None };
        self.clear_speaker_state();
        let switched = self.switch_stored_meeting(storage, |store| {
            let meeting_id = store.on_meeting_join(url, title_arg(title).as_deref(), platform)?;
            self.log(&format!("[KTalk Bridge] Joined {platform} meeting: {url}"));
            Ok(meeting_id)
        })?;
        let path_changed = switched.moved;
        let became_active = !path_changed
            && (prev_source == Some(Value::from("orphan")) || (self.config.recording_policy.gated() && !was_recording));
        Ok(JoinSwitch { meeting_id: switched.answer, prev_meeting_id, path_changed, became_active })
    }

    /// `ktalk_only`: the capture starts once the live side moved to the meeting; when
    /// it fails the join is refused. True when the capture runs.
    async fn start_ktalk_capture(&self, client: Option<&Arc<Client>>) -> Result<bool, String> {
        let Err(e) = self.set_meeting_capture_active(true) else {
            self.st().meeting_owner = client.map(|c| c.id);
            return Ok(true);
        };
        self.refuse_capture(&e, client).await?;
        Ok(false)
    }

    /// `ktalk_only`: the live side could not move to the meeting or start the
    /// capture; the capture is closed again and the join refused.
    async fn refuse_capture(&self, e: &str, client: Option<&Arc<Client>>) -> Result<(), String> {
        self.st().meeting_owner = None;
        if let Err(cleanup) = self.set_meeting_capture_active(false) {
            self.log(&format!("[KTalk Bridge] Capture cleanup failed: {cleanup}"));
        }
        self.log(&format!("[KTalk Bridge] Capture start failed: {e}"));
        if let Some(client) = client {
            let frame = obj! {"type" => protocol::MEETING_JOIN_REJECTED, "reason" => "capture_unavailable"};
            self.send_bounded(client, &dumps_ascii(&frame)).await.map_err(|e| e.0)?;
        }
        Ok(())
    }

    /// A new (or newly live) meeting starts; a reconnect keeps its state. The joining
    /// tab becomes the owner and gets the history.
    async fn enter_meeting(
        self: &Arc<Self>,
        switch: &JoinSwitch,
        title: Value,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        if switch.path_changed || switch.became_active {
            self.start_joined_meeting(switch, title).await?;
        } else {
            // A reconnect keeps the tick counters, but re-arms them when a spurious
            // leave cleared them or they point at another meeting.
            let now = self.now();
            let mut st = self.st();
            if st.tick.as_ref().is_none_or(|t| t.meeting_id != switch.meeting_id) {
                st.tick = Some(TickState::new(switch.meeting_id.clone(), title, now));
            }
        }
        if let Some(client) = client {
            self.st().meeting_owner = Some(client.id);
        }
        // History to the joining tab only: a broadcast would move a sibling tab's
        // `currentMeetingId` into this meeting.
        self.send_transcript_history(client).await
    }

    /// `meeting_started` for a joined meeting: the previous meeting's requests go
    /// stale, and a manual Copilot is preempted.
    async fn start_joined_meeting(self: &Arc<Self>, switch: &JoinSwitch, title: Value) -> Result<(), String> {
        // A direct switch to another meeting strands the old meeting's requests:
        // their late replies must not reach the new meeting's Copilot tab.
        let prev_meeting_id = switch.prev_meeting_id.as_deref();
        if prev_meeting_id.is_some_and(|p| !p.is_empty()) && switch.prev_meeting_id != switch.meeting_id {
            self.mark_stale_logged(prev_meeting_id, false, "on meeting_join transition");
        }
        // Segment ids restart at 1 per meeting.
        self.clear_transcript_buffer();
        let started = self.meeting_started_frame(switch.meeting_id.as_deref(), title.clone());
        // The manual mirror and its requests go before the first send can let a
        // late reply in; the webapp hears of it after the lifecycle.
        let preempted = self.preempt_manual_copilot(false).await;
        self.emit_lifecycle(started).await;
        if preempted {
            self.notify_manual_copilot_preempted().await;
        }
        self.st().tick = Some(TickState::new(switch.meeting_id.clone(), title, self.now()));
        Ok(())
    }

    fn meeting_started_frame(&self, meeting_id: Option<&str>, title: Value) -> Value {
        obj! {
            "type" => protocol::MEETING_STARTED,
            "meeting_id" => meeting_id,
            "meeting_title" => title,
            "created_at" => Value::Float(self.now() * 1000.0),
        }
    }

    pub(crate) async fn on_meeting_leave(
        self: &Arc<Self>,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        let is_owner = client.map(|c| c.id) == self.st().meeting_owner;
        if self.config.recording_policy.gated() && !is_owner {
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
        let (prev_meeting_id, new_meeting_id) = self.leave_stored_meeting(&storage, &reason)?;
        self.mark_stale_logged(prev_meeting_id.as_deref(), true, "on meeting_leave");
        let ended = self.meeting_ended_frame(prev_meeting_id.as_deref());
        self.st().tick = None;
        self.clear_transcript_buffer();
        self.emit_lifecycle(ended).await;
        // `new_meeting` resets the tab's state: only the tab that left.
        let frame = obj! {"type" => "new_meeting", "meeting_id" => new_meeting_id.as_str()};
        self.send_to_tab_or_extensions(client, &frame).await;
        Ok(())
    }

    /// The storage leaves the meeting for a new orphan one around the live side's
    /// writers, and the left meeting's increments start; answers (the left meeting,
    /// the new one).
    fn leave_stored_meeting(&self, storage: &StoreHandle, reason: &str) -> Result<(Option<String>, String), String> {
        let prev_meeting_id = storage.current_meeting_id();
        let prev_meeting_path = storage.current_meeting_path();
        let switched = self.switch_stored_meeting(storage, |store| {
            let new_meeting_id = store.on_ktalk_leave(reason)?;
            self.log(&format!("[KTalk Bridge] Left meeting (reason: {reason}), new meeting: {new_meeting_id}"));
            self.start_increments(prev_meeting_id.as_deref(), prev_meeting_path);
            Ok(new_meeting_id)
        })?;
        Ok((prev_meeting_id, switched.answer))
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
        if !self.in_conference_meeting(&storage) {
            return Ok(());
        }
        let prev_meeting_id = storage.current_meeting_id();
        let prev_meeting_path = storage.current_meeting_path();
        self.switch_stored_meeting(&storage, |store| store.on_ktalk_leave("disconnected"))?;
        self.start_increments(prev_meeting_id.as_deref(), prev_meeting_path);
        self.mark_stale_logged(prev_meeting_id.as_deref(), true, "on extension disconnect");
        self.st().tick = None;
        self.clear_transcript_buffer();
        if prev_meeting_id.as_deref().is_some_and(|m| !m.is_empty()) {
            let ended = self.meeting_ended_frame(prev_meeting_id.as_deref());
            self.emit_lifecycle(ended).await;
        }
        self.log("[KTalk Bridge] Ended KTalk meeting due to client disconnect");
        Ok(())
    }

    /// The storage is in a conference meeting; a meta.json that cannot be read is
    /// logged and counts as none.
    fn in_conference_meeting(&self, storage: &StoreHandle) -> bool {
        if storage.current_meeting_path().is_none() {
            return false;
        }
        match storage.load_meta() {
            Ok(meta) => Self::is_conference_meta(storage, &meta),
            Err(e) => {
                self.log(&format!("[KTalk Bridge] _load_meta failed during disconnect cleanup: {e}"));
                false
            }
        }
    }

    /// A frame to `client`, or to every extension without one; a failed send to the
    /// client is ignored.
    async fn send_to_tab_or_extensions(&self, client: Option<&Arc<Client>>, frame: &Value) {
        match client {
            Some(client) => {
                let _ = client.sink.send(dumps(frame)).await;
            }
            None => self.broadcast_message(frame).await,
        }
    }

    /// `_send_transcript_history(websocket)`: to that client, or to every extension
    /// without one.
    pub async fn send_transcript_history(&self, client: Option<&Arc<Client>>) -> Result<(), String> {
        let Some(storage) = self.storage() else { return Ok(()) };
        let segments = storage.get_current_transcript()?;
        let meeting_id = storage.current_meeting_id();
        let frame = obj! {
            "type" => "transcript_history",
            "meeting_id" => meeting_id.as_deref(),
            "segments" => Value::Array(segments),
        };
        self.send_to_tab_or_extensions(client, &frame).await;
        Ok(())
    }

    /// `_preempt_manual_copilot(notify=)`: a conference takes the Copilot over; the
    /// manual mirror clears and the manual requests go stale. True when it was armed.
    pub async fn preempt_manual_copilot(&self, notify: bool) -> bool {
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
    pub async fn notify_manual_copilot_preempted(&self) {
        let frame = obj! {
            "type" => protocol::COPILOT_STATUS,
            "request_id" => "",
            "status" => protocol::STATUS_MANUAL_PREEMPTED,
            "message" => "Manual copilot session preempted by a conference meeting",
        };
        self.broadcast_to_subscribers(&frame).await;
    }
}
