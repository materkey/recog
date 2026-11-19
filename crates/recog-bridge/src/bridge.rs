//! `KTalkBridge`: its state, the queries and sends the live process calls, and the
//! fan-out helpers every handler sends through.
//!
//! Threads as in Python: the handlers run on the bridge's own runtime thread (a
//! current-thread tokio runtime, the counterpart of the asyncio loop thread), so two
//! handlers interleave only where one awaits; the live process calls the queries and
//! `send_transcript`/`send_refine` from its own threads. Everything those threads
//! share sits behind one lock (`self._lock`); it is never held across an await nor
//! while a hook runs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::join_all;
use indexmap::IndexMap;
use recog_core::protocol;
use recog_core::pyjson::{Map, Value};
use recog_core::user_profile::{Profile, resolve_identity};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::client::{Client, ClientId, SendError};
use crate::copilot::{
    AgentMerge, COPILOT_TICK_INTERVAL_SECONDS, COPILOT_TICK_NEW_SEGMENTS, COPILOT_TICK_RECENT_SEGMENTS, TickState,
    TranscriptBuffer,
};
use crate::hooks::{
    Clock, CopilotPolicy, GoldenTap, IdentitySource, IncrementGenerator, LiveEvents, MeetingCapture, MeetingStore,
    Participant, PolicySource, SpeakerEvent, SpeakerSegment, SystemClock, local_isoformat,
};
use crate::json::{self, dumps, dumps_ascii};
use crate::obj;
use crate::speakers::Speakers;
use crate::switch::StoreHandle;

/// Where the bridge's lines go (all of Python's go to stderr).
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// How a bridge is set up: `KTalkBridge(host, port, ktalk_only=)` and the module
/// constants a test may change.
#[derive(Clone)]
pub struct BridgeConfig {
    pub host: String,
    /// 0 binds a free port.
    pub port: u16,
    /// Record only during a conference: joins switch the capture on.
    pub recording_policy: recog_core::platforms::RecordingPolicy,
    /// Exact origins admitted beside the built-in ones (`ws_origin.EXTRA_ORIGINS`).
    pub extra_origins: Vec<String>,
    /// The debug state file (`/tmp/ktalk_debug.json`); None writes none.
    pub debug_file: Option<PathBuf>,
    /// `BROADCAST_SEND_TIMEOUT_S`.
    pub send_timeout: Duration,
    /// `MIC_SPEAKER_NAME`, the legacy name of the first identity.
    pub legacy_mic_name: String,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            host: "localhost".to_owned(),
            port: 8766,
            recording_policy: Default::default(),
            extra_origins: Vec::new(),
            debug_file: Some(PathBuf::from(protocol::KTALK_DEBUG_FILE)),
            send_timeout: Duration::from_secs(5),
            legacy_mic_name: "Me".to_owned(),
        }
    }
}

/// The client sets of `KTalkBridge`: every socket starts as an extension
/// (`_connected_clients`) and moves on its hello.
#[derive(Default)]
pub(crate) struct Clients {
    pub all: BTreeMap<ClientId, Arc<Client>>,
    pub connected: BTreeSet<ClientId>,
    pub subscribers: BTreeSet<ClientId>,
    pub channels: BTreeSet<ClientId>,
    pub supervisors: BTreeSet<ClientId>,
    pub timed_out: BTreeSet<ClientId>,
}

impl Clients {
    pub fn list(&self, ids: &BTreeSet<ClientId>) -> Vec<Arc<Client>> {
        ids.iter().filter_map(|id| self.all.get(id).cloned()).collect()
    }

    pub fn registered(&self, id: ClientId) -> bool {
        self.connected.contains(&id)
            || self.subscribers.contains(&id)
            || self.channels.contains(&id)
            || self.supervisors.contains(&id)
    }
}

pub(crate) struct State {
    pub speakers: Speakers,
    pub clients: Clients,
    pub identity: Value,
    pub policy: CopilotPolicy,
    pub manual_copilot_active: bool,
    pub agent: AgentMerge,
    /// request_id → meeting id at forwarding (`""` for none), `stale:`-prefixed once
    /// that meeting ended.
    pub pending: IndexMap<String, String>,
    pub transcript: TranscriptBuffer,
    pub tick: Option<TickState>,
    /// `_last_seq`: an int or float, as the frames gave it.
    pub last_seq: Value,
    pub clock_offset: f64,
    pub pcm_drop_log_ts: HashMap<String, f64>,
    pub meeting_owner: Option<ClientId>,
    pub golden_clients: u64,
    pub last_debug_update: f64,
    pub running: bool,
}

#[derive(Default, Clone)]
pub(crate) struct Wiring {
    pub storage: Option<StoreHandle>,
    pub live: Option<Arc<dyn LiveEvents>>,
    pub capture: Option<Arc<dyn MeetingCapture>>,
    pub identity: Option<Arc<dyn IdentitySource>>,
    pub increments: Option<Arc<dyn IncrementGenerator>>,
    pub golden: Option<Arc<dyn GoldenTap>>,
}

/// A transcript line for the extension (`send_transcript`'s arguments).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptLine {
    pub speaker: String,
    pub text: String,
    pub timestamp: String,
    pub is_continuation: bool,
    /// True: the microphone (the user); false: system audio (the others).
    pub is_local: bool,
    pub uncertain: bool,
    /// `"local"`, `"mics_off"` or empty.
    pub uncertain_reason: String,
    /// A retroactive correction of an Unknown segment.
    pub correction: bool,
    /// Positive: the bubble id a later `transcript_refine` addresses.
    pub segment_id: i64,
    /// Non-empty: the stable source (a per-pid PCM stream), so continuations of
    /// different sources do not merge.
    pub source_id: String,
}

impl TranscriptLine {
    /// The `transcript` frame, tagged with `meeting_id`; the segment and source ids
    /// only when set.
    fn frame(&self, meeting_id: Option<&str>) -> Value {
        let mut map = Map::new();
        map.insert("type", Value::from("transcript"));
        map.insert("speaker", Value::from(self.speaker.as_str()));
        map.insert("text", Value::from(self.text.as_str()));
        map.insert("timestamp", Value::from(self.timestamp.as_str()));
        map.insert("is_continuation", Value::from(self.is_continuation));
        map.insert("is_local", Value::from(self.is_local));
        map.insert("uncertain", Value::from(self.uncertain));
        map.insert("uncertain_reason", Value::from(self.uncertain_reason.as_str()));
        map.insert("correction", Value::from(self.correction));
        map.insert("meeting_id", Value::from(meeting_id));
        if self.segment_id > 0 {
            map.insert("segment_id", Value::from(self.segment_id));
        }
        if !self.source_id.is_empty() {
            map.insert("source_id", Value::from(self.source_id.as_str()));
        }
        Value::Object(map)
    }
}

/// One refined segment (`send_refine`'s `updates`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefineUpdate {
    pub segment_id: i64,
    pub text: String,
}

/// The bridge (`KTalkBridge`).
pub struct Bridge {
    pub(crate) config: BridgeConfig,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) log: Logger,
    pub(crate) policy_source: Arc<dyn PolicySource>,
    pub(crate) state: Mutex<State>,
    pub(crate) wiring: Mutex<Wiring>,
    pub(crate) manual_lock: tokio::sync::Mutex<()>,
    pub(crate) profile_lock: tokio::sync::Mutex<()>,
    pub(crate) settings_lock: tokio::sync::Mutex<()>,
    pub(crate) runtime: Mutex<Option<Handle>>,
    pub(crate) background: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) next_client: AtomicU64,
    pub(crate) rate_limits: Mutex<HashMap<&'static str, (Option<std::time::Instant>, u64)>>,
    pub(crate) thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub(crate) stop_signal: tokio::sync::Notify,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Bridge {
    /// `KTalkBridge(...)`: the policy is read once now, as Python's constructor does.
    ///
    /// # Errors
    /// The policy cannot be read.
    pub fn new(config: BridgeConfig, policy_source: Arc<dyn PolicySource>) -> Result<Arc<Self>, String> {
        Self::with_clock(config, policy_source, Arc::new(SystemClock), Arc::new(|line: &str| eprintln!("{line}")))
    }

    /// [`Bridge::new`] with a clock and a log sink.
    ///
    /// # Errors
    /// The policy cannot be read.
    pub fn with_clock(
        config: BridgeConfig,
        policy_source: Arc<dyn PolicySource>,
        clock: Arc<dyn Clock>,
        log: Logger,
    ) -> Result<Arc<Self>, String> {
        let policy = policy_source.read_policy()?;
        let unconfigured = Profile {
            version: 1,
            revision: "unconfigured".to_owned(),
            name: String::new(),
            aliases: Vec::new(),
            configured: false,
        };
        let identity = resolve_identity(&unconfigured, Some(&config.legacy_mic_name), None);
        let state = State {
            speakers: Speakers::default(),
            clients: Clients::default(),
            identity,
            policy,
            manual_copilot_active: false,
            agent: AgentMerge::default(),
            pending: IndexMap::new(),
            transcript: TranscriptBuffer::default(),
            tick: None,
            last_seq: Value::Int(0),
            clock_offset: 0.0,
            pcm_drop_log_ts: HashMap::new(),
            meeting_owner: None,
            golden_clients: 0,
            last_debug_update: 0.0,
            running: false,
        };
        Ok(Arc::new(Self {
            config,
            clock,
            log,
            policy_source,
            state: Mutex::new(state),
            wiring: Mutex::new(Wiring::default()),
            manual_lock: tokio::sync::Mutex::new(()),
            profile_lock: tokio::sync::Mutex::new(()),
            settings_lock: tokio::sync::Mutex::new(()),
            runtime: Mutex::new(None),
            background: Mutex::new(Vec::new()),
            next_client: AtomicU64::new(1),
            rate_limits: Mutex::new(HashMap::new()),
            thread: Mutex::new(None),
            stop_signal: tokio::sync::Notify::new(),
        }))
    }

    pub(crate) fn st(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub(crate) fn wired(&self) -> Wiring {
        lock(&self.wiring).clone()
    }

    pub(crate) fn log(&self, line: &str) {
        (self.log)(line);
    }

    #[must_use]
    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    // ---- wiring (the Python setters) ----

    pub fn set_meeting_storage(&self, storage: Arc<dyn MeetingStore>) {
        lock(&self.wiring).storage = Some(StoreHandle::new(storage));
    }

    /// The speaker, segment, meeting-switch and PCM callbacks.
    pub fn set_live_events(&self, live: Option<Arc<dyn LiveEvents>>) {
        lock(&self.wiring).live = live;
    }

    /// `set_meeting_capture_callback(callback, active_getter=)`.
    pub fn set_meeting_capture(&self, capture: Arc<dyn MeetingCapture>) {
        lock(&self.wiring).capture = Some(capture);
    }

    pub fn set_increment_generator(&self, generator: Arc<dyn IncrementGenerator>) {
        lock(&self.wiring).increments = Some(generator);
    }

    /// `bridge.golden = recorder` (None stops recording).
    pub fn set_golden(&self, golden: Option<Arc<dyn GoldenTap>>) {
        lock(&self.wiring).golden = golden;
    }

    /// `configure_user_profile(identity, read_identity, apply_mic)`.
    ///
    /// # Errors
    /// The identity has no `local_user_aliases` (Python's `KeyError`).
    pub fn configure_user_profile(&self, identity: &Value, source: Arc<dyn IdentitySource>) -> Result<(), String> {
        let identity = copy_identity(identity)?;
        self.st().identity = identity;
        lock(&self.wiring).identity = Some(source);
        Ok(())
    }

    /// Lets sends from other threads reach the clients (Python's `self._loop`); set
    /// by the server, or by a test running handlers on its own runtime.
    pub fn attach_runtime(&self, handle: Handle) {
        *lock(&self.runtime) = Some(handle);
    }

    // ---- queries of the live process ----

    pub fn now(&self) -> f64 {
        self.clock.time()
    }

    /// `get_current_speaker()`.
    #[must_use]
    pub fn get_current_speaker(&self) -> Option<String> {
        let now = self.now();
        self.st().speakers.current_speaker(now)
    }

    /// `get_dominant_speaker_for_period(start_time, end_time)`.
    #[must_use]
    pub fn get_dominant_speaker_for_period(&self, start_time: f64, end_time: f64) -> Option<String> {
        let now = self.now();
        self.st().speakers.dominant_speaker(start_time, end_time, now)
    }

    #[must_use]
    pub fn is_someone_speaking(&self) -> bool {
        self.st().speakers.is_speaking
    }

    #[must_use]
    pub fn is_current_speaker_local(&self) -> bool {
        let now = self.now();
        self.st().speakers.is_current_speaker_local(now)
    }

    #[must_use]
    pub fn get_participants(&self) -> IndexMap<String, Participant> {
        self.st().speakers.participants.clone()
    }

    #[must_use]
    pub fn are_all_mics_muted(&self) -> bool {
        self.st().speakers.are_all_mics_muted()
    }

    /// `is_local_mic_muted()`: `localMicMuted` of the source, else the local
    /// participant, else open.
    #[must_use]
    pub fn is_local_mic_muted(&self) -> bool {
        self.st().speakers.is_local_mic_muted()
    }

    #[must_use]
    pub fn is_speaker_muted(&self, speaker_name: &str) -> bool {
        self.st().speakers.is_speaker_muted(speaker_name)
    }

    #[must_use]
    pub fn has_remote_participants(&self) -> bool {
        self.st().speakers.has_remote_participants()
    }

    #[must_use]
    pub fn get_sole_unmuted_remote_participant(&self) -> Option<String> {
        self.st().speakers.sole_unmuted_remote_participant()
    }

    #[must_use]
    pub fn get_speaker_history(&self) -> Vec<SpeakerEvent> {
        self.st().speakers.history.clone()
    }

    /// `get_completed_segment()`: the oldest finished segment, removed.
    pub fn get_completed_segment(&self) -> Option<SpeakerSegment> {
        let mut st = self.st();
        (!st.speakers.completed_segments.is_empty()).then(|| st.speakers.completed_segments.remove(0))
    }

    pub fn get_all_completed_segments(&self) -> Vec<SpeakerSegment> {
        std::mem::take(&mut self.st().speakers.completed_segments)
    }

    /// `user_identity_snapshot()`.
    #[must_use]
    pub fn user_identity_snapshot(&self) -> Map {
        self.st().identity.as_object().cloned().unwrap_or_default()
    }

    /// `copilot_policy_snapshot()`.
    #[must_use]
    pub fn copilot_policy_snapshot(&self) -> Map {
        self.st().policy.to_map()
    }

    pub(crate) fn policy_enabled(&self) -> bool {
        self.st().policy.enabled.truthy()
    }

    /// `is_recording_active()`: the capture owner's state, else "always" unless
    /// `ktalk_only`.
    #[must_use]
    pub fn is_recording_active(&self) -> bool {
        match self.wired().capture {
            Some(capture) => capture.recording_active(),
            None => !self.config.recording_policy.gated(),
        }
    }

    pub fn set_meeting_capture_active(&self, active: bool) -> Result<(), String> {
        match self.wired().capture {
            Some(capture) => capture.set_capture(active),
            None if active && self.config.recording_policy.gated() => {
                Err("No capture controller registered".to_owned())
            }
            None => Ok(()),
        }
    }

    /// The manual mirror, read for the supervisor's reconcile.
    #[must_use]
    pub fn manual_copilot_active(&self) -> bool {
        self.st().manual_copilot_active
    }

    /// `_set_manual_copilot_active`: a change restarts the sessions-zero debounce.
    pub(crate) fn set_manual_copilot_active(&self, active: bool) {
        let mut st = self.st();
        if st.manual_copilot_active != active {
            st.manual_copilot_active = active;
            st.agent.sessions_zero_since = None;
        }
    }

    // ---- meeting state ----

    pub(crate) fn storage(&self) -> Option<StoreHandle> {
        lock(&self.wiring).storage.clone()
    }

    /// `meta and is_conference_source(meta.get("source"))`; a meta.json that is not
    /// an object counts as no conference (Python would raise).
    pub(crate) fn is_conference_meta(storage: &StoreHandle, meta: &Value) -> bool {
        if !meta.truthy() {
            return false;
        }
        match meta.as_object().and_then(|m| m.get("source")) {
            Some(Value::Str(s)) => storage.is_conference_source(Some(s)),
            Some(Value::Null) | None => storage.is_conference_source(None),
            Some(_) => false,
        }
    }

    /// `_is_meeting_active()`: the current meeting comes from a conference AND an
    /// extension is connected. Not `current_meeting_path is not None`: that is never
    /// None, a leave opens an orphan at once.
    #[must_use]
    pub fn is_conference_active(&self) -> bool {
        self.meeting_active_logged("_load_meta failed computing meeting-active")
    }

    pub(crate) fn meeting_active_logged(&self, failure: &str) -> bool {
        let Some(storage) = self.storage() else { return false };
        if storage.current_meeting_path().is_none() {
            return false;
        }
        let meta = match storage.load_meta() {
            Ok(meta) => meta,
            Err(e) => {
                self.log(&format!("[KTalk Bridge] {failure}: {e}"));
                return false;
            }
        };
        if !Self::is_conference_meta(&storage, &meta) {
            return false;
        }
        !self.st().clients.connected.is_empty()
    }

    /// `_has_active_ktalk_meeting()`: the PCM gate, without the extension check.
    pub fn has_active_conference_meeting(&self) -> bool {
        let Some(storage) = self.storage() else { return false };
        if storage.current_meeting_path().is_none() {
            return false;
        }
        match storage.load_meta() {
            Ok(meta) => Self::is_conference_meta(&storage, &meta),
            Err(e) => {
                self.log(&format!("[KTalk Bridge] pcm_chunk meeting guard failed: {e}"));
                false
            }
        }
    }

    pub fn copilot_should_be_alive(&self) -> bool {
        self.is_conference_active() || self.st().manual_copilot_active
    }

    /// `_compute_meeting_state()`: the `meeting_state` reply to `supervisor_hello`.
    #[must_use]
    pub fn compute_meeting_state(&self) -> Value {
        let mut active = self.is_conference_active();
        let mut meeting_id = Value::Null;
        let mut meeting_title = Value::Null;
        if active {
            if let Some(storage) = self.storage() {
                meeting_id = Value::from(storage.current_meeting_id());
                meeting_title = storage
                    .load_meta()
                    .ok()
                    .and_then(|m| m.as_object().map(|o| o.get("title").cloned().unwrap_or(Value::Null)))
                    .unwrap_or(Value::Null);
            }
        } else if self.st().manual_copilot_active {
            active = true;
            meeting_id = Value::from(crate::copilot::MANUAL_MEETING_ID);
        }
        obj! {"type" => protocol::MEETING_STATE, "active" => active, "meeting_id" => meeting_id, "meeting_title" => meeting_title}
    }

    pub(crate) fn current_meeting_id(&self) -> Option<String> {
        self.storage().and_then(|s| s.current_meeting_id())
    }

    pub(crate) fn clear_speaker_state(&self) {
        self.st().speakers.clear();
    }

    // ---- sends from the live process ----

    fn spawn_on_loop(&self, future: impl std::future::Future<Output = ()> + Send + 'static) -> bool {
        let handle = lock(&self.runtime).clone();
        match handle {
            Some(handle) => {
                handle.spawn(future);
                true
            }
            None => false,
        }
    }

    /// `send_transcript(...)`: the line to every extension, tagged with the current
    /// meeting so a tab of another meeting drops it; a line with a segment id also
    /// enters the recent-transcript buffer and may fire a `transcript_tick`.
    pub fn send_transcript(self: &Arc<Self>, line: &TranscriptLine) {
        let meeting_id = self.current_meeting_id();
        let message = dumps_ascii(&line.frame(meeting_id.as_deref()));
        self.send_on_loop(&message, |st| st.clients.list(&st.clients.connected));
        if line.segment_id > 0 {
            let meeting_id = self.current_meeting_id();
            let now_ms = self.now() * 1000.0;
            self.st().transcript.record(line.segment_id, &line.speaker, &line.text, meeting_id.as_deref(), now_ms);
            self.maybe_emit_transcript_tick();
        }
    }

    /// Sends `message` to the clients `recipients` picks, each on the loop; nothing
    /// without a running loop.
    fn send_on_loop(&self, message: &str, recipients: impl FnOnce(&State) -> Vec<Arc<Client>>) {
        if lock(&self.runtime).is_none() {
            return;
        }
        let recipients = recipients(&self.st());
        for client in recipients {
            let message = message.to_owned();
            self.spawn_on_loop(async move {
                let _ = client.sink.send(message).await;
            });
        }
    }

    /// `_maybe_emit_transcript_tick`: to channel clients after enough new segments or
    /// time, while a conference runs and the Copilot is on.
    fn maybe_emit_transcript_tick(self: &Arc<Self>) {
        let (frame, recipients) = {
            let mut st = self.st();
            let Some(frame) = self.next_tick_frame(&mut st) else { return };
            (frame, st.clients.list(&st.clients.channels))
        };
        let Some(handle) = lock(&self.runtime).clone() else { return };
        for client in recipients {
            let bridge = self.clone();
            let frame = frame.clone();
            handle.spawn(async move {
                // Snapshot identity and policy on the loop, so a tick queued before
                // a refresh cannot bring back the old aliases.
                let Some(base) = frame.as_object() else { return };
                let merged = json::merged(
                    &json::merged(base, &bridge.user_identity_snapshot()),
                    &bridge.copilot_policy_snapshot(),
                );
                let _ = bridge.send_bounded(&client, &dumps(&Value::Object(merged))).await;
            });
        }
    }

    /// Counts one more segment and answers the `transcript_tick` frame when one is
    /// due; None while the Copilot is off, no meeting ticks or no channel listens.
    fn next_tick_frame(&self, st: &mut State) -> Option<Value> {
        if !st.policy.enabled.truthy() || st.tick.is_none() || st.clients.channels.is_empty() {
            return None;
        }
        let now = self.clock.time();
        let tick = st.tick.as_mut().expect("checked above");
        tick.seg_count += 1;
        let new_segs = tick.seg_count - tick.last_seg_count;
        let time_trigger = now - tick.last_emit_ts >= COPILOT_TICK_INTERVAL_SECONDS;
        if !(new_segs >= COPILOT_TICK_NEW_SEGMENTS || time_trigger) {
            return None;
        }
        let meeting_id = tick.meeting_id.clone();
        let title = if tick.meeting_title.truthy() { tick.meeting_title.clone() } else { Value::from("") };
        tick.last_seg_count = tick.seg_count;
        tick.last_emit_ts = now;
        let new_segments = st.transcript.tail(usize::try_from(new_segs).unwrap_or(0), meeting_id.as_deref());
        let recent = st.transcript.tail(COPILOT_TICK_RECENT_SEGMENTS, meeting_id.as_deref());
        Some(tick_frame(meeting_id.as_deref(), title, new_segments, recent))
    }

    /// `send_refine(updates, meeting_id)`: tier-2 text to the extensions and the
    /// subscribers; the buffer takes it for entries of that meeting.
    pub fn send_refine(&self, updates: &[RefineUpdate], meeting_id: Option<&str>) {
        if updates.is_empty() {
            return;
        }
        let refined_at = local_isoformat(self.clock.as_ref(), self.now());
        let message = dumps(&refine_envelope(updates, refined_at, meeting_id));
        self.send_on_loop(&message, |st| {
            let mut list = st.clients.list(&st.clients.connected);
            list.extend(st.clients.list(&st.clients.subscribers));
            list
        });
        let mut st = self.st();
        for u in updates {
            st.transcript.refine(u.segment_id, &u.text, meeting_id);
        }
    }

    // ---- fan-out ----

    /// `_send_bounded`: one send bounded by `send_timeout`. A timeout marks the
    /// client (later broadcasts skip it), schedules its close and answers false; the
    /// role sets keep it until its handler reaps the socket, so the role's disconnect
    /// cleanup still runs. Other send errors are returned, for the caller to log.
    pub async fn send_bounded(&self, client: &Arc<Client>, message: &str) -> Result<bool, SendError> {
        if self.st().clients.timed_out.contains(&client.id) {
            return Ok(false);
        }
        match tokio::time::timeout(self.config.send_timeout, client.sink.send(message.to_owned())).await {
            Ok(Ok(())) => Ok(true),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                let first = {
                    let mut st = self.st();
                    let first = st.clients.registered(client.id) && !st.clients.timed_out.contains(&client.id);
                    if first {
                        st.clients.timed_out.insert(client.id);
                    }
                    first
                };
                if !first {
                    return Ok(false);
                }
                self.log(&format!(
                    "[KTalk Bridge] Send timed out after {}s — closing stalled client",
                    recog_core::py::float_repr(self.config.send_timeout.as_secs_f64())
                ));
                let close = client.sink.close(1000, String::new());
                let log = self.log.clone();
                tokio::spawn(async move {
                    if let Err(e) = close.await {
                        log(&format!("[KTalk Bridge] Failed to close stalled client: {e}"));
                    }
                });
                Ok(false)
            }
        }
    }

    async fn fan_out(&self, recipients: Vec<Arc<Client>>, message: &str, on_error: impl Fn(&SendError)) {
        let sends = recipients.iter().map(|client| self.send_bounded(client, message));
        for result in join_all(sends).await {
            if let Err(e) = result {
                on_error(&e);
            }
        }
    }

    /// `_broadcast_message`: extensions only (transcript, lifecycle, meeting frames).
    pub async fn broadcast_message(&self, data: &Value) {
        let recipients = {
            let st = self.st();
            st.clients.list(&st.clients.connected)
        };
        if recipients.is_empty() {
            return;
        }
        self.fan_out(recipients, &dumps(data), |_| {}).await;
    }

    /// `_broadcast_copilot_frame`: extensions and subscribers, even with no extension.
    pub async fn broadcast_copilot_frame(&self, data: &Value) {
        let recipients = {
            let st = self.st();
            let mut list = st.clients.list(&st.clients.connected);
            list.extend(st.clients.list(&st.clients.subscribers));
            list
        };
        if recipients.is_empty() {
            return;
        }
        self.fan_out(recipients, &dumps(data), |_| {}).await;
    }

    fn frame_type(data: &Value) -> String {
        data.as_object().and_then(|m| m.get("type")).map_or_else(|| "None".to_owned(), json::py_str)
    }

    /// `_broadcast_to_channel_clients`.
    pub async fn broadcast_to_channel_clients(&self, data: &Value) {
        let recipients = {
            let st = self.st();
            st.clients.list(&st.clients.channels)
        };
        if recipients.is_empty() {
            return;
        }
        let kind = Self::frame_type(data);
        self.fan_out(recipients, &dumps(data), |e| {
            self.log(&format!("[KTalk Bridge] Failed to send {kind} to channel client: {e}"));
        })
        .await;
    }

    /// `_broadcast_to_subscribers`: the webapp only.
    pub async fn broadcast_to_subscribers(&self, data: &Value) {
        let recipients = {
            let st = self.st();
            st.clients.list(&st.clients.subscribers)
        };
        if recipients.is_empty() {
            return;
        }
        let kind = Self::frame_type(data);
        self.fan_out(recipients, &dumps(data), |e| {
            self.log(&format!("[KTalk Bridge] Failed to send {kind} to subscriber: {e}"));
        })
        .await;
    }

    /// `_emit_lifecycle`: `meeting_started`/`meeting_ended` to the channel (when the
    /// Copilot is on), the extensions and the supervisor, one fan-out point so no
    /// emit site can skip the supervisor.
    pub async fn emit_lifecycle(&self, frame: Value) {
        let frame = if Self::frame_type(&frame) == protocol::MEETING_STARTED {
            let base = frame.as_object().cloned().unwrap_or_default();
            Value::Object(json::merged(
                &json::merged(&base, &self.user_identity_snapshot()),
                &self.copilot_policy_snapshot(),
            ))
        } else {
            frame
        };
        if self.policy_enabled() {
            self.broadcast_to_channel_clients(&frame).await;
        }
        self.broadcast_message(&frame).await;
        let supervisors = {
            let st = self.st();
            st.clients.list(&st.clients.supervisors)
        };
        if supervisors.is_empty() {
            return;
        }
        let kind = Self::frame_type(&frame);
        self.fan_out(supervisors, &dumps(&frame), |e| {
            self.log(&format!("[KTalk Bridge] Failed to send {kind} to supervisor: {e}"));
        })
        .await;
    }

    /// `_relay_to_supervisors`: true only when a supervisor actually got the frame. A
    /// spawn stops being relayed once a conference has started.
    pub async fn relay_to_supervisors(&self, frame: &Value) -> bool {
        let supervisors = {
            let st = self.st();
            st.clients.list(&st.clients.supervisors)
        };
        if supervisors.is_empty() {
            return false;
        }
        let message = dumps(frame);
        let kind = Self::frame_type(frame);
        let mut delivered = false;
        for client in supervisors {
            if kind == protocol::COPILOT_SPAWN_REQUEST && self.is_conference_active() {
                break;
            }
            match self.send_bounded(&client, &message).await {
                Ok(true) => delivered = true,
                Ok(false) => {}
                Err(e) => self.log(&format!("[KTalk Bridge] Failed to send {kind} to supervisor: {e}")),
            }
        }
        delivered
    }

    /// `_reply_copilot_status`: a per-request status to its originator only.
    pub async fn reply_copilot_status(
        &self,
        client: Option<&Arc<Client>>,
        request_id: &str,
        status: &str,
        message: &str,
    ) {
        let Some(client) = client else {
            self.log(&format!(
                "[KTalk Bridge] {status} status for request_id={request_id} dropped — no originator socket"
            ));
            return;
        };
        let frame = obj! {"type" => protocol::COPILOT_STATUS, "request_id" => request_id, "status" => status, "message" => message};
        if let Err(e) = client.sink.send(dumps(&frame)).await {
            self.log(&format!("[KTalk Bridge] Failed to send {status} status: {e}"));
        }
    }

    /// Runs a background task (increment generation), kept until it ends so shutdown
    /// can wait for it.
    pub(crate) fn spawn_background(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut tasks = lock(&self.background);
        tasks.retain(|t| !t.is_finished());
        tasks.push(tokio::spawn(future));
    }

    /// `_safe_generate_increment` for a meeting just left.
    pub(crate) fn start_increments(&self, meeting_id: Option<&str>, meeting_path: Option<PathBuf>) {
        let Some(generator) = self.wired().increments else { return };
        let (Some(id), Some(path)) = (meeting_id.filter(|m| !m.is_empty()), meeting_path) else { return };
        let log = self.log.clone();
        let future = generator.generate_all(id.to_owned(), path);
        self.spawn_background(async move {
            if let Err(e) = future.await {
                log(&format!("[KTalk Bridge] increment generation error: {e}"));
            }
        });
    }

    /// `log_rate_limited(key, msg)`: once per 30 s per key, with the count suppressed
    /// since, on the real monotonic clock as Python's default.
    pub(crate) fn log_rate_limited(&self, key: &'static str, message: &str) {
        let now = std::time::Instant::now();
        let suppressed = {
            let mut limits = lock(&self.rate_limits);
            let entry = limits.entry(key).or_insert((None, 0));
            if let Some(last) = entry.0
                && now.duration_since(last) < Duration::from_secs(30)
            {
                entry.1 += 1;
                return;
            }
            let suppressed = entry.1;
            *entry = (Some(now), 0);
            suppressed
        };
        if suppressed > 0 {
            self.log(&format!("{message} (+{suppressed} suppressed)"));
        } else {
            self.log(message);
        }
    }

    pub(crate) fn next_client_id(&self) -> ClientId {
        ClientId(self.next_client.fetch_add(1, Ordering::SeqCst))
    }
}

/// `{**identity, "local_user_aliases": list(identity["local_user_aliases"])}`.
pub(crate) fn copy_identity(identity: &Value) -> Result<Value, String> {
    let Some(map) = identity.as_object() else {
        return Err(format!("'{}' object is not a mapping", identity.type_name()));
    };
    let aliases = map.get("local_user_aliases").ok_or_else(|| "'local_user_aliases'".to_owned())?;
    let mut copy = map.clone();
    copy.insert("local_user_aliases", Value::Array(json::iterate(aliases)?));
    Ok(Value::Object(copy))
}

/// The `transcript_tick` frame of a meeting: the new segments, the recent ones and
/// those as `speaker: text` lines.
fn tick_frame(meeting_id: Option<&str>, title: Value, new_segments: Vec<Map>, recent: Vec<Map>) -> Value {
    let context = recent_context(&recent);
    obj! {
        "type" => protocol::TRANSCRIPT_TICK,
        "meeting_id" => meeting_id,
        "meeting_title" => title,
        "new_segments" => Value::Array(new_segments.into_iter().map(Value::Object).collect()),
        "recent_context" => context,
        "recent_segments" => Value::Array(recent.into_iter().map(Value::Object).collect()),
    }
}

/// `speaker: text` per entry, `?` for a missing speaker.
fn recent_context(recent: &[Map]) -> String {
    recent
        .iter()
        .map(|e| {
            let field = |k: &str| e.get(k).filter(|v| v.truthy()).map(json::py_str);
            format!("{}: {}", field("speaker").unwrap_or_else(|| "?".to_owned()), field("text").unwrap_or_default())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `transcript_refine` frame; `meeting_id` only when one is given.
fn refine_envelope(updates: &[RefineUpdate], refined_at: String, meeting_id: Option<&str>) -> Value {
    let payload_updates: Vec<Value> =
        updates.iter().map(|u| obj! {"segment_id" => u.segment_id, "text" => u.text.as_str()}).collect();
    let mut envelope = obj! {"type" => protocol::TRANSCRIPT_REFINE, "updates" => Value::Array(payload_updates), "refined_at" => refined_at};
    if let (Some(mid), Some(map)) = (meeting_id.filter(|m| !m.is_empty()), envelope.as_object_mut()) {
        map.insert("meeting_id", Value::from(mid));
    }
    envelope
}
