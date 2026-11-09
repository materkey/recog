//! Test doubles of the bridge's surroundings, for the crate's tests and those of the
//! crates that drive a bridge: the counterparts of the Python tests' `FakeWebSocket`,
//! `Mock()` storage and recording callbacks.
//!
//! Everything here records what the bridge did to it, as JSON values, so a test can
//! compare it with what it expects.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use futures_util::Stream;
use recog_core::pyjson::{Map, Value};

use crate::client::{ClientSink, Connection, Inbound, SendError};
use crate::hooks::{
    BoxFuture, Clock, CopilotPolicy, GoldenTap, IdentitySource, IncrementGenerator, LiveEvents, MeetingCapture,
    MeetingStore, PcmChunk, PolicySource, SpeakerSegment,
};
use crate::obj;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a [`RecordingSink`] answers a send.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SendMode {
    /// Records the text and succeeds.
    #[default]
    Record,
    /// Never resolves: a peer that stopped reading.
    Stall,
    /// Fails with this text (a dead socket).
    Fail(String),
}

/// A sink that keeps what it was sent.
#[derive(Default)]
pub struct RecordingSink {
    pub sent: Mutex<Vec<String>>,
    pub closes: Mutex<Vec<(u16, String)>>,
    pub mode: Mutex<SendMode>,
    pub close_error: Mutex<Option<String>>,
}

impl RecordingSink {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    #[must_use]
    pub fn with_mode(mode: SendMode) -> Arc<Self> {
        let sink = Self::new();
        *lock(&sink.mode) = mode;
        sink
    }

    pub fn set_mode(&self, mode: SendMode) {
        *lock(&self.mode) = mode;
    }

    /// The sent frames, parsed.
    #[must_use]
    pub fn frames(&self) -> Vec<Value> {
        lock(&self.sent).iter().map(|t| recog_core::pyjson::loads(t).unwrap_or(Value::Null)).collect()
    }

    /// The sent frames of one `type`.
    #[must_use]
    pub fn of_type(&self, kind: &str) -> Vec<Value> {
        self.frames().into_iter().filter(|f| frame_type(f) == Some(kind)).collect()
    }

    /// Clears what was recorded.
    pub fn clear(&self) {
        lock(&self.sent).clear();
    }
}

/// The `type` of a frame.
#[must_use]
pub fn frame_type(frame: &Value) -> Option<&str> {
    frame.as_object().and_then(|m| m.get("type")).and_then(Value::as_str)
}

impl ClientSink for RecordingSink {
    fn send(&self, text: String) -> BoxFuture<Result<(), SendError>> {
        match lock(&self.mode).clone() {
            SendMode::Record => {
                lock(&self.sent).push(text);
                Box::pin(async { Ok(()) })
            }
            SendMode::Stall => Box::pin(futures_util::future::pending()),
            SendMode::Fail(e) => Box::pin(async move { Err(SendError(e)) }),
        }
    }

    fn close(&self, code: u16, reason: String) -> BoxFuture<Result<(), SendError>> {
        lock(&self.closes).push((code, reason));
        let error = lock(&self.close_error).clone();
        Box::pin(async move { error.map_or(Ok(()), |e| Err(SendError(e))) })
    }
}

#[derive(Default)]
struct QueueState {
    items: VecDeque<Result<Inbound, String>>,
    closed: bool,
    idle: bool,
    waker: Option<Waker>,
}

/// The receiving side of a fake connection, fed by its [`FakeConnection`].
#[derive(Default)]
struct Queue {
    state: Mutex<QueueState>,
    idle: tokio::sync::Notify,
}

struct QueueStream(Arc<Queue>);

impl Stream for QueueStream {
    type Item = Result<Inbound, String>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut state = lock(&self.0.state);
        if let Some(item) = state.items.pop_front() {
            state.idle = false;
            return Poll::Ready(Some(item));
        }
        if state.closed {
            state.idle = true;
            drop(state);
            self.0.idle.notify_waiters();
            return Poll::Ready(None);
        }
        state.idle = true;
        state.waker = Some(cx.waker().clone());
        drop(state);
        self.0.idle.notify_waiters();
        Poll::Pending
    }
}

/// A connection driven by a test: messages pushed in, the sink recording out. Like
/// the Python `ControllableFakeWebSocket`, it stays open until closed.
pub struct FakeConnection {
    pub sink: Arc<RecordingSink>,
    queue: Arc<Queue>,
}

impl FakeConnection {
    /// A connection and the [`Connection`] to hand the bridge.
    #[must_use]
    pub fn new(remote_address: &str, origins: Option<Vec<String>>) -> (Self, Connection) {
        Self::with_sink(RecordingSink::new(), remote_address, origins)
    }

    #[must_use]
    pub fn with_sink(
        sink: Arc<RecordingSink>,
        remote_address: &str,
        origins: Option<Vec<String>>,
    ) -> (Self, Connection) {
        let queue = Arc::new(Queue::default());
        let connection = Connection {
            sink: sink.clone(),
            inbound: Box::pin(QueueStream(queue.clone())),
            remote_address: remote_address.to_owned(),
            origins,
        };
        (Self { sink, queue }, connection)
    }

    fn push(&self, item: Result<Inbound, String>) {
        let mut state = lock(&self.queue.state);
        state.items.push_back(item);
        state.idle = false;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// A text frame.
    pub fn send_text(&self, text: &str) {
        self.push(Ok(Inbound::Text(text.to_owned())));
    }

    /// A frame as JSON.
    pub fn send(&self, frame: &Value) {
        self.send_text(&crate::json::dumps(frame));
    }

    /// The peer's error ends the stream (`ConnectionClosedError`).
    pub fn fail(&self, message: &str) {
        self.push(Err(message.to_owned()));
    }

    /// A clean close.
    pub fn close(&self) {
        let mut state = lock(&self.queue.state);
        state.closed = true;
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }

    /// Waits until the handler asked for the next message with none queued, i.e.
    /// it has processed everything sent so far.
    pub async fn idle(&self) {
        loop {
            let notified = self.queue.idle.notified();
            {
                let state = lock(&self.queue.state);
                if state.idle && state.items.is_empty() {
                    return;
                }
            }
            notified.await;
        }
    }
}

/// A clock the test sets.
#[derive(Debug, Default)]
pub struct ManualClock {
    micros: AtomicI64,
    monotonic_micros: AtomicI64,
    /// The local zone's offset in seconds.
    pub offset_s: AtomicI64,
}

impl ManualClock {
    #[must_use]
    pub fn at(seconds: f64) -> Arc<Self> {
        let clock = Arc::new(Self::default());
        clock.set(seconds);
        clock
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn set(&self, seconds: f64) {
        self.micros.store((seconds * 1e6).round() as i64, Ordering::SeqCst);
        self.monotonic_micros.store((seconds * 1e6).round() as i64, Ordering::SeqCst);
    }

    pub fn set_micros(&self, micros: i64) {
        self.micros.store(micros, Ordering::SeqCst);
        self.monotonic_micros.store(micros, Ordering::SeqCst);
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn advance(&self, seconds: f64) {
        let delta = (seconds * 1e6).round() as i64;
        self.micros.fetch_add(delta, Ordering::SeqCst);
        self.monotonic_micros.fetch_add(delta, Ordering::SeqCst);
    }

    #[must_use]
    pub fn micros(&self) -> i64 {
        self.micros.load(Ordering::SeqCst)
    }
}

impl Clock for ManualClock {
    #[allow(clippy::cast_precision_loss)]
    fn time(&self) -> f64 {
        self.micros.load(Ordering::SeqCst) as f64 / 1e6
    }
    #[allow(clippy::cast_precision_loss)]
    fn monotonic(&self) -> f64 {
        self.monotonic_micros.load(Ordering::SeqCst) as f64 / 1e6
    }
    fn utc_offset(&self, _utc_micros: i64) -> i64 {
        self.offset_s.load(Ordering::SeqCst)
    }
}

impl recog_core::time::Clock for ManualClock {
    fn now_micros(&self) -> i64 {
        self.micros()
    }
    fn utc_offset(&self, _utc_micros: i64) -> i64 {
        self.offset_s.load(Ordering::SeqCst)
    }
}

/// A policy the test switches.
pub struct SwitchablePolicy(pub Mutex<Result<CopilotPolicy, String>>);

impl SwitchablePolicy {
    #[must_use]
    pub fn enabled() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Ok(default_policy()))))
    }

    pub fn set(&self, policy: Result<CopilotPolicy, String>) {
        *lock(&self.0) = policy;
    }

    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(policy) = lock(&self.0).as_mut() {
            policy.enabled = Value::Bool(enabled);
        }
    }
}

impl PolicySource for SwitchablePolicy {
    fn read_policy(&self) -> Result<CopilotPolicy, String> {
        lock(&self.0).clone()
    }
}

/// A policy with the Copilot on.
#[must_use]
pub fn default_policy() -> CopilotPolicy {
    CopilotPolicy { instructions: Value::from("Be brief."), revision: Value::from("r1"), enabled: Value::Bool(true) }
}

/// Records every call the bridge makes into the live process, in order.
#[derive(Default)]
pub struct RecordingLive {
    pub calls: Mutex<Vec<Value>>,
    pub wants_pcm: AtomicBool,
    pub pcm_error: Mutex<Option<String>>,
    pub meeting_change_error: Mutex<Option<String>>,
    pub stats: Mutex<Option<Result<Map, String>>>,
    /// The storage root, so recorded paths are relative.
    pub root: Mutex<Option<PathBuf>>,
}

impl RecordingLive {
    #[must_use]
    pub fn new() -> Arc<Self> {
        let live = Arc::new(Self::default());
        live.wants_pcm.store(true, Ordering::SeqCst);
        live
    }

    fn record(&self, call: Value) {
        lock(&self.calls).push(call);
    }

    #[must_use]
    pub fn calls(&self) -> Vec<Value> {
        lock(&self.calls).clone()
    }

    /// The names of the calls.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.calls().iter().filter_map(|c| c.as_object()?.get("call")?.as_str().map(str::to_owned)).collect()
    }

    fn path(&self, path: Option<&Path>) -> Value {
        let Some(path) = path else { return Value::Null };
        let root = lock(&self.root).clone();
        let shown =
            root.and_then(|r| path.strip_prefix(r).ok().map(Path::to_path_buf)).unwrap_or_else(|| path.to_path_buf());
        Value::from(shown.to_string_lossy().into_owned())
    }
}

/// A segment as JSON.
#[must_use]
pub fn segment_json(s: &SpeakerSegment) -> Value {
    obj! {
        "speaker_name" => s.speaker_name.as_str(),
        "speaker_id" => s.speaker_id.as_str(),
        "start_time" => Value::Float(s.start_time),
        "end_time" => Value::Float(s.end_time),
        "duration_ms" => s.duration_ms,
        "is_local" => s.is_local,
    }
}

/// A PCM chunk as JSON, its samples as hex.
#[must_use]
pub fn chunk_json(c: &PcmChunk) -> Value {
    let hex: String = c.pcm.iter().map(|b| format!("{b:02x}")).collect();
    obj! {
        "pid" => c.pid.as_str(),
        "speaker_name" => c.speaker_name.as_str(),
        "is_local" => c.is_local,
        "seq" => Value::Int(c.seq),
        "chunk_start_ts" => Value::Float(c.chunk_start_ts),
        "chunk_end_ts" => Value::Float(c.chunk_end_ts),
        "sample_rate" => Value::Int(c.sample_rate),
        "pcm" => hex,
    }
}

impl LiveEvents for RecordingLive {
    fn speaker_changed(&self, speaker: Option<&str>, is_speaking: bool) {
        self.record(obj! {"call" => "speaker_changed", "speaker" => speaker, "is_speaking" => is_speaking});
    }
    fn segment_completed(&self, segment: &SpeakerSegment) {
        self.record(obj! {"call" => "segment_completed", "segment" => segment_json(segment)});
    }
    fn before_meeting_switch(&self) {
        self.record(obj! {"call" => "before_meeting_switch"});
    }
    fn meeting_changed(&self, meeting_id: Option<&str>, meeting_path: Option<&Path>) -> Result<(), String> {
        self.record(obj! {"call" => "meeting_changed", "meeting_id" => meeting_id, "path" => self.path(meeting_path)});
        lock(&self.meeting_change_error).clone().map_or(Ok(()), Err)
    }
    fn pcm_chunk(&self, chunk: PcmChunk) -> Result<(), String> {
        self.record(obj! {"call" => "pcm_chunk", "chunk" => chunk_json(&chunk)});
        lock(&self.pcm_error).clone().map_or(Ok(()), Err)
    }
    fn wants_pcm(&self) -> bool {
        self.wants_pcm.load(Ordering::SeqCst)
    }
    fn pcm_stats(&self) -> Option<Result<Map, String>> {
        lock(&self.stats).clone()
    }
}

/// The `ktalk_only` capture switch, recorded.
#[derive(Default)]
pub struct RecordingCapture {
    pub calls: Mutex<Vec<bool>>,
    pub active: AtomicBool,
    pub start_error: Mutex<Option<String>>,
    pub stop_error: Mutex<Option<String>>,
}

impl RecordingCapture {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl MeetingCapture for RecordingCapture {
    fn set_capture(&self, active: bool) -> Result<(), String> {
        lock(&self.calls).push(active);
        let error = if active { lock(&self.start_error).clone() } else { lock(&self.stop_error).clone() };
        if let Some(e) = error {
            return Err(e);
        }
        self.active.store(active, Ordering::SeqCst);
        Ok(())
    }
    fn recording_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

/// The live process's identity, as the test sets it.
pub struct StaticIdentity {
    pub identity: Mutex<Result<Value, String>>,
    pub applied: Mutex<Vec<Value>>,
    pub apply_error: Mutex<Option<String>>,
}

impl StaticIdentity {
    #[must_use]
    pub fn new(identity: Value) -> Arc<Self> {
        Arc::new(Self {
            identity: Mutex::new(Ok(identity)),
            applied: Mutex::new(Vec::new()),
            apply_error: Mutex::new(None),
        })
    }

    pub fn set(&self, identity: Result<Value, String>) {
        *lock(&self.identity) = identity;
    }
}

impl IdentitySource for StaticIdentity {
    fn read_identity(&self) -> Result<Value, String> {
        lock(&self.identity).clone()
    }
    fn apply_mic_identity(&self, identity: &Value) -> Result<(), String> {
        if let Some(e) = lock(&self.apply_error).clone() {
            return Err(e);
        }
        lock(&self.applied).push(identity.clone());
        Ok(())
    }
}

/// An identity value as `resolve_identity` returns it.
#[must_use]
pub fn identity(name: &str, aliases: &[&str], revision: &str) -> Value {
    obj! {
        "local_user_name" => name,
        "local_user_aliases" => Value::Array(aliases.iter().map(|a| Value::from(*a)).collect()),
        "local_user_profile_revision" => revision,
        "local_user_identity_known" => !aliases.is_empty(),
    }
}

/// Records increment runs; each completes at once unless told to fail.
#[derive(Default)]
pub struct RecordingIncrements {
    pub runs: Mutex<Vec<(String, PathBuf)>>,
    pub error: Mutex<Option<String>>,
}

impl IncrementGenerator for RecordingIncrements {
    fn generate_all(&self, meeting_id: String, meeting_path: PathBuf) -> BoxFuture<Result<(), String>> {
        lock(&self.runs).push((meeting_id, meeting_path));
        let error = lock(&self.error).clone();
        Box::pin(async move { error.map_or(Ok(()), Err) })
    }
}

/// Records the golden taps.
#[derive(Default)]
pub struct RecordingGolden {
    pub events: Mutex<Vec<Value>>,
}

impl GoldenTap for RecordingGolden {
    fn client_connected(&self, client: u64) {
        lock(&self.events).push(obj! {"event" => "connect", "client" => i64::try_from(client).unwrap_or(0)});
    }
    fn frame(&self, client: u64, data: &Value) {
        lock(&self.events)
            .push(obj! {"event" => "frame", "client" => i64::try_from(client).unwrap_or(0), "data" => data.clone()});
    }
    fn client_disconnected(&self, client: u64) {
        lock(&self.events).push(obj! {"event" => "disconnect", "client" => i64::try_from(client).unwrap_or(0)});
    }
}

/// A storage whose answers the test sets, recording the calls (Python's `Mock()`
/// storage with `_load_meta` and `on_ktalk_leave` return values).
pub struct FakeStore {
    pub meta: Mutex<Result<Value, String>>,
    pub current_id: Mutex<Option<String>>,
    pub current_path: Mutex<Option<PathBuf>>,
    pub calls: Mutex<Vec<Value>>,
    /// What `on_ktalk_leave` answers.
    pub leave_id: Mutex<String>,
    /// What `on_meeting_join` answers, and the path it moves to.
    pub join_result: Mutex<(Option<String>, Option<PathBuf>)>,
    pub transcript: Mutex<Vec<Value>>,
    pub join_error: Mutex<Option<String>>,
}

impl FakeStore {
    /// A current meeting of `source` with id `m1` at `/meetings/m1`.
    #[must_use]
    pub fn with_source(source: &str) -> Arc<Self> {
        Arc::new(Self {
            meta: Mutex::new(Ok(obj! {"source" => source})),
            current_id: Mutex::new(Some("m1".into())),
            current_path: Mutex::new(Some(PathBuf::from("/meetings/m1"))),
            calls: Mutex::new(Vec::new()),
            leave_id: Mutex::new("new_meeting_id".into()),
            join_result: Mutex::new((Some("m1".into()), Some(PathBuf::from("/meetings/m1")))),
            transcript: Mutex::new(Vec::new()),
            join_error: Mutex::new(None),
        })
    }

    pub fn set_meta(&self, meta: Result<Value, String>) {
        *lock(&self.meta) = meta;
    }

    #[must_use]
    pub fn calls(&self) -> Vec<Value> {
        lock(&self.calls).clone()
    }

    /// The `op` names of the calls.
    #[must_use]
    pub fn ops(&self) -> Vec<String> {
        self.calls().iter().filter_map(|c| c.as_object()?.get("op")?.as_str().map(str::to_owned)).collect()
    }
}

impl MeetingStore for FakeStore {
    fn current_meeting_id(&self) -> Option<String> {
        lock(&self.current_id).clone()
    }
    fn current_meeting_path(&self) -> Option<PathBuf> {
        lock(&self.current_path).clone()
    }
    fn load_meta(&self) -> Result<Value, String> {
        lock(&self.meta).clone()
    }
    fn is_conference_source(&self, source: Option<&str>) -> bool {
        source.is_some_and(|s| recog_core::meeting_storage::CONFERENCE_SOURCES.contains(&s))
    }
    fn on_meeting_join(&self, url: &str, title: Option<&str>, platform: &str) -> Result<Option<String>, String> {
        lock(&self.calls)
            .push(obj! {"op" => "on_meeting_join", "url" => url, "title" => title, "platform" => platform});
        if let Some(e) = lock(&self.join_error).clone() {
            return Err(e);
        }
        let (id, path) = lock(&self.join_result).clone();
        lock(&self.current_id).clone_from(&id);
        *lock(&self.current_path) = path;
        self.set_meta(Ok(obj! {"source" => platform, "title" => title}));
        Ok(id)
    }
    fn on_ktalk_leave(&self, reason: &str) -> Result<String, String> {
        lock(&self.calls).push(obj! {"op" => "on_ktalk_leave", "reason" => reason});
        let id = lock(&self.leave_id).clone();
        *lock(&self.current_id) = Some(id.clone());
        *lock(&self.current_path) = Some(PathBuf::from(format!("/meetings/{id}")));
        self.set_meta(Ok(obj! {"source" => "orphan"}));
        Ok(id)
    }
    fn update_title(&self, title: &str) -> Result<(), String> {
        lock(&self.calls).push(obj! {"op" => "update_title", "title" => title});
        Ok(())
    }
    fn get_current_transcript(&self) -> Result<Vec<Value>, String> {
        Ok(lock(&self.transcript).clone())
    }
}

/// A log sink that keeps the lines.
#[must_use]
pub fn collecting_log() -> (crate::Logger, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    (Arc::new(move |line: &str| lock(&sink).push(line.to_owned())), lines)
}

impl FakeConnection {
    /// The messages pushed but not yet read by the handler (Python's `fake_ws._messages`).
    #[must_use]
    pub fn queued(&self) -> usize {
        lock(&self.queue.state).items.len()
    }
}
