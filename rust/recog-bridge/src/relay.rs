//! The Copilot frames: the channel's hello, chat relay and replies, the recent
//! transcript, manual sessions (spawn/stop, the supervisor's hello), the agent-state
//! merge, and the profile and settings refreshes of the webapp.

use std::sync::Arc;

use recog_core::protocol;
use recog_core::pyjson::{Map, Value};

use crate::bridge::{Bridge, copy_identity};
use crate::client::Client;
use crate::copilot::MANUAL_MEETING_ID;
use crate::hooks::CopilotPolicy;
use crate::json::{self, dumps, dumps_ascii};
use crate::obj;

pub(crate) fn status_frame(request_id: &str, status: &str, message: &str) -> Value {
    obj! {"type" => protocol::COPILOT_STATUS, "request_id" => request_id, "status" => status, "message" => message}
}

/// What a `get_recent_transcript_request` asks for; a field of the wrong type gets
/// Python's default (180 s, every id, refined segments included).
struct RecentWindow {
    seconds: f64,
    since: Option<i128>,
    include_refined: bool,
}

impl RecentWindow {
    fn of(data: &Map) -> Self {
        let seconds = data.get("window_seconds").and_then(json::as_f64).filter(|w| *w >= 0.0).unwrap_or(180.0);
        let since = data.get("since_segment_id").filter(|v| json::is_int(v)).and_then(json::as_int);
        let include_refined = match data.get("include_refined") {
            Some(Value::Bool(b)) => *b,
            _ => true,
        };
        Self { seconds, since, include_refined }
    }
}

fn with_identity(bridge: &Bridge, kind: &str, rest: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    map.insert("type", Value::from(kind));
    let mut map =
        json::merged(&json::merged(&map, &bridge.user_identity_snapshot()), &bridge.copilot_policy_snapshot());
    for (k, v) in rest {
        map.insert(*k, v.clone());
    }
    Value::Object(map)
}

impl Bridge {
    fn is_subscriber(&self, client: Option<&Arc<Client>>) -> bool {
        client.is_some_and(|c| self.st().clients.subscribers.contains(&c.id))
    }

    /// `ai_settings_refresh` (webapp only): re-read the Copilot policy; a change goes
    /// to the supervisor and the channel, and switching the Copilot off fails the
    /// pending requests and the manual mirror.
    pub(crate) async fn refresh_ai_settings(&self, client: Option<&Arc<Client>>) -> Result<(), String> {
        let Some(client) = client.filter(|c| self.is_subscriber(Some(c))) else { return Ok(()) };
        let _guard = self.settings_lock.lock().await;
        let Some(policy) = self.read_saved_policy().await else {
            let frame = error_frame(protocol::AI_SETTINGS_ERROR, "Could not apply the saved AI settings");
            self.send_bounded(client, &dumps_ascii(&frame)).await.map_err(|e| e.0)?;
            return Ok(());
        };
        if self.adopt_policy(&policy) {
            self.announce_policy_change(&policy).await;
        }
        let applied = obj! {"type" => protocol::AI_SETTINGS_APPLIED, "revision" => policy.revision, "model_application" => "next_session"};
        self.send_bounded(client, &dumps(&applied)).await.map_err(|e| e.0)?;
        Ok(())
    }

    /// The saved policy, read off the loop; None when it cannot be read.
    async fn read_saved_policy(&self) -> Option<CopilotPolicy> {
        let source = self.policy_source.clone();
        match tokio::task::spawn_blocking(move || source.read_policy()).await {
            Ok(Ok(policy)) => Some(policy),
            _ => None,
        }
    }

    /// Keeps `policy`; answers whether it differs from the one it replaces.
    fn adopt_policy(&self, policy: &CopilotPolicy) -> bool {
        let mut st = self.st();
        let changed = !policy.same_as(&st.policy);
        st.policy = policy.clone();
        changed
    }

    /// A changed policy: the supervisor and the channel hear of it, and switching the
    /// Copilot off first fails what waits for it.
    async fn announce_policy_change(&self, policy: &CopilotPolicy) {
        if !policy.enabled.truthy() {
            self.fail_requests_of_disabled_copilot().await;
        }
        self.relay_to_supervisors(&obj! {"type" => protocol::COPILOT_SETTINGS_CHANGED}).await;
        let updated = with_identity(self, protocol::COPILOT_POLICY_UPDATED, &[]);
        self.broadcast_to_channel_clients(&updated).await;
    }

    /// The Copilot was switched off: the manual mirror ends and each pending request
    /// gets a terminal error.
    async fn fail_requests_of_disabled_copilot(&self) {
        self.set_manual_copilot_active(false);
        let pending: Vec<String> = self.st().pending.keys().cloned().collect();
        for request_id in pending {
            // Python builds this one with `status` before `request_id`.
            let frame = obj! {
                "type" => protocol::COPILOT_STATUS,
                "status" => "error",
                "request_id" => request_id.as_str(),
                "message" => "Copilot выключен в настройках.",
            };
            self.broadcast_copilot_frame(&frame).await;
        }
        self.st().pending.clear();
    }

    /// `user_profile_refresh` (webapp only): the profile file is the truth, the frame
    /// a hint; the microphone takes the new identity, the channel hears of a change
    /// and the asking tab gets the applied revision.
    pub(crate) async fn refresh_user_profile(&self, client: Option<&Arc<Client>>) -> Result<(), String> {
        let Some(client) = client.filter(|c| self.is_subscriber(Some(c))) else { return Ok(()) };
        let _guard = self.profile_lock.lock().await;
        let Ok((identity, changed)) = self.apply_user_profile() else {
            let frame = error_frame(protocol::USER_PROFILE_ERROR, "Could not apply the saved user profile");
            self.send_bounded(client, &dumps_ascii(&frame)).await.map_err(|e| e.0)?;
            return Ok(());
        };
        let fields = identity.as_object().cloned().unwrap_or_default();
        if changed {
            let mut frame = Map::new();
            frame.insert("type", Value::from(protocol::USER_PROFILE_UPDATED));
            self.broadcast_to_channel_clients(&Value::Object(json::merged(&frame, &fields))).await;
        }
        let revision = fields.get("local_user_profile_revision").cloned().ok_or("'local_user_profile_revision'")?;
        let frame = obj! {"type" => protocol::USER_PROFILE_APPLIED, "revision" => revision, "identity" => identity};
        self.send_bounded(client, &dumps(&frame)).await.map_err(|e| e.0)?;
        Ok(())
    }

    /// Reads the saved profile, hands it to the microphone and keeps it; answers the
    /// identity and whether it changed.
    fn apply_user_profile(&self) -> Result<(Value, bool), String> {
        let source = self.wired().identity.ok_or_else(|| "Live identity is not connected".to_owned())?;
        let identity = copy_identity(&source.read_identity()?)?;
        let changed = !identity.py_eq(&self.st().identity);
        source.apply_mic_identity(&identity)?;
        self.st().identity = identity.clone();
        Ok((identity, changed))
    }

    /// `copilot_channel_hello`: the socket leaves the extensions for the channel set,
    /// gets the policy, and the first channel client is announced to everyone.
    pub(crate) async fn on_channel_hello(self: &Arc<Self>, client: Option<&Arc<Client>>) -> Result<(), String> {
        let Some(client) = client else {
            self.log("[KTalk Bridge] copilot_channel_hello without websocket — ignored");
            return Ok(());
        };
        let (first, no_extensions) = {
            let mut st = self.st();
            st.clients.connected.remove(&client.id);
            let first = st.clients.channels.is_empty();
            st.clients.channels.insert(client.id);
            (first, st.clients.connected.is_empty())
        };
        self.log(&format!("[KTalk Bridge] Copilot channel client registered from {}", client.remote_address));
        let policy = with_identity(self, protocol::COPILOT_POLICY_UPDATED, &[]);
        self.send_bounded(client, &dumps(&policy)).await.map_err(|e| e.0)?;
        // The socket sat among the extensions until now: an extension that left
        // meanwhile saw one still there and kept the meeting open.
        if no_extensions {
            self.end_meeting_on_extension_gone().await?;
        }
        if first {
            self.broadcast_copilot_frame(&status_frame("", "channel_connected", "Copilot channel server connected"))
                .await;
        }
        Ok(())
    }

    /// `copilot_chat_message`: a typed question to every channel client, enriched
    /// with the meeting; the webapp tabs see it echoed.
    pub(crate) async fn on_chat_message(&self, data: &Map, client: Option<&Arc<Client>>) -> Result<(), String> {
        let Some(request_id) = json::nonempty_str(data.get("request_id")) else {
            self.log("[KTalk Bridge] copilot_chat_message missing/invalid request_id — ignored");
            return Ok(());
        };
        let Some(text) = json::nonempty_str(data.get("text")) else {
            self.log("[KTalk Bridge] copilot_chat_message missing/invalid text — ignored");
            return Ok(());
        };
        let meeting_id = self.chat_meeting_id();
        let created_at = self.created_at_or_now(data);
        let channels = {
            let st = self.st();
            st.clients.list(&st.clients.channels)
        };
        if channels.is_empty() {
            self.reply_no_channel(client, request_id).await;
            return Ok(());
        }
        let outbound = with_identity(
            self,
            protocol::COPILOT_USER_MESSAGE,
            &[
                ("request_id", Value::from(request_id)),
                ("text", Value::from(text)),
                ("meeting_id", Value::from(meeting_id.as_deref())),
                ("meeting_title", data.get("meeting_title").cloned().unwrap_or(Value::Null)),
                ("transcript_window", data.get("transcript_window").cloned().unwrap_or(Value::Null)),
                ("created_at", created_at.clone()),
            ],
        );
        // Published before the first send, so a join re-entering meanwhile can mark it.
        self.st().pending.insert(request_id.to_owned(), meeting_id.clone().unwrap_or_default());
        let message = dumps(&outbound);
        let echo = obj! {"type" => protocol::COPILOT_CHAT_ECHO, "request_id" => request_id, "text" => text, "created_at" => created_at};
        let forwards = channels.iter().map(|channel| async {
            if let Err(e) = self.send_bounded(channel, &message).await {
                self.log(&format!("[KTalk Bridge] Failed to forward copilot_chat_message to channel client: {e}"));
            }
        });
        futures_util::future::join(self.broadcast_to_subscribers(&echo), futures_util::future::join_all(forwards))
            .await;
        Ok(())
    }

    /// The meeting a chat question belongs to. Only a conference request is stamped:
    /// a manual one would get the orphan meeting, which a silence split rotates, and
    /// its reply would look stale.
    fn chat_meeting_id(&self) -> Option<String> {
        if self.storage().is_some() && self.is_conference_active() { self.current_meeting_id() } else { None }
    }

    /// The frame's numeric `created_at`, else now in milliseconds.
    fn created_at_or_now(&self, data: &Map) -> Value {
        match data.get("created_at") {
            Some(v) if json::is_number(v) => v.clone(),
            _ => Value::Float(self.now() * 1000.0),
        }
    }

    /// A chat with no channel connected: its originator learns the channel is gone.
    async fn reply_no_channel(&self, client: Option<&Arc<Client>>, request_id: &str) {
        match client {
            Some(client) => {
                let frame = status_frame(request_id, "channel_disconnected", "Copilot channel server is not connected");
                if let Err(e) = client.sink.send(dumps(&frame)).await {
                    self.log(&format!("[KTalk Bridge] Failed to send channel_disconnected status: {e}"));
                }
            }
            None => {
                self.log("[KTalk Bridge] copilot_chat_message dropped — no channel client and no originator socket")
            }
        }
    }

    /// `copilot_channel_reply`: relayed as `copilot_reply` unless its request went
    /// stale or it belongs to another meeting; a tracked request then gets a terminal
    /// error so its UI stops waiting.
    pub(crate) async fn on_channel_reply(&self, data: &Map) -> Result<(), String> {
        let Some(request_id) = json::nonempty_str(data.get("request_id")) else {
            self.log("[KTalk Bridge] copilot_channel_reply missing/invalid request_id — ignored");
            return Ok(());
        };
        let Some(Value::Str(text)) = data.get("text") else {
            self.log("[KTalk Bridge] copilot_channel_reply missing/invalid text — ignored");
            return Ok(());
        };
        let Some(kind) = json::nonempty_str(data.get("kind")) else {
            self.log("[KTalk Bridge] copilot_channel_reply missing/invalid kind — ignored");
            return Ok(());
        };
        let meeting_id = data.get("meeting_id").cloned().unwrap_or(Value::Null);
        if !matches!(meeting_id, Value::Null | Value::Str(_)) {
            self.log("[KTalk Bridge] copilot_channel_reply invalid meeting_id type — ignored");
            return Ok(());
        }
        let tracked = self.st().pending.get(request_id).cloned();
        if let Some(scope) = tracked.as_deref().and_then(|t| t.strip_prefix("stale:")) {
            self.drop_stale_reply(request_id, kind, scope).await;
            return Ok(());
        }
        if let Some((reply_meeting, current)) = self.other_meeting_of(&meeting_id) {
            self.drop_other_meeting_reply(request_id, kind, &reply_meeting, &current).await;
            return Ok(());
        }
        let created_at = self.created_at_or_now(data);
        self.broadcast_copilot_frame(&reply_frame(request_id, kind, text, meeting_id, created_at)).await;
        self.st().pending.shift_remove(request_id);
        Ok(())
    }

    /// A reply to a request gone stale: dropped, and its UI told why.
    async fn drop_stale_reply(&self, request_id: &str, kind: &str, scope: &str) {
        self.log(&format!(
            "[KTalk Bridge] copilot_channel_reply for stale request_id={request_id} (kind={kind}) — dropped"
        ));
        self.st().pending.shift_remove(request_id);
        let message = if scope.is_empty() {
            "Ответ не доставлен: ручная сессия copilot была прервана."
        } else {
            "Ответ не доставлен: встреча, в которой был задан вопрос, уже завершилась."
        };
        self.broadcast_copilot_frame(&status_frame(request_id, "error", message)).await;
    }

    /// (the reply's meeting, the current one) when the reply names a meeting and the
    /// bridge is in another.
    fn other_meeting_of(&self, meeting_id: &Value) -> Option<(String, String)> {
        let Value::Str(mid) = meeting_id else { return None };
        if mid.is_empty() {
            return None;
        }
        let current = self.current_meeting_id().filter(|c| !c.is_empty())?;
        (current != *mid).then(|| (mid.clone(), current))
    }

    /// A reply of another meeting: dropped; a tracked request gets a terminal error.
    async fn drop_other_meeting_reply(&self, request_id: &str, kind: &str, reply_meeting: &str, current: &str) {
        self.log(&format!(
            "[KTalk Bridge] copilot_channel_reply for meeting_id={reply_meeting} but current is {current} (kind={kind}) — dropped"
        ));
        let tracked = self.st().pending.shift_remove(request_id);
        if let Some(scope) = tracked {
            let message = if scope.is_empty() {
                "Ответ не доставлен: ручная сессия copilot была прервана."
            } else {
                "Ответ не доставлен: встреча сменилась до получения ответа."
            };
            self.broadcast_copilot_frame(&status_frame(request_id, "error", message)).await;
        }
    }

    /// `copilot_channel_status`: a per-request status, relayed unless stale; a
    /// terminal one ends the bookkeeping.
    pub(crate) async fn on_channel_status(&self, data: &Map) -> Result<(), String> {
        let Some(request_id) = json::nonempty_str(data.get("request_id")) else {
            self.log("[KTalk Bridge] copilot_channel_status missing/invalid request_id — ignored");
            return Ok(());
        };
        let Some(status) = json::nonempty_str(data.get("status")) else {
            self.log("[KTalk Bridge] copilot_channel_status missing/invalid status — ignored");
            return Ok(());
        };
        if self.st().pending.get(request_id).is_some_and(|t| t.starts_with("stale:")) {
            self.log(&format!(
                "[KTalk Bridge] copilot_channel_status for stale request_id={request_id} (status={status}) — dropped"
            ));
            return Ok(());
        }
        let message = match data.get("message") {
            Some(Value::Str(m)) => m.as_str(),
            _ => "",
        };
        self.broadcast_copilot_frame(&status_frame(request_id, status, message)).await;
        if matches!(status, "done" | "error") {
            self.st().pending.shift_remove(request_id);
        }
        Ok(())
    }

    /// `get_recent_transcript_request`: the buffered segments of the current meeting
    /// within the window, to the asking channel only.
    pub(crate) async fn on_recent_transcript_request(
        &self,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        let Some(request_id) = json::nonempty_str(data.get("request_id")) else {
            self.log("[KTalk Bridge] get_recent_transcript_request missing/invalid request_id — ignored");
            return Ok(());
        };
        let segments = self.recent_segments(&RecentWindow::of(data));
        let response = with_identity(
            self,
            protocol::GET_RECENT_TRANSCRIPT_RESPONSE,
            &[("request_id", Value::from(request_id)), ("segments", Value::Array(segments))],
        );
        match client {
            Some(client) => {
                if let Err(e) = client.sink.send(dumps(&response)).await {
                    self.log(&format!("[KTalk Bridge] Failed to send get_recent_transcript_response: {e}"));
                }
            }
            None => self.log("[KTalk Bridge] get_recent_transcript_request without websocket — response dropped"),
        }
        Ok(())
    }

    /// The buffered segments of the current meeting that `window` selects, in order.
    fn recent_segments(&self, window: &RecentWindow) -> Vec<Value> {
        let current = self.current_meeting_id();
        let cutoff_ms = self.now() * 1000.0 - window.seconds * 1000.0;
        let entries = self.st().transcript.ordered();
        let current_value = Value::from(current.as_deref());
        entries
            .into_iter()
            .filter(|e| e.get("meeting_id").unwrap_or(&Value::Null) == &current_value)
            .filter(|e| e.get("end_ms").and_then(json::as_f64).unwrap_or(0.0) >= cutoff_ms)
            .filter(|e| {
                window.since.is_none_or(|s| e.get("segment_id").and_then(json::as_int).is_some_and(|id| id > s))
            })
            .filter(|e| window.include_refined || !e.get("refined").is_some_and(Value::truthy))
            .map(Value::Object)
            .collect()
    }

    /// `copilot_spawn_request`: relayed to the supervisor when no conference owns the
    /// Copilot; the manual mirror is armed only for a delivered request.
    pub(crate) async fn on_spawn_request(&self, data: &Map, client: Option<&Arc<Client>>) -> Result<(), String> {
        let Some(request_id) = json::nonempty_str(data.get("request_id")) else {
            self.log("[KTalk Bridge] copilot_spawn_request missing/invalid request_id — ignored");
            return Ok(());
        };
        let _guard = self.manual_lock.lock().await;
        let conference = "A conference meeting owns the copilot session";
        if self.is_conference_active() {
            self.reply_copilot_status(client, request_id, protocol::STATUS_CONFERENCE_ACTIVE, conference).await;
            return Ok(());
        }
        let delivered = self.relay_to_supervisors(&Value::Object(data.clone())).await;
        // A join may have completed a whole conference start during the relay.
        if self.is_conference_active() {
            self.log(&format!(
                "[KTalk Bridge] Conference started while relaying copilot_spawn_request request_id={request_id} — manual mirror not armed"
            ));
            self.reply_copilot_status(client, request_id, protocol::STATUS_CONFERENCE_ACTIVE, conference).await;
            return Ok(());
        }
        if !delivered {
            self.reply_copilot_status(
                client,
                request_id,
                protocol::STATUS_SUPERVISOR_DISCONNECTED,
                "Copilot supervisor is not connected",
            )
            .await;
            return Ok(());
        }
        self.set_manual_copilot_active(true);
        Ok(())
    }

    /// `copilot_stop_request`: the mirror clears even when the relay fails, and again
    /// after it (a reconnecting supervisor may have re-armed it meanwhile).
    pub(crate) async fn on_stop_request(&self, data: &Map, client: Option<&Arc<Client>>) -> Result<(), String> {
        let Some(request_id) = json::nonempty_str(data.get("request_id")) else {
            self.log("[KTalk Bridge] copilot_stop_request missing/invalid request_id — ignored");
            return Ok(());
        };
        let _guard = self.manual_lock.lock().await;
        self.set_manual_copilot_active(false);
        if !self.relay_to_supervisors(&Value::Object(data.clone())).await {
            self.reply_copilot_status(
                client,
                request_id,
                protocol::STATUS_SUPERVISOR_DISCONNECTED,
                "Copilot supervisor is not connected",
            )
            .await;
        }
        self.set_manual_copilot_active(false);
        Ok(())
    }

    /// `supervisor_hello`: the socket joins the supervisor set, the race with a
    /// departed extension is settled, a manual session it reports is re-armed, and
    /// it gets `meeting_state` to reconcile.
    pub(crate) async fn on_supervisor_hello(
        self: &Arc<Self>,
        data: &Map,
        client: Option<&Arc<Client>>,
    ) -> Result<(), String> {
        let Some(client) = client else {
            self.log("[KTalk Bridge] supervisor_hello without websocket — ignored");
            return Ok(());
        };
        let no_extensions = {
            let mut st = self.st();
            st.clients.connected.remove(&client.id);
            st.clients.supervisors.insert(client.id);
            st.clients.connected.is_empty()
        };
        self.log(&format!("[KTalk Bridge] Copilot supervisor registered from {}", client.remote_address));
        if no_extensions {
            self.end_meeting_on_extension_gone().await?;
        }
        if data.get("current_meeting_id") == Some(&Value::from(MANUAL_MEETING_ID)) && !self.is_conference_active() {
            self.set_manual_copilot_active(true);
        }
        let state = self.compute_meeting_state();
        if let Err(e) = client.sink.send(dumps(&state)).await {
            self.log(&format!("[KTalk Bridge] Failed to send meeting_state to supervisor: {e}"));
        }
        Ok(())
    }

    /// `copilot_agent_status`: the supervisor's screen state, then the merge.
    pub(crate) async fn on_agent_status(&self, data: &Map) -> Result<(), String> {
        let Some(status) = json::nonempty_str(data.get("status")) else {
            self.log("[KTalk Bridge] copilot_agent_status missing/invalid status — ignored");
            return Ok(());
        };
        let now = self.clock.monotonic();
        {
            let mut st = self.st();
            st.agent.supervisor_status = Some(status.to_owned());
            st.agent.supervisor_status_ts = now;
        }
        self.recompute_agent_state(None).await;
        Ok(())
    }

    /// `claude_sessions`: the channel's MCP session count; null is unknown, never a
    /// confident zero.
    pub(crate) async fn on_claude_sessions(&self, data: &Map) -> Result<(), String> {
        let count = match data.get("count") {
            None | Some(Value::Null) => None,
            Some(v) if json::is_int(v) => json::as_int(v),
            Some(_) => {
                self.log("[KTalk Bridge] claude_sessions invalid count — ignored");
                return Ok(());
            }
        };
        self.st().agent.claude_sessions = count;
        self.recompute_agent_state(None).await;
        Ok(())
    }

    /// `_recompute_agent_state(now)`: the effective chip status, broadcast only on a
    /// change; a TUI that lost its MCP session for a minute is reported for
    /// replacement. Called on every report and every second by the server loop.
    pub async fn recompute_agent_state(&self, now: Option<f64>) {
        let now = now.unwrap_or_else(|| self.clock.monotonic());
        // Python evaluates "should a session be alive" lazily, after the cheaper
        // conditions, and only then reads meta.json; the same order keeps its log
        // lines.
        let mut agent = std::mem::take(&mut self.st().agent);
        let alive = agent.sessions_lost_under_supervisor() && self.copilot_should_be_alive();
        agent.update_zero_anchor(now, alive);
        let effective = agent.effective(now, || self.copilot_should_be_alive());
        let recover = agent.take_recovery(now);
        self.st().agent = agent;
        if recover {
            self.relay_to_supervisors(&self.channel_unavailable_frame()).await;
        }
        if self.note_effective_state(&effective) {
            self.broadcast_copilot_frame(&agent_state_frame(effective)).await;
        }
    }

    /// `copilot_channel_unavailable` for the meeting a session serves now.
    fn channel_unavailable_frame(&self) -> Value {
        let meeting_id = self.compute_meeting_state().as_object().and_then(|m| m.get("meeting_id").cloned());
        obj! {"type" => protocol::COPILOT_CHANNEL_UNAVAILABLE, "meeting_id" => meeting_id.unwrap_or(Value::Null)}
    }

    /// Keeps `effective` as the last broadcast status; answers whether it changed.
    fn note_effective_state(&self, effective: &str) -> bool {
        let mut st = self.st();
        let changed = st.agent.last_effective != effective;
        if changed {
            effective.clone_into(&mut st.agent.last_effective);
        }
        changed
    }
}

/// The agent chip's status frame.
pub(crate) fn agent_state_frame(effective: String) -> Value {
    obj! {"type" => protocol::COPILOT_STATUS, "request_id" => "", "status" => protocol::STATUS_AGENT_STATE, protocol::STATUS_AGENT_STATE => effective}
}

/// `{"type": kind, "message": message}`: a refresh the webapp asked for failed.
fn error_frame(kind: &str, message: &str) -> Value {
    obj! {"type" => kind, "message" => message}
}

/// The `copilot_reply` relayed to the extensions and the webapp.
fn reply_frame(request_id: &str, kind: &str, text: &str, meeting_id: Value, created_at: Value) -> Value {
    obj! {
        "type" => protocol::COPILOT_REPLY,
        "request_id" => request_id,
        "kind" => kind,
        "text" => text,
        "meeting_id" => meeting_id,
        "created_at" => created_at,
    }
}
