//! `recog-bridge-probe <work dir>`: runs the Rust bridge on a scenario (JSON on
//! stdin) and prints what it did, in the shape `tests/parity/bridge_driver.py`
//! prints for the Python bridge on the same scenario, for the differential tests.
//!
//! The clock is virtual (advanced by the scenario), meeting ids count up, titles come
//! from a fake generator, the storage is a real `MeetingStorage` under the work dir,
//! and the live side is a recorder; sockets are fakes, or real WebSockets for a
//! scenario whose transport is "ws".
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use recog_bridge::hooks::{CopilotPolicy, IdentitySource, LiveEvents, MeetingCapture, PcmChunk, SpeakerSegment};
use recog_bridge::obj;
use recog_bridge::testing::{
    FakeConnection, ManualClock, RecordingGolden, RecordingIncrements, RecordingSink, SendMode, SwitchablePolicy,
    chunk_json, segment_json,
};
use recog_bridge::{Bridge, BridgeConfig, ClientId, RefineUpdate, Role, TranscriptLine};
use recog_core::meeting_storage::{
    GeneratedTitle, MeetingStorage, NewSegment, SessionMode, StorageOptions, Stream, TitleGenerator, TitleRequest,
};
use recog_core::pyjson::{self, Map, Value};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const IDLE_TIMEOUT: Duration = Duration::from_secs(1);
/// The local zone of the runs: UTC+3 without daylight saving (TZ=Etc/GMT-3).
const UTC_OFFSET_S: i64 = 3 * 3600;

type Lines = Arc<Mutex<Vec<String>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The live side: every callback, in order, in one list (as the Python recorder).
#[derive(Default)]
struct ProbeLive {
    root: PathBuf,
    calls: Mutex<Vec<Value>>,
    wants_pcm: AtomicBool,
    pcm_error: Mutex<Option<String>>,
    meeting_change_error: Mutex<Option<String>>,
    stats: Mutex<Option<Value>>,
    capture_active: AtomicBool,
    capture_start_error: Mutex<Option<String>>,
    capture_stop_error: Mutex<Option<String>>,
    identity: Mutex<Option<Value>>,
    identity_error: Mutex<Option<String>>,
    apply_error: Mutex<Option<String>>,
}

impl ProbeLive {
    fn record(&self, call: Value) {
        lock(&self.calls).push(call);
    }

    fn rel(&self, path: Option<&Path>) -> Value {
        path.map_or(Value::Null, |p| {
            Value::from(p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().into_owned())
        })
    }
}

impl LiveEvents for ProbeLive {
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
        self.record(obj! {"call" => "meeting_changed", "meeting_id" => meeting_id, "path" => self.rel(meeting_path)});
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
        let stats = lock(&self.stats).clone()?;
        let map = stats.as_object().cloned().unwrap_or_default();
        Some(match map.get("error") {
            Some(e) => Err(e.py_str()),
            None => Ok(map),
        })
    }
}

impl MeetingCapture for ProbeLive {
    fn set_capture(&self, active: bool) -> Result<(), String> {
        self.record(obj! {"call" => "set_capture", "active" => active});
        let error =
            if active { lock(&self.capture_start_error).clone() } else { lock(&self.capture_stop_error).clone() };
        if let Some(e) = error {
            return Err(e);
        }
        self.capture_active.store(active, Ordering::SeqCst);
        Ok(())
    }
    fn recording_active(&self) -> bool {
        self.capture_active.load(Ordering::SeqCst)
    }
}

impl IdentitySource for ProbeLive {
    fn read_identity(&self) -> Result<Value, String> {
        if let Some(e) = lock(&self.identity_error).clone() {
            return Err(e);
        }
        Ok(lock(&self.identity).clone().unwrap_or(Value::Null))
    }
    fn apply_mic_identity(&self, identity: &Value) -> Result<(), String> {
        if let Some(e) = lock(&self.apply_error).clone() {
            return Err(e);
        }
        self.record(obj! {"call" => "apply_mic_identity", "identity" => identity.clone()});
        Ok(())
    }
}

/// The driver's fake `generate_title`, with its fake settings' provenance.
struct FakeTitles;

impl TitleGenerator for FakeTitles {
    fn generate(&self, r: &TitleRequest) -> Result<Option<GeneratedTitle>, String> {
        if r.transcript.contains("untitled") {
            return Ok(None);
        }
        Ok(Some(GeneratedTitle {
            title: format!("T{}|{}|{}|{}", r.transcript.chars().count(), r.user_name, r.participants, r.meeting_title),
            provider: "claude".into(),
            model: Value::from("m1"),
            settings_revision: Value::from("r1"),
        }))
    }
}

fn get<'a>(map: &'a Map, key: &str) -> Option<&'a Value> {
    map.get(key)
}

fn text(map: &Map, key: &str) -> String {
    get(map, key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn flag(map: &Map, key: &str, default: bool) -> bool {
    get(map, key).map_or(default, Value::truthy)
}

fn opt_text(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn policy_of(value: &Value) -> Result<CopilotPolicy, String> {
    let map = value.as_object().cloned().unwrap_or_default();
    if let Some(e) = map.get("error") {
        return Err(e.py_str());
    }
    Ok(CopilotPolicy {
        instructions: map.get("instructions").cloned().unwrap_or(Value::Null),
        revision: map.get("revision").cloned().unwrap_or(Value::Null),
        enabled: map.get("enabled").cloned().unwrap_or(Value::Null),
    })
}

fn mode_of(step: &Map) -> SendMode {
    match get(step, "mode").and_then(Value::as_str) {
        Some("stall") => SendMode::Stall,
        Some("fail") => SendMode::Fail(text(step, "fail")),
        _ => SendMode::Record,
    }
}

struct WsClient {
    sink: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >,
    received: Lines,
    closes: Arc<Mutex<Option<Vec<Value>>>>,
    reader: JoinHandle<()>,
}

struct Probe {
    root: PathBuf,
    scenario: Map,
    clock: Arc<ManualClock>,
    bridge: Arc<Bridge>,
    storage: Option<Arc<MeetingStorage>>,
    live: Arc<ProbeLive>,
    policy: Arc<SwitchablePolicy>,
    increments: Arc<RecordingIncrements>,
    golden: Arc<RecordingGolden>,
    clients: Vec<(String, FakeConnection, JoinHandle<()>)>,
    ws: BTreeMap<String, WsClient>,
    ws_refused: BTreeMap<String, Vec<Value>>,
    ws_order: Vec<String>,
    ws_addr: Option<std::net::SocketAddr>,
    results: Vec<Value>,
    stdout: Lines,
    stderr: Lines,
}

impl Probe {
    fn new(root: PathBuf, home: PathBuf, scenario: Map) -> Self {
        let config = scenario.get("config").and_then(Value::as_object).cloned().unwrap_or_default();
        let clock = Arc::new(ManualClock::default());
        clock.offset_s.store(UTC_OFFSET_S, Ordering::SeqCst);
        let start = config.get("start_micros").and_then(recog_bridge::json::as_int).unwrap_or(1_783_159_200_000_000);
        clock.set_micros(i64::try_from(start).unwrap_or(0));
        let (stdout, stderr): (Lines, Lines) = (Arc::default(), Arc::default());
        let default_policy = obj! {"instructions" => "Be brief.", "revision" => "r1", "enabled" => true};
        let policy = SwitchablePolicy::enabled();
        policy.set(policy_of(config.get("policy").unwrap_or(&default_policy)));
        let send_timeout = config.get("send_timeout").and_then(recog_bridge::json::as_f64).unwrap_or(5.0);
        let bridge_config = BridgeConfig {
            ktalk_only: flag(&config, "ktalk_only", false),
            debug_file: Some(root.join("ktalk_debug.json")),
            send_timeout: Duration::from_secs_f64(send_timeout),
            ..BridgeConfig::default()
        };
        let log_lines = stderr.clone();
        let log: recog_bridge::Logger = Arc::new(move |line: &str| lock(&log_lines).push(line.to_owned()));
        let bridge = Bridge::with_clock(bridge_config, policy.clone(), clock.clone(), log).expect("policy at start");
        bridge.attach_runtime(tokio::runtime::Handle::current());
        let live = Arc::new(ProbeLive { root: root.clone(), ..ProbeLive::default() });
        live.wants_pcm.store(flag(&config, "wants_pcm", true), Ordering::SeqCst);
        *lock(&live.stats) = config.get("pcm_stats").cloned();
        bridge.set_live_events(Some(live.clone()));
        if flag(&config, "capture", false) {
            bridge.set_meeting_capture(live.clone());
        }
        if let Some(identity) = config.get("identity") {
            *lock(&live.identity) = Some(identity.clone());
            bridge.configure_user_profile(identity, live.clone()).expect("identity");
        }
        let increments = Arc::new(RecordingIncrements::default());
        let storage = flag(&config, "storage", true).then(|| {
            let mut options = StorageOptions::new(&home);
            options.clock = clock.clone();
            options.external_index_cache_dir = home.join("cache");
            let count = Arc::new(AtomicU64::new(0));
            options.meeting_suffix = Arc::new(move || format!("{:08x}", count.fetch_add(1, Ordering::SeqCst) + 1));
            options.title_generator = Some(Arc::new(FakeTitles));
            let (out, err) = (stdout.clone(), stderr.clone());
            options.log = Arc::new(move |stream, line| match stream {
                Stream::Stdout => lock(&out).push(line.to_owned()),
                Stream::Stderr => lock(&err).push(line.to_owned()),
            });
            let storage = Arc::new(MeetingStorage::new(&root.join("meetings"), options).expect("storage"));
            let mode =
                SessionMode { ktalk: true, ktalk_only: flag(&config, "ktalk_only", false), ..SessionMode::default() };
            storage.start_session(&mode).expect("session");
            bridge.set_meeting_storage(storage.clone());
            bridge.set_increment_generator(increments.clone());
            storage
        });
        let golden = Arc::new(RecordingGolden::default());
        if flag(&config, "golden", false) {
            bridge.set_golden(Some(golden.clone()));
        }
        Self {
            root,
            scenario,
            clock,
            bridge,
            storage,
            live,
            policy,
            increments,
            golden,
            clients: Vec::new(),
            ws: BTreeMap::new(),
            ws_refused: BTreeMap::new(),
            ws_order: Vec::new(),
            ws_addr: None,
            results: Vec::new(),
            stdout,
            stderr,
        }
    }

    fn client(&self, name: &str) -> &FakeConnection {
        &self.clients.iter().find(|(n, _, _)| n == name).expect("known client").1
    }

    async fn wait_idle(&self, name: &str) {
        let _ = tokio::time::timeout(IDLE_TIMEOUT, self.client(name).idle()).await;
        settle().await;
    }

    async fn wait_handler(&mut self, name: &str) {
        let index = self.clients.iter().position(|(n, _, _)| n == name).expect("known client");
        let task = &mut self.clients[index].2;
        let _ = tokio::time::timeout(IDLE_TIMEOUT, task).await;
        settle().await;
    }

    fn ws_transport(&self) -> bool {
        self.scenario.get("transport").and_then(Value::as_str) == Some("ws")
    }

    async fn step(&mut self, step: &Map) {
        let op = text(step, "op");
        if self.ws_transport() && matches!(op.as_str(), "connect" | "send" | "send_text" | "close") {
            self.ws_step(step, &op).await;
            return;
        }
        match op.as_str() {
            "connect" => {
                let name = text(step, "client");
                let origins = get(step, "origins").and_then(ValueExt::as_array_strings);
                let index = self.clients.len() + 1;
                let sink = RecordingSink::with_mode(mode_of(step));
                let (fake, connection) =
                    FakeConnection::with_sink(sink, &format!("('127.0.0.1', {})", 40000 + index), origins);
                let bridge = self.bridge.clone();
                let task = tokio::spawn(async move { bridge.handle_client(connection).await });
                self.clients.push((name.clone(), fake, task));
                self.wait_idle(&name).await;
            }
            "send" | "send_text" => {
                let name = text(step, "client");
                let fake = self.client(&name);
                if op == "send" {
                    fake.send(get(step, "frame").unwrap_or(&Value::Null));
                } else {
                    fake.send_text(&text(step, "text"));
                }
                self.wait_idle(&name).await;
            }
            "close" => {
                let name = text(step, "client");
                self.client(&name).close();
                self.wait_handler(&name).await;
            }
            "fail" => {
                let name = text(step, "client");
                self.client(&name).fail(&text(step, "message"));
                self.wait_handler(&name).await;
            }
            "mode" => self.client(&text(step, "client")).sink.set_mode(mode_of(step)),
            "advance" => self.clock.advance(get(step, "seconds").and_then(recog_bridge::json::as_f64).unwrap_or(0.0)),
            "sleep" => {
                let seconds = get(step, "seconds").and_then(recog_bridge::json::as_f64).unwrap_or(0.0);
                tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                settle().await;
            }
            "periodic" => {
                self.bridge.periodic().await;
                self.bridge.write_debug_state();
                settle().await;
            }
            "call" => self.call(step).await,
            "query" => {
                let result = self.query(step);
                self.results.push(result);
            }
            "set" => self.set(step),
            other => panic!("unknown op {other}"),
        }
    }

    async fn call(&self, step: &Map) {
        let method = text(step, "method");
        let args = get(step, "args").and_then(Value::as_object).cloned().unwrap_or_default();
        match method.as_str() {
            "send_transcript" => {
                let int = |k: &str| get(&args, k).and_then(recog_bridge::json::as_int).unwrap_or(0);
                self.bridge.send_transcript(&TranscriptLine {
                    speaker: text(&args, "speaker"),
                    text: text(&args, "text"),
                    timestamp: text(&args, "timestamp"),
                    is_continuation: flag(&args, "is_continuation", false),
                    is_local: flag(&args, "is_local", false),
                    uncertain: flag(&args, "uncertain", false),
                    uncertain_reason: text(&args, "uncertain_reason"),
                    correction: flag(&args, "correction", false),
                    segment_id: i64::try_from(int("segment_id")).unwrap_or(0),
                    source_id: text(&args, "source_id"),
                });
            }
            "send_refine" => {
                let updates: Vec<RefineUpdate> = get(&args, "updates")
                    .and_then(ValueExt::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_object)
                            .map(|u| RefineUpdate {
                                segment_id: get(u, "segment_id")
                                    .and_then(recog_bridge::json::as_int)
                                    .and_then(|n| i64::try_from(n).ok())
                                    .unwrap_or(0),
                                text: text(u, "text"),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.bridge.send_refine(&updates, opt_text(get(&args, "meeting_id")).as_deref());
            }
            "append_segment" => {
                let storage = self.storage.as_ref().expect("storage");
                let segment = NewSegment {
                    speaker: text(&args, "speaker"),
                    speaker_id: text(&args, "speaker_id"),
                    text: text(&args, "text"),
                    is_local: flag(&args, "is_local", false),
                    is_continuation: flag(&args, "is_continuation", false),
                    uncertain: flag(&args, "uncertain", false),
                    uncertain_reason: text(&args, "uncertain_reason"),
                    audio_offset_ms: 0,
                    captured_at: get(&args, "captured_at").and_then(recog_bridge::json::as_f64),
                };
                storage.append_segment(&segment).expect("append");
            }
            "end_meeting_on_extension_gone" => {
                if let Err(e) = self.bridge.end_meeting_on_extension_gone().await {
                    lock(&self.stderr).push(format!("raised: {e}"));
                }
            }
            other => panic!("unknown call {other}"),
        }
        settle().await;
    }

    fn name_of(&self, id: Option<ClientId>) -> Value {
        let Some(ClientId(n)) = id else { return Value::Null };
        let index = usize::try_from(n).unwrap_or(0).wrapping_sub(1);
        self.clients.get(index).map_or(Value::Null, |(name, _, _)| Value::from(name.as_str()))
    }

    fn query(&self, step: &Map) -> Value {
        let b = &self.bridge;
        let method = text(step, "method");
        let args: Vec<Value> = get(step, "args").and_then(ValueExt::as_array).cloned().unwrap_or_default();
        let num = |i: usize| args.get(i).and_then(recog_bridge::json::as_f64).unwrap_or(0.0);
        let opt = |v: Option<String>| Value::from(v);
        match method.as_str() {
            "state" => self.state(),
            "get_current_speaker" => opt(b.get_current_speaker()),
            "get_dominant_speaker_for_period" => opt(b.get_dominant_speaker_for_period(num(0), num(1))),
            "is_someone_speaking" => Value::Bool(b.is_someone_speaking()),
            "is_current_speaker_local" => Value::Bool(b.is_current_speaker_local()),
            "are_all_mics_muted" => Value::Bool(b.are_all_mics_muted()),
            "is_local_mic_muted" => Value::Bool(b.is_local_mic_muted()),
            "is_speaker_muted" => Value::Bool(b.is_speaker_muted(&args.first().map(Value::py_str).unwrap_or_default())),
            "has_remote_participants" => Value::Bool(b.has_remote_participants()),
            "get_sole_unmuted_remote_participant" => opt(b.get_sole_unmuted_remote_participant()),
            "is_conference_active" => Value::Bool(b.is_conference_active()),
            "is_recording_active" => Value::Bool(b.is_recording_active()),
            "user_identity_snapshot" => Value::Object(b.user_identity_snapshot()),
            "copilot_policy_snapshot" => Value::Object(b.copilot_policy_snapshot()),
            "compute_meeting_state" => b.compute_meeting_state(),
            "get_participants" => {
                let mut map = Map::new();
                for (k, p) in b.get_participants() {
                    map.insert(
                        k,
                        obj! {"id" => p.id.as_str(), "name" => p.name.as_str(), "is_local" => p.is_local, "mic_muted" => p.mic_muted},
                    );
                }
                Value::Object(map)
            }
            "get_speaker_history" => Value::Array(
                b.get_speaker_history()
                    .iter()
                    .map(|e| {
                        obj! {
                            "speaker_name" => e.speaker_name.as_deref(),
                            "speaker_id" => e.speaker_id.as_str(),
                            "timestamp" => Value::Float(e.timestamp),
                            "is_local" => e.is_local,
                        }
                    })
                    .collect(),
            ),
            "get_all_completed_segments" => {
                Value::Array(b.get_all_completed_segments().iter().map(segment_json).collect())
            }
            other => panic!("unknown query {other}"),
        }
    }

    fn state(&self) -> Value {
        let b = &self.bridge;
        let pending = b.pending_requests().into_iter().map(|(k, v)| Value::Array(vec![Value::from(k), Value::from(v)]));
        let tick = b.tick_state().map_or(Value::Null, |t| {
            obj! {
                "meeting_id" => t.meeting_id.as_deref(),
                "meeting_title" => t.meeting_title.clone(),
                "seg_count" => t.seg_count,
                "last_seg_count" => t.last_seg_count,
                "last_emit_ts" => Value::Float(t.last_emit_ts),
            }
        });
        let agent = b.with_agent(|a| {
            obj! {
                "status" => a.supervisor_status.as_deref(),
                "sessions" => a.claude_sessions.map_or(Value::Null, Value::Int),
                "zero_since" => a.sessions_zero_since.map_or(Value::Null, Value::Float),
                "last" => a.last_effective.as_str(),
                "recovery_sent_at" => a.channel_recovery_sent_at.map_or(Value::Null, Value::Float),
            }
        });
        let mut roles = Map::new();
        let mut timed_out = Vec::new();
        for (i, (name, _, _)) in self.clients.iter().enumerate() {
            let id = ClientId(u64::try_from(i + 1).unwrap_or(0));
            let names = b.roles_of(id).into_iter().map(|r| match r {
                Role::Extension => Value::from("extension"),
                Role::Subscriber => Value::from("subscriber"),
                Role::Channel => Value::from("channel"),
                Role::Supervisor => Value::from("supervisor"),
            });
            roles.insert(name.clone(), Value::Array(names.collect()));
            if b.is_timed_out(id) {
                timed_out.push(Value::from(name.as_str()));
            }
        }
        timed_out.sort_by_key(Value::py_str);
        obj! {
            "pending" => Value::Array(pending.collect()),
            "owner" => self.name_of(b.meeting_owner()),
            "manual" => b.manual_copilot_active(),
            "tick" => tick,
            "buffer" => Value::Array(b.transcript_buffer().into_iter().map(Value::Object).collect()),
            "last_seq" => b.last_seq(),
            "clock_offset" => Value::Float(b.clock_offset()),
            "agent" => agent,
            "roles" => Value::Object(roles),
            "timed_out" => Value::Array(timed_out),
            "transport_stats" => Value::Object(b.pcm_transport_stats()),
        }
    }

    fn set(&self, step: &Map) {
        let l = &self.live;
        for (key, value) in step.iter() {
            let msg = || opt_text(Some(value));
            match key.as_str() {
                "op" => {}
                "policy" => self.policy.set(policy_of(value)),
                "identity" => {
                    *lock(&l.identity) = Some(value.clone());
                    *lock(&l.identity_error) = None;
                }
                "identity_error" => *lock(&l.identity_error) = msg(),
                "apply_error" => *lock(&l.apply_error) = msg(),
                "pcm_error" => *lock(&l.pcm_error) = msg(),
                "meeting_change_error" => *lock(&l.meeting_change_error) = msg(),
                "capture_start_error" => *lock(&l.capture_start_error) = msg(),
                "capture_stop_error" => *lock(&l.capture_stop_error) = msg(),
                "pcm_stats" => *lock(&l.stats) = Some(value.clone()),
                other => panic!("unknown setting {other}"),
            }
        }
    }

    async fn ws_step(&mut self, step: &Map, op: &str) {
        let name = text(step, "client");
        if op != "connect" && !self.ws.contains_key(&name) {
            tokio::time::sleep(Duration::from_millis(200)).await;
            return;
        }
        match op {
            "connect" => {
                let addr = self.ws_addr.expect("server");
                let mut request = format!("ws://{addr}").into_client_request().expect("request");
                for origin in get(step, "origins").and_then(ValueExt::as_array_strings).unwrap_or_default() {
                    request.headers_mut().append("Origin", origin.parse().expect("header"));
                }
                let socket = match tokio_tungstenite::connect_async(request).await {
                    Ok((socket, _)) => socket,
                    Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                        let body = String::from_utf8_lossy(response.body().as_deref().unwrap_or_default()).into_owned();
                        let refusal = vec![Value::Array(vec![
                            Value::from("http"),
                            Value::from(i64::from(response.status().as_u16())),
                            Value::from(body),
                        ])];
                        self.ws_refused.insert(name.clone(), refusal);
                        self.ws_order.push(name);
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        return;
                    }
                    Err(e) => panic!("connect: {e}"),
                };
                let (sink, mut stream) = socket.split();
                let received: Lines = Arc::default();
                let closes = Arc::new(Mutex::new(None));
                let (r, c) = (received.clone(), closes.clone());
                let reader = tokio::spawn(async move {
                    let mut close = Vec::new();
                    while let Some(Ok(message)) = stream.next().await {
                        match message {
                            Message::Text(t) => lock(&r).push(t.as_str().to_owned()),
                            Message::Close(Some(frame)) => {
                                close.push(Value::Array(vec![
                                    Value::from(i64::from(u16::from(frame.code))),
                                    Value::from(frame.reason.as_str()),
                                ]));
                            }
                            _ => {}
                        }
                    }
                    *lock(&c) = Some(close);
                });
                self.ws.insert(name.clone(), WsClient { sink, received, closes, reader });
                self.ws_order.push(name);
            }
            "send" => {
                let frame = recog_bridge::json::dumps(get(step, "frame").unwrap_or(&Value::Null));
                let _ = self.ws.get_mut(&name).expect("client").sink.send(Message::text(frame)).await;
            }
            "send_text" => {
                let message = if flag(step, "binary", false) {
                    Message::Binary(text(step, "text").into_bytes().into())
                } else {
                    Message::text(text(step, "text"))
                };
                let _ = self.ws.get_mut(&name).expect("client").sink.send(message).await;
            }
            "close" => self.ws_close(&name).await,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    async fn ws_close(&mut self, name: &str) {
        let client = self.ws.get_mut(name).expect("client");
        // A close with 1000, as Python's websockets client sends it.
        let frame = tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
            reason: "".into(),
        };
        let _ = client.sink.send(Message::Close(Some(frame))).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), &mut client.reader).await;
    }

    async fn run(&mut self) {
        if self.ws_transport() {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
            self.ws_addr = listener.local_addr().ok();
            let bridge = self.bridge.clone();
            tokio::spawn(async move { bridge.accept(listener).await });
        }
        let steps = self.scenario.get("steps").and_then(ValueExt::as_array).cloned().unwrap_or_default();
        for step in &steps {
            self.step(step.as_object().expect("step object")).await;
        }
        let names: Vec<String> = self.clients.iter().map(|(n, _, _)| n.clone()).collect();
        for name in names {
            let index = self.clients.iter().position(|(n, _, _)| *n == name).expect("client");
            if !self.clients[index].2.is_finished() {
                self.clients[index].1.close();
                self.wait_handler(&name).await;
            }
        }
        for name in self.ws_order.clone() {
            let Some(client) = self.ws.get(&name) else { continue };
            let done = lock(&client.closes).is_some();
            if !done {
                self.ws_close(&name).await;
            }
        }
        settle().await;
        self.bridge.drain_background_tasks(Duration::from_secs(5)).await;
    }

    fn output(&self) -> Value {
        let (mut received, mut closes) = (Map::new(), Map::new());
        if self.ws_transport() {
            for name in &self.ws_order {
                if let Some(refused) = self.ws_refused.get(name) {
                    received.insert(name.clone(), Value::Array(Vec::new()));
                    closes.insert(name.clone(), Value::Array(refused.clone()));
                    continue;
                }
                let client = &self.ws[name];
                let texts = lock(&client.received).iter().map(|t| Value::from(t.as_str())).collect();
                received.insert(name.clone(), Value::Array(texts));
                closes.insert(name.clone(), Value::Array(lock(&client.closes).clone().unwrap_or_default()));
            }
        } else {
            for (name, fake, _) in &self.clients {
                let texts = lock(&fake.sink.sent).iter().map(|t| Value::from(t.as_str())).collect();
                received.insert(name.clone(), Value::Array(texts));
                let list = lock(&fake.sink.closes)
                    .iter()
                    .map(|(code, reason)| {
                        Value::Array(vec![Value::from(i64::from(*code)), Value::from(reason.as_str())])
                    })
                    .collect();
                closes.insert(name.clone(), Value::Array(list));
            }
        }
        let increments = lock(&self.increments.runs)
            .iter()
            .map(|(id, path)| {
                let rel = path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().into_owned();
                Value::Array(vec![Value::from(id.as_str()), Value::from(rel)])
            })
            .collect();
        let mut files = Map::new();
        let mut paths = Vec::new();
        collect_files(&self.root, &mut paths);
        paths.sort();
        for path in paths {
            let rel = path.strip_prefix(&self.root).unwrap_or(&path).to_string_lossy().into_owned();
            files.insert(rel, Value::from(std::fs::read_to_string(&path).unwrap_or_default()));
        }
        let lines = |l: &Lines| Value::Array(lock(l).iter().map(|s| Value::from(s.as_str())).collect());
        obj! {
            "received" => Value::Object(received),
            "closes" => Value::Object(closes),
            "live" => Value::Array(lock(&self.live.calls).clone()),
            "results" => Value::Array(self.results.clone()),
            "increments" => Value::Array(increments),
            "golden" => Value::Array(lock(&self.golden.events).clone()),
            "files" => Value::Object(files),
            "stdout" => lines(&self.stdout),
            "stderr" => lines(&self.stderr),
        }
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

trait ValueExt {
    fn as_array_strings(&self) -> Option<Vec<String>>;
    fn as_array(&self) -> Option<&Vec<Value>>;
}

impl ValueExt for Value {
    fn as_array_strings(&self) -> Option<Vec<String>> {
        self.as_array().map(|items| items.iter().map(Value::py_str).collect())
    }
    fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(root), Some(home)) = (args.next(), args.next()) else {
        eprintln!("usage: recog-bridge-probe <work dir> <home dir> < scenario.json");
        std::process::exit(2);
    };
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).expect("stdin");
    let scenario = match pyjson::loads(&input) {
        Ok(Value::Object(map)) => map,
        _ => {
            eprintln!("recog-bridge-probe: the scenario is not a JSON object");
            std::process::exit(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let output = runtime.block_on(async move {
        let mut probe = Probe::new(PathBuf::from(root), PathBuf::from(home), scenario);
        probe.run().await;
        probe.output()
    });
    println!("{}", pyjson::dumps(&output, None));
}
