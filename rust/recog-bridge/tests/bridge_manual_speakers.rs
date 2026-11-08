//! tests/test_ktalk_bridge.py from `test_real_subscriber_socket_relays_spawn_and_stop_to_supervisor`
//! to the end: manual Copilot sessions (spawn/stop, their races with a conference
//! start and with a reconnecting supervisor), preemption by a conference, terminal
//! echoes of stale replies, the channel recovery request, the speaker `since`
//! backdating, attribution around the user's own voice, and the golden recorder.
//!
//! Python's mocked `_is_meeting_active` and `_relay_to_supervisors` have no Rust
//! counterpart: those cases drive the real conditions instead (a storage switched
//! to a conference from inside a supervisor's send; a registered supervisor whose
//! frames are counted). The contended-lock-on-a-new-event-loop case is ported for its
//! intent: the same bridge serves contending manual requests on a second runtime.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Harness, NOW, field, json, map, settle, wait_for};
use futures_util::{SinkExt, StreamExt};
use recog_bridge::hooks::BoxFuture;
use recog_bridge::speakers::SPEAKER_ONSET_MAX_BACKDATE_S;
use recog_bridge::testing::{FakeStore, RecordingGolden, RecordingSink, SendMode, frame_type};
use recog_bridge::{Bridge, Client, ClientId, ClientSink, Role, SendError};
use recog_core::pyjson::{self, Value};
use tokio::sync::{Notify, Semaphore};
use tokio_tungstenite::tungstenite::Message;

const MSG_MEETING_ENDED: &str = "Ответ не доставлен: встреча, в которой был задан вопрос, уже завершилась.";
const MSG_MEETING_CHANGED: &str = "Ответ не доставлен: встреча сменилась до получения ответа.";
const MSG_MANUAL_PREEMPTED: &str = "Ответ не доставлен: ручная сессия copilot была прервана.";

const JOIN: &str =
    r#"{"type": "meeting_join", "url": "https://ktalk.test/room", "title": "Standup", "platform": "ktalk"}"#;

// ---- helpers (the Python module's fixtures) ----

/// A client in no set, like the Python tests' bare `RecordingClient()` originators.
fn loose(n: u64) -> (Arc<RecordingSink>, Arc<Client>) {
    let sink = RecordingSink::new();
    let client = Client::new(ClientId(1_000_000 + n), sink.clone(), "('127.0.0.1', 12345)");
    (sink, client)
}

fn status_of(frame: &Value) -> Option<&str> {
    frame.as_object().and_then(|m| m.get("status")).and_then(Value::as_str)
}

fn str_field<'a>(frame: &'a Value, key: &str) -> Option<&'a str> {
    field(frame, key).as_str()
}

/// `_copilot_frames(client, frame_type, status)`.
fn frames_of(frames: Vec<Value>, kind: &str, status: Option<&str>) -> Vec<Value> {
    frames.into_iter().filter(|f| frame_type(f) == Some(kind) && (status.is_none() || status_of(f) == status)).collect()
}

fn copilot_frames(sink: &RecordingSink, kind: &str, status: Option<&str>) -> Vec<Value> {
    frames_of(sink.frames(), kind, status)
}

/// `_agent_states(client)`.
fn agent_states(sink: &RecordingSink) -> Vec<String> {
    copilot_frames(sink, "copilot_status", Some("agent_state"))
        .iter()
        .filter_map(|f| str_field(f, "agent_state").map(str::to_owned))
        .collect()
}

fn pending(h: &Harness, request_id: &str) -> Option<String> {
    h.bridge.pending_requests().into_iter().find(|(k, _)| k == request_id).map(|(_, v)| v)
}

/// `_ktalk_storage(meeting_id, title, source)`.
fn ktalk_storage(meeting_id: &str, title: &str, source: &str) -> Arc<FakeStore> {
    let storage = FakeStore::with_source(source);
    storage.set_meta(Ok(json(&format!(r#"{{"source": "{source}", "title": "{title}"}}"#))));
    *storage.current_id.lock().unwrap() = Some(meeting_id.to_owned());
    *storage.current_path.lock().unwrap() = Some(PathBuf::from(format!("/tmp/{meeting_id}.jsonl")));
    storage
}

/// `_meeting_storage_mock(meeting_id)`: a join moves the current meeting to a new
/// path, so the bridge runs the lifecycle.
fn meeting_storage_mock(meeting_id: &str) -> Arc<FakeStore> {
    let storage = FakeStore::with_source("ktalk");
    *storage.current_id.lock().unwrap() = Some(meeting_id.to_owned());
    *storage.current_path.lock().unwrap() = Some(PathBuf::from("/tmp/old.jsonl"));
    *storage.join_result.lock().unwrap() =
        (Some(meeting_id.to_owned()), Some(PathBuf::from(format!("/tmp/{meeting_id}.jsonl"))));
    storage
}

/// An orphan meeting that a join turns into the ktalk meeting `m-conf`.
fn orphan_then_conference(orphan_id: &str) -> Arc<FakeStore> {
    let storage = FakeStore::with_source("orphan");
    storage.set_meta(Ok(json(r#"{"source": "orphan", "title": "Standup"}"#)));
    *storage.current_id.lock().unwrap() = Some(orphan_id.to_owned());
    *storage.current_path.lock().unwrap() = Some(PathBuf::from(format!("/tmp/{orphan_id}.jsonl")));
    *storage.join_result.lock().unwrap() = (Some("m-conf".to_owned()), Some(PathBuf::from("/tmp/m-conf.jsonl")));
    storage
}

/// `_bridge_with_supervisor()`.
fn bridge_with_supervisor() -> (Harness, Arc<RecordingSink>, Arc<RecordingSink>) {
    let h = Harness::new();
    let (sup, _) = h.add(Role::Supervisor);
    let (sub, _) = h.add(Role::Subscriber);
    (h, sup, sub)
}

/// `_bridge_with_extension_and_subscriber()`.
fn bridge_with_extension_and_subscriber() -> (Harness, Arc<RecordingSink>, Arc<RecordingSink>) {
    let h = Harness::new();
    let (ext, _) = h.add(Role::Extension);
    let (sub, _) = h.add(Role::Subscriber);
    (h, ext, sub)
}

/// `_bridge_with_active_ktalk()`.
fn bridge_with_active_ktalk() -> (Harness, Arc<RecordingSink>) {
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "ktalk"));
    let (ext, _) = h.add(Role::Extension);
    (h, ext)
}

type Hook = Box<dyn Fn(&Value) -> Option<BoxFuture<()>> + Send + Sync>;

/// A sink that records every frame, then runs a hook that may await (re-enter the
/// bridge, or wait on a barrier) before the send completes: the Python tests'
/// `RecordingClient` subclasses with their own `send`.
struct HookSink {
    sent: Mutex<Vec<String>>,
    hook: Hook,
}

impl HookSink {
    fn new(hook: impl Fn(&Value) -> Option<BoxFuture<()>> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { sent: Mutex::new(Vec::new()), hook: Box::new(hook) })
    }

    fn frames(&self) -> Vec<Value> {
        self.sent.lock().unwrap().iter().map(|t| pyjson::loads(t).unwrap()).collect()
    }

    fn types(&self) -> Vec<String> {
        self.frames().iter().filter_map(|f| frame_type(f).map(str::to_owned)).collect()
    }
}

impl ClientSink for HookSink {
    fn send(&self, text: String) -> BoxFuture<Result<(), SendError>> {
        let frame = pyjson::loads(&text).unwrap();
        self.sent.lock().unwrap().push(text);
        let then = (self.hook)(&frame);
        Box::pin(async move {
            if let Some(then) = then {
                then.await;
            }
            Ok(())
        })
    }

    fn close(&self, _code: u16, _reason: String) -> BoxFuture<Result<(), SendError>> {
        Box::pin(async { Ok(()) })
    }
}

/// A send of frames of `kind` that announces itself and waits for a release.
struct Barrier {
    started: Arc<Notify>,
    release: Arc<Semaphore>,
}

impl Barrier {
    fn new() -> Self {
        Self { started: Arc::new(Notify::new()), release: Arc::new(Semaphore::new(0)) }
    }

    fn sink(&self, kind: &'static str) -> Arc<HookSink> {
        self.sink_when(move |f| frame_type(f) == Some(kind))
    }

    fn sink_when(&self, blocks: impl Fn(&Value) -> bool + Send + Sync + 'static) -> Arc<HookSink> {
        let started = self.started.clone();
        let release = self.release.clone();
        HookSink::new(move |frame| {
            if !blocks(frame) {
                return None;
            }
            started.notify_one();
            let release = release.clone();
            Some(Box::pin(async move {
                let _permit = release.acquire().await;
            }))
        })
    }

    async fn wait_started(&self) {
        tokio::time::timeout(Duration::from_secs(1), self.started.notified()).await.expect("the send never started");
    }

    fn release(&self) {
        self.release.add_permits(1);
    }
}

/// `bridge._process_message(frame)` on its own task.
fn spawn_process(
    bridge: &Arc<Bridge>,
    frame: &str,
    client: Option<Arc<Client>>,
) -> tokio::task::JoinHandle<Result<(), String>> {
    let bridge = bridge.clone();
    let data = map(frame);
    tokio::spawn(async move { bridge.process_message(&data, client.as_ref()).await })
}

/// `_speaker_update(pid, name, since_s)` (and `_local_speaker_update`), stamped `now`.
fn speaker_update(pid: &str, name: &str, since_s: Option<f64>, is_local: bool, now: f64) -> String {
    let since = since_s.map_or_else(String::new, |s| format!(r#", "since": {:.3}"#, s * 1000.0));
    format!(
        r#"{{"type": "speaker_update", "speaker": {{"id": "{pid}", "name": "{name}", "isLocal": {is_local}{since}}}, "timestamp": {:.3}}}"#,
        now * 1000.0
    )
}

fn set_last_history_timestamp(h: &Harness, timestamp: f64) {
    let () = h.bridge.with_speakers(|s| s.history.last_mut().expect("history").timestamp = timestamp);
}

/// `_history_with_alice_then_grace`: Alice spoke from -3 s and was re-reported at
/// -0.5 s (adapter grace).
async fn history_with_alice_then_grace(h: &Harness) -> f64 {
    let now = h.bridge.now();
    h.process(&speaker_update("a", "Alice", None, false, now), None).await.unwrap();
    set_last_history_timestamp(h, now - 3.0);
    h.process(&speaker_update("a", "Alice", None, false, now), None).await.unwrap();
    set_last_history_timestamp(h, now - 0.5);
    now
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// `_recv_type(ws, want)`.
async fn recv_type(ws: &mut Ws, want: &str) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap();
        if let Message::Text(text) = message {
            let frame = pyjson::loads(text.as_str()).unwrap();
            if frame_type(&frame) == Some(want) {
                return frame;
            }
        }
    }
}

// ---- real sockets ----

#[test]
fn given_real_server_with_subscriber_and_supervisor_when_spawn_and_stop_sent_then_supervisor_gets_each_frame_and_mirror_follows()
 {
    // given
    let h = Harness::new();
    let addrs = h.bridge.start().unwrap();
    let url = format!("ws://{}", addrs[0]);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let (mut supervisor, _) = tokio_tungstenite::connect_async(url.as_str()).await.unwrap();
        supervisor.send(Message::text(r#"{"type": "supervisor_hello"}"#)).await.unwrap();
        recv_type(&mut supervisor, "meeting_state").await;
        let (mut subscriber, _) = tokio_tungstenite::connect_async(url.as_str()).await.unwrap();
        subscriber.send(Message::text(r#"{"type": "subscriber_hello"}"#)).await.unwrap();
        let spawn =
            r#"{"type": "copilot_spawn_request", "request_id": "req-real-spawn", "opaque": {"future_protocol_field": true}}"#;
        // when
        subscriber.send(Message::text(spawn)).await.unwrap();
        // then
        assert_eq!(recv_type(&mut supervisor, "copilot_spawn_request").await, json(spawn));
        assert!(wait_for(|| h.bridge.manual_copilot_active()).await);
        // when
        let stop = r#"{"type": "copilot_stop_request", "request_id": "req-real-stop", "opaque": "preserved"}"#;
        subscriber.send(Message::text(stop)).await.unwrap();
        // then
        assert_eq!(recv_type(&mut supervisor, "copilot_stop_request").await, json(stop));
        assert!(wait_for(|| !h.bridge.manual_copilot_active()).await);
        subscriber.close(None).await.unwrap();
        supervisor.close(None).await.unwrap();
    });
    h.bridge.stop();
}

#[test]
fn given_real_server_with_subscriber_and_no_supervisor_when_spawn_sent_then_same_socket_gets_supervisor_disconnected() {
    // given
    let h = Harness::new();
    let addrs = h.bridge.start().unwrap();
    let url = format!("ws://{}", addrs[0]);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let rejected = runtime.block_on(async {
        let (mut subscriber, _) = tokio_tungstenite::connect_async(url.as_str()).await.unwrap();
        subscriber.send(Message::text(r#"{"type": "subscriber_hello"}"#)).await.unwrap();
        // when
        subscriber
            .send(Message::text(r#"{"type": "copilot_spawn_request", "request_id": "req-real-reject"}"#))
            .await
            .unwrap();
        let mut rejected = recv_type(&mut subscriber, "copilot_status").await;
        while status_of(&rejected) != Some("supervisor_disconnected") {
            rejected = recv_type(&mut subscriber, "copilot_status").await;
        }
        subscriber.close(None).await.unwrap();
        rejected
    });
    h.bridge.stop();
    // then
    assert_eq!(str_field(&rejected, "request_id"), Some("req-real-reject"));
}

// ---- spawn / stop ----

#[tokio::test]
async fn given_supervisor_and_no_conference_when_spawn_request_then_relayed_verbatim_and_manual_flag_armed() {
    // given
    let (h, sup, _sub) = bridge_with_supervisor();
    let (origin, origin_client) = loose(1);
    let request =
        r#"{"type": "copilot_spawn_request", "request_id": "req-spawn", "opaque": {"future_protocol_field": true}}"#;
    // when
    h.process(request, Some(&origin_client)).await.unwrap();
    // then
    assert_eq!(copilot_frames(&sup, "copilot_spawn_request", None), [json(request)]);
    assert!(h.bridge.manual_copilot_active());
    assert!(origin.frames().is_empty());
}

#[tokio::test]
async fn given_active_conference_when_spawn_request_then_nothing_relayed_and_originator_alone_gets_conference_active() {
    // given
    let (h, sup, sub) = bridge_with_supervisor();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "ktalk"));
    let _ext = h.add(Role::Extension);
    let (origin, origin_client) = loose(1);
    // when
    h.process(r#"{"type": "copilot_spawn_request", "request_id": "req-conf"}"#, Some(&origin_client)).await.unwrap();
    // then
    assert!(sup.frames().is_empty());
    assert!(!h.bridge.manual_copilot_active());
    let rejected = copilot_frames(&origin, "copilot_status", Some("conference_active"));
    assert_eq!(rejected.len(), 1);
    assert_eq!(str_field(&rejected[0], "request_id"), Some("req-conf"));
    assert!(sub.frames().is_empty());
}

#[tokio::test]
async fn given_no_supervisor_when_spawn_request_then_originator_gets_supervisor_disconnected_and_flag_stays_off() {
    // given
    let h = Harness::new();
    let (sub, _) = h.add(Role::Subscriber);
    let (origin, origin_client) = loose(1);
    // when
    h.process(r#"{"type": "copilot_spawn_request", "request_id": "req-nosup"}"#, Some(&origin_client)).await.unwrap();
    // then
    let frames = copilot_frames(&origin, "copilot_status", Some("supervisor_disconnected"));
    assert_eq!(frames.len(), 1);
    assert_eq!(str_field(&frames[0], "request_id"), Some("req-nosup"));
    assert!(!h.bridge.manual_copilot_active());
    assert!(sub.frames().is_empty());
}

#[tokio::test]
async fn given_live_manual_session_when_stop_request_then_relayed_verbatim_and_flag_and_zero_anchor_cleared() {
    // given
    let (h, sup, _sub) = bridge_with_supervisor();
    h.bridge.force_manual_copilot_active(true);
    let () = h.bridge.with_agent(|a| a.sessions_zero_since = Some(1.0));
    let (origin, origin_client) = loose(1);
    let request =
        r#"{"type": "copilot_stop_request", "request_id": "req-stop", "opaque": {"future_protocol_field": true}}"#;
    // when
    h.process(request, Some(&origin_client)).await.unwrap();
    // then
    assert_eq!(copilot_frames(&sup, "copilot_stop_request", None), [json(request)]);
    assert!(!h.bridge.manual_copilot_active());
    assert_eq!(h.bridge.with_agent(|a| a.sessions_zero_since), None);
    assert!(origin.frames().is_empty());
}

#[tokio::test]
async fn given_active_conference_when_stop_request_then_still_relayed_without_conference_active() {
    // given
    let (h, sup, _sub) = bridge_with_supervisor();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "ktalk"));
    let _ext = h.add(Role::Extension);
    let (origin, origin_client) = loose(1);
    let request = r#"{"type": "copilot_stop_request", "request_id": "req-stop-conf", "opaque": "preserved"}"#;
    // when
    h.process(request, Some(&origin_client)).await.unwrap();
    // then
    assert_eq!(copilot_frames(&sup, "copilot_stop_request", None), [json(request)]);
    assert!(copilot_frames(&origin, "copilot_status", Some("conference_active")).is_empty());
}

#[tokio::test]
async fn given_live_manual_session_and_no_supervisor_when_stop_request_then_supervisor_disconnected_and_flag_cleared() {
    // given
    let h = Harness::new();
    h.bridge.force_manual_copilot_active(true);
    let (origin, origin_client) = loose(1);
    // when
    h.process(r#"{"type": "copilot_stop_request", "request_id": "req-stop-nosup"}"#, Some(&origin_client))
        .await
        .unwrap();
    // then
    let frames = copilot_frames(&origin, "copilot_status", Some("supervisor_disconnected"));
    assert_eq!(frames.len(), 1);
    assert_eq!(str_field(&frames[0], "request_id"), Some("req-stop-nosup"));
    assert!(!h.bridge.manual_copilot_active());
}

#[tokio::test]
async fn given_supervisor_and_armed_flag_when_spawn_and_stop_lack_a_valid_request_id_then_each_dropped_with_a_log() {
    // given
    let (h, sup, sub) = bridge_with_supervisor();
    h.bridge.force_manual_copilot_active(true);
    let (origin, origin_client) = loose(1);
    let malformed = [
        r#"{"type": "copilot_spawn_request"}"#,
        r#"{"type": "copilot_spawn_request", "request_id": ""}"#,
        r#"{"type": "copilot_spawn_request", "request_id": 42}"#,
        r#"{"type": "copilot_stop_request"}"#,
        r#"{"type": "copilot_stop_request", "request_id": ""}"#,
        r#"{"type": "copilot_stop_request", "request_id": null}"#,
    ];
    // when
    for frame in malformed {
        h.process(frame, Some(&origin_client)).await.unwrap();
    }
    // then
    assert!(sup.frames().is_empty());
    assert!(origin.frames().is_empty());
    assert!(sub.frames().is_empty());
    assert!(h.bridge.manual_copilot_active());
    assert_eq!(h.logs().iter().filter(|l| l.contains("missing/invalid request_id")).count(), malformed.len());
}

// ---- meeting_state and the supervisor's hello ----

#[test]
fn given_armed_manual_flag_and_no_conference_when_meeting_state_computed_then_manual_meeting_reported() {
    // given
    let h = Harness::new();
    h.bridge.force_manual_copilot_active(true);
    // when
    let frame = h.bridge.compute_meeting_state();
    // then
    assert_eq!(
        frame,
        json(r#"{"type": "meeting_state", "active": true, "meeting_id": "manual", "meeting_title": null}"#)
    );
}

#[test]
fn given_armed_manual_flag_and_active_conference_when_meeting_state_computed_then_conference_reported() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "ktalk"));
    let _ext = h.add(Role::Extension);
    h.bridge.force_manual_copilot_active(true);
    // when
    let frame = h.bridge.compute_meeting_state();
    // then
    assert_eq!(field(&frame, "active"), &Value::Bool(true));
    assert_eq!(str_field(&frame, "meeting_id"), Some("m-1"));
    assert_eq!(str_field(&frame, "meeting_title"), Some("Standup"));
}

#[tokio::test]
async fn given_lost_manual_flag_and_no_conference_when_supervisor_hello_reports_manual_then_flag_rearmed_before_reply()
{
    // given
    let h = Harness::new();
    let (sup, sup_client) = loose(1);
    // when
    h.process(r#"{"type": "supervisor_hello", "current_meeting_id": "manual"}"#, Some(&sup_client)).await.unwrap();
    // then
    assert!(h.bridge.manual_copilot_active());
    let states = copilot_frames(&sup, "meeting_state", None);
    assert_eq!(states.len(), 1);
    assert_eq!(field(&states[0], "active"), &Value::Bool(true));
    assert_eq!(str_field(&states[0], "meeting_id"), Some("manual"));
}

#[tokio::test]
async fn given_active_conference_when_supervisor_hello_reports_manual_then_flag_stays_off_and_conference_reported() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "ktalk"));
    let _ext = h.add(Role::Extension);
    let (sup, sup_client) = loose(1);
    // when
    h.process(r#"{"type": "supervisor_hello", "current_meeting_id": "manual"}"#, Some(&sup_client)).await.unwrap();
    // then
    assert!(!h.bridge.manual_copilot_active());
    let states = copilot_frames(&sup, "meeting_state", None);
    assert_eq!(states.len(), 1);
    assert_eq!(field(&states[0], "active"), &Value::Bool(true));
    assert_eq!(str_field(&states[0], "meeting_id"), Some("m-1"));
}

// ---- preemption by a conference ----

#[tokio::test]
async fn given_live_manual_session_when_conference_meeting_starts_then_flag_cleared_and_subscribers_alone_notified() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-conf"));
    let (ext, _) = h.add(Role::Extension);
    let (sub, _) = h.add(Role::Subscriber);
    h.bridge.force_manual_copilot_active(true);
    // when
    h.process(JOIN, None).await.unwrap();
    // then
    assert!(!h.bridge.manual_copilot_active());
    let notices = copilot_frames(&sub, "copilot_status", Some("manual_preempted"));
    assert_eq!(notices.len(), 1);
    assert_eq!(str_field(&notices[0], "request_id"), Some(""));
    assert!(copilot_frames(&ext, "copilot_status", Some("manual_preempted")).is_empty());
}

#[tokio::test]
async fn given_no_manual_session_when_conference_meeting_starts_then_no_preemption_notice() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-conf"));
    let _ext = h.add(Role::Extension);
    let (sub, _) = h.add(Role::Subscriber);
    // when
    h.process(JOIN, None).await.unwrap();
    // then
    assert!(copilot_frames(&sub, "copilot_status", Some("manual_preempted")).is_empty());
}

#[tokio::test]
async fn given_dead_and_healthy_subscriber_when_manual_session_preempted_then_healthy_one_still_notified() {
    // given
    let h = Harness::new();
    let dead = RecordingSink::with_mode(SendMode::Fail("socket is gone".into()));
    h.bridge.add_client(Role::Subscriber, dead, "('127.0.0.1', 12345)");
    let (healthy, _) = h.add(Role::Subscriber);
    h.bridge.force_manual_copilot_active(true);
    // when
    h.bridge.preempt_manual_copilot(true).await;
    // then
    assert_eq!(copilot_frames(&healthy, "copilot_status", Some("manual_preempted")).len(), 1);
    assert!(!h.bridge.manual_copilot_active());
    assert!(h.logged("Failed to send copilot_status to subscriber: socket is gone"));
}

#[tokio::test]
async fn given_manual_session_online_with_zero_claude_sessions_when_zero_outlasts_debounce_then_chip_goes_offline() {
    // given
    let h = Harness::new();
    let (sub, _) = h.add(Role::Subscriber);
    h.bridge.force_manual_copilot_active(true);
    let () = h.bridge.with_agent(|a| {
        a.supervisor_status = Some("online".into());
        a.supervisor_status_ts = 100.0;
        a.claude_sessions = Some(0);
    });
    // when
    h.bridge.recompute_agent_state(Some(100.0)).await;
    let () = h.bridge.with_agent(|a| a.supervisor_status_ts = 111.0);
    h.bridge.recompute_agent_state(Some(111.0)).await;
    // then
    assert_eq!(agent_states(&sub), ["online", "offline"]);
}

// ---- manual chat ----

#[tokio::test]
async fn given_no_conference_and_orphan_meeting_when_chat_message_forwarded_then_no_meeting_id_and_pending_scope_empty()
{
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("orphan-1", "Standup", "orphan"));
    let (channel, _) = h.add(Role::Channel);
    let (_origin, origin_client) = loose(1);
    // when
    h.process(
        r#"{"type": "copilot_chat_message", "request_id": "req-manual", "text": "что было сказано?"}"#,
        Some(&origin_client),
    )
    .await
    .unwrap();
    // then
    let forwarded = copilot_frames(&channel, "copilot_user_message", None);
    assert_eq!(forwarded.len(), 1);
    assert_eq!(field(&forwarded[0], "meeting_id"), &Value::Null);
    assert_eq!(pending(&h, "req-manual").as_deref(), Some(""));
}

#[tokio::test]
async fn given_manual_chat_in_flight_when_orphan_meeting_rotates_before_reply_then_reply_still_broadcast() {
    // given
    let h = Harness::new();
    let storage = ktalk_storage("orphan-1", "Standup", "orphan");
    h.bridge.set_meeting_storage(storage.clone());
    let (channel, _) = h.add(Role::Channel);
    let (sub, _) = h.add(Role::Subscriber);
    let (_origin, origin_client) = loose(1);
    h.process(r#"{"type": "copilot_chat_message", "request_id": "req-rot", "text": "вопрос"}"#, Some(&origin_client))
        .await
        .unwrap();
    let echoed_meeting_id = field(&copilot_frames(&channel, "copilot_user_message", None)[0], "meeting_id").clone();
    // when
    *storage.current_id.lock().unwrap() = Some("orphan-2".into());
    let reply = format!(
        r#"{{"type": "copilot_channel_reply", "request_id": "req-rot", "kind": "manual", "text": "ответ", "meeting_id": {}}}"#,
        pyjson::dumps(&echoed_meeting_id, None)
    );
    h.process(&reply, None).await.unwrap();
    // then
    let replies = copilot_frames(&sub, "copilot_reply", None);
    assert_eq!(replies.len(), 1);
    assert_eq!(str_field(&replies[0], "text"), Some("ответ"));
}

// ---- spawn against dead supervisors and conference races ----

#[tokio::test]
async fn given_only_a_dead_supervisor_when_spawn_request_then_supervisor_disconnected_and_flag_stays_off() {
    // given
    let h = Harness::new();
    let dead = RecordingSink::with_mode(SendMode::Fail("socket is gone".into()));
    h.bridge.add_client(Role::Supervisor, dead, "('127.0.0.1', 12345)");
    let (origin, origin_client) = loose(1);
    // when
    h.process(r#"{"type": "copilot_spawn_request", "request_id": "req-dead"}"#, Some(&origin_client)).await.unwrap();
    // then
    let frames = copilot_frames(&origin, "copilot_status", Some("supervisor_disconnected"));
    assert_eq!(frames.len(), 1);
    assert_eq!(str_field(&frames[0], "request_id"), Some("req-dead"));
    assert!(!h.bridge.manual_copilot_active());
}

#[tokio::test]
async fn given_dead_and_live_supervisor_when_spawn_request_then_live_one_gets_it_and_flag_armed() {
    // given
    let h = Harness::new();
    let dead = RecordingSink::with_mode(SendMode::Fail("socket is gone".into()));
    h.bridge.add_client(Role::Supervisor, dead, "('127.0.0.1', 12345)");
    let (live, _) = h.add(Role::Supervisor);
    let (origin, origin_client) = loose(1);
    // when
    h.process(r#"{"type": "copilot_spawn_request", "request_id": "req-mixed"}"#, Some(&origin_client)).await.unwrap();
    // then
    assert_eq!(
        copilot_frames(&live, "copilot_spawn_request", None),
        [json(r#"{"type": "copilot_spawn_request", "request_id": "req-mixed"}"#)]
    );
    assert!(h.bridge.manual_copilot_active());
    assert!(origin.frames().is_empty());
}

#[tokio::test]
async fn given_no_conference_at_guard_when_join_completes_during_relay_then_flag_not_armed_and_conference_active() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(orphan_then_conference("orphan-1"));
    let _ext = h.add(Role::Extension);
    let bridge = h.bridge.clone();
    let sup = HookSink::new(move |_| {
        let bridge = bridge.clone();
        Some(Box::pin(async move {
            let _ = bridge.process_message(&map(JOIN), None).await;
        }))
    });
    h.bridge.add_client(Role::Supervisor, sup.clone(), "('127.0.0.1', 12345)");
    let (origin, origin_client) = loose(1);
    assert!(!h.bridge.is_conference_active());
    // when
    h.process(r#"{"type": "copilot_spawn_request", "request_id": "req-race"}"#, Some(&origin_client)).await.unwrap();
    // then
    assert!(!frames_of(sup.frames(), "copilot_spawn_request", None).is_empty());
    assert!(h.bridge.is_conference_active());
    assert!(!h.bridge.manual_copilot_active());
    let rejected = copilot_frames(&origin, "copilot_status", Some("conference_active"));
    assert_eq!(rejected.len(), 1);
    assert_eq!(str_field(&rejected[0], "request_id"), Some("req-race"));
}

#[tokio::test]
async fn given_two_supervisors_and_no_conference_when_conference_starts_after_first_send_then_late_supervisor_gets_no_spawn()
 {
    // given
    let h = Harness::new();
    let storage = orphan_then_conference("orphan-1");
    h.bridge.set_meeting_storage(storage.clone());
    let _ext = h.add(Role::Extension);
    let first = HookSink::new(move |_| {
        storage.set_meta(Ok(json(r#"{"source": "ktalk", "title": "Standup"}"#)));
        None
    });
    h.bridge.add_client(Role::Supervisor, first.clone(), "('127.0.0.1', 12345)");
    let (late, _) = h.add(Role::Supervisor);
    let (origin, origin_client) = loose(1);
    let request = r#"{"type": "copilot_spawn_request", "request_id": "req-fanout-race"}"#;
    // when
    h.process(request, Some(&origin_client)).await.unwrap();
    // then
    assert_eq!(frames_of(first.frames(), "copilot_spawn_request", None), [json(request)]);
    assert!(copilot_frames(&late, "copilot_spawn_request", None).is_empty());
    let rejected = copilot_frames(&origin, "copilot_status", Some("conference_active"));
    assert_eq!(rejected.len(), 1);
    assert_eq!(str_field(&rejected[0], "request_id"), Some("req-fanout-race"));
    assert!(!h.bridge.manual_copilot_active());
}

#[tokio::test]
async fn given_spawn_relay_suspended_when_stop_arrives_and_spawn_resumes_then_supervisor_sees_spawn_then_stop_and_flag_off()
 {
    // given
    let h = Harness::new();
    let barrier = Barrier::new();
    let sup = barrier.sink("copilot_spawn_request");
    h.bridge.add_client(Role::Supervisor, sup.clone(), "('127.0.0.1', 12345)");
    let spawn = spawn_process(
        &h.bridge,
        r#"{"type": "copilot_spawn_request", "request_id": "req-spawn-race"}"#,
        Some(loose(1).1),
    );
    barrier.wait_started().await;
    let stop = spawn_process(
        &h.bridge,
        r#"{"type": "copilot_stop_request", "request_id": "req-stop-race"}"#,
        Some(loose(2).1),
    );
    settle().await;
    assert_eq!(sup.types(), ["copilot_spawn_request"]);
    // when
    barrier.release();
    spawn.await.unwrap().unwrap();
    stop.await.unwrap().unwrap();
    // then
    assert_eq!(sup.types(), ["copilot_spawn_request", "copilot_stop_request"]);
    assert!(!h.bridge.manual_copilot_active());
}

#[tokio::test]
async fn given_stop_relay_suspended_when_reconnecting_supervisor_rearms_manual_then_completed_stop_wins() {
    // given
    let h = Harness::new();
    h.bridge.force_manual_copilot_active(true);
    let barrier = Barrier::new();
    let sup = barrier.sink("copilot_stop_request");
    h.bridge.add_client(Role::Supervisor, sup, "('127.0.0.1', 12345)");
    let stop = spawn_process(
        &h.bridge,
        r#"{"type": "copilot_stop_request", "request_id": "req-stop-hello-race"}"#,
        Some(loose(1).1),
    );
    barrier.wait_started().await;
    // when
    let (_reconnecting, reconnecting_client) = loose(2);
    h.process(r#"{"type": "supervisor_hello", "current_meeting_id": "manual"}"#, Some(&reconnecting_client))
        .await
        .unwrap();
    assert!(h.bridge.manual_copilot_active());
    barrier.release();
    stop.await.unwrap().unwrap();
    // then
    assert!(!h.bridge.manual_copilot_active());
    assert_eq!(field(&h.bridge.compute_meeting_state(), "active"), &Value::Bool(false));
}

/// Ported for intent: Python rebinds its `asyncio.Lock` to each new event loop; the
/// Rust lock belongs to no runtime, so the same bridge must serve contending manual
/// requests on a runtime started after the first one ended.
#[test]
fn given_manual_lock_contended_on_one_runtime_when_reused_on_a_new_runtime_then_requests_still_complete() {
    // given
    let h = Harness::new();
    let yielding = HookSink::new(|_| Some(Box::pin(tokio::task::yield_now())));
    h.bridge.add_client(Role::Supervisor, yielding.clone(), "('127.0.0.1', 12345)");
    let contend = |bridge: Arc<Bridge>| async move {
        let spawn = spawn_process(&bridge, r#"{"type": "copilot_spawn_request", "request_id": "s"}"#, Some(loose(1).1));
        let stop = spawn_process(&bridge, r#"{"type": "copilot_stop_request", "request_id": "t"}"#, Some(loose(2).1));
        spawn.await.unwrap().unwrap();
        stop.await.unwrap().unwrap();
    };
    for _ in 0..2 {
        // when
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let bridge = h.bridge.clone();
        runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), contend(bridge)).await })
            .expect("deadlock");
    }
    // then
    assert_eq!(
        yielding.types(),
        ["copilot_spawn_request", "copilot_stop_request", "copilot_spawn_request", "copilot_stop_request"]
    );
    assert!(!h.bridge.manual_copilot_active());
}

// ---- terminal echoes of dropped replies ----

#[tokio::test]
async fn given_stale_tracked_request_when_reply_arrives_then_one_terminal_error_to_each_audience_and_entry_popped() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    h.bridge.set_pending_request("req-late", "stale:m-prev");
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-late", "kind": "manual", "text": "поздняя сводка"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    for client in [&ext, &sub] {
        assert!(copilot_frames(client, "copilot_reply", None).is_empty());
        let errors = copilot_frames(client, "copilot_status", Some("error"));
        assert_eq!(errors.len(), 1);
        assert_eq!(str_field(&errors[0], "request_id"), Some("req-late"));
        assert_eq!(str_field(&errors[0], "message"), Some(MSG_MEETING_ENDED));
    }
    assert_eq!(pending(&h, "req-late"), None);
    assert!(h.logged("copilot_channel_reply for stale request_id=req-late (kind=manual) — dropped"));
}

#[tokio::test]
async fn given_tracked_request_of_previous_meeting_when_reply_carries_it_then_one_terminal_meeting_changed_error_each()
{
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    h.bridge.set_meeting_storage(ktalk_storage("m-new", "Standup", "orphan"));
    h.bridge.set_pending_request("req-x", "m-prev");
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-x", "kind": "manual", "text": "ответ из прошлой встречи", "meeting_id": "m-prev"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    for client in [&ext, &sub] {
        assert!(copilot_frames(client, "copilot_reply", None).is_empty());
        let errors = copilot_frames(client, "copilot_status", Some("error"));
        assert_eq!(errors.len(), 1);
        assert_eq!(str_field(&errors[0], "request_id"), Some("req-x"));
        assert_eq!(str_field(&errors[0], "message"), Some(MSG_MEETING_CHANGED));
    }
    assert_eq!(pending(&h, "req-x"), None);
    assert!(h.logged("copilot_channel_reply for meeting_id=m-prev but current is m-new (kind=manual) — dropped"));
}

#[tokio::test]
async fn given_untracked_proactive_reply_of_another_meeting_when_it_arrives_then_nothing_broadcast() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    h.bridge.set_meeting_storage(ktalk_storage("m-now", "Standup", "orphan"));
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "proactive_abc123", "kind": "proactive", "text": "заметка из прошлой встречи", "meeting_id": "m-old"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert!(ext.frames().is_empty());
    assert!(sub.frames().is_empty());
    assert!(h.logged("copilot_channel_reply for meeting_id=m-old but current is m-now (kind=proactive) — dropped"));
}

#[tokio::test]
async fn given_pending_requests_of_every_scope_when_manual_session_preempted_then_only_manual_ones_marked_stale() {
    // given
    let h = Harness::new();
    h.bridge.force_manual_copilot_active(true);
    h.bridge.set_pending_request("req-m", "");
    h.bridge.set_pending_request("req-c", "m-cur");
    h.bridge.set_pending_request("req-s", "stale:m-old");
    // when
    h.bridge.preempt_manual_copilot(true).await;
    // then
    assert_eq!(pending(&h, "req-m").as_deref(), Some("stale:"));
    assert_eq!(pending(&h, "req-c").as_deref(), Some("m-cur"));
    assert_eq!(pending(&h, "req-s").as_deref(), Some("stale:m-old"));
    assert!(h.logged("Marked 1 manual copilot request(s) stale on preemption"));
}

#[tokio::test]
async fn given_no_manual_session_and_manual_pending_entry_when_preempt_runs_then_entry_untouched() {
    // given
    let h = Harness::new();
    h.bridge.set_pending_request("req-m", "");
    // when
    h.bridge.preempt_manual_copilot(true).await;
    // then
    assert_eq!(pending(&h, "req-m").as_deref(), Some(""));
}

#[tokio::test]
async fn given_manual_request_pending_when_conference_preempts_and_reply_arrives_then_terminal_manual_preempted_error()
{
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-conf"));
    let (ext, _) = h.add(Role::Extension);
    let (sub, _) = h.add(Role::Subscriber);
    h.bridge.force_manual_copilot_active(true);
    h.bridge.set_pending_request("req-manual", "");
    h.process(JOIN, None).await.unwrap();
    assert_eq!(pending(&h, "req-manual").as_deref(), Some("stale:"));
    ext.clear();
    sub.clear();
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-manual", "kind": "manual", "text": "поздний ответ ручной сессии", "meeting_id": null}"#,
        None,
    )
    .await
    .unwrap();
    // then
    for client in [&ext, &sub] {
        assert!(copilot_frames(client, "copilot_reply", None).is_empty());
        let errors = copilot_frames(client, "copilot_status", Some("error"));
        assert_eq!(errors.len(), 1);
        assert_eq!(str_field(&errors[0], "request_id"), Some("req-manual"));
        assert_eq!(str_field(&errors[0], "message"), Some(MSG_MANUAL_PREEMPTED));
        assert_ne!(str_field(&errors[0], "message"), Some(MSG_MEETING_ENDED));
    }
    assert_eq!(pending(&h, "req-manual"), None);
}

#[tokio::test]
async fn given_channel_replying_from_inside_meeting_started_when_conference_joins_then_reply_is_stale() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-conf"));
    h.bridge.force_manual_copilot_active(true);
    h.bridge.set_pending_request("req-reentrant", "");
    let replied = Arc::new(AtomicBool::new(false));
    let (bridge, flag) = (h.bridge.clone(), replied.clone());
    let channel = HookSink::new(move |frame| {
        if frame_type(frame) != Some("meeting_started") || flag.swap(true, Ordering::SeqCst) {
            return None;
        }
        let bridge = bridge.clone();
        Some(Box::pin(async move {
            let reply = r#"{"type": "copilot_channel_reply", "request_id": "req-reentrant", "kind": "manual", "text": "слишком поздний ответ", "meeting_id": null}"#;
            bridge.process_message(&map(reply), None).await.unwrap();
        }))
    });
    h.bridge.add_client(Role::Channel, channel.clone(), "('127.0.0.1', 12345)");
    // when
    h.process(JOIN, None).await.unwrap();
    // then
    assert!(replied.load(Ordering::SeqCst));
    assert_eq!(frames_of(channel.frames(), "meeting_started", None).len(), 1);
    for client in [&ext, &sub] {
        assert!(copilot_frames(client, "copilot_reply", None).is_empty());
        let errors = copilot_frames(client, "copilot_status", Some("error"));
        assert_eq!(errors.len(), 1);
        assert_eq!(str_field(&errors[0], "request_id"), Some("req-reentrant"));
        assert_eq!(str_field(&errors[0], "message"), Some(MSG_MANUAL_PREEMPTED));
    }
    assert_eq!(pending(&h, "req-reentrant"), None);
}

#[tokio::test]
async fn given_channel_joining_a_conference_from_inside_chat_relay_when_late_reply_arrives_then_pending_stays_stale() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    let sub_client = h.bridge.clients_in(Role::Subscriber)[0];
    h.bridge.set_meeting_storage(orphan_then_conference("orphan"));
    h.bridge.force_manual_copilot_active(true);
    type Seen = Arc<Mutex<(Option<String>, Option<String>)>>;
    let seen: Seen = Arc::new(Mutex::new((None, None)));
    let joined = Arc::new(AtomicBool::new(false));
    let (bridge, seen_in, flag) = (h.bridge.clone(), seen.clone(), joined.clone());
    let channel = HookSink::new(move |frame| {
        if frame_type(frame) != Some("copilot_user_message") || flag.swap(true, Ordering::SeqCst) {
            return None;
        }
        let lookup = |b: &Bridge| b.pending_requests().into_iter().find(|(k, _)| k == "req-race").map(|(_, v)| v);
        seen_in.lock().unwrap().0 = lookup(&bridge);
        let (bridge, seen_in) = (bridge.clone(), seen_in.clone());
        Some(Box::pin(async move {
            bridge.process_message(&map(JOIN), None).await.unwrap();
            seen_in.lock().unwrap().1 = lookup(&bridge);
        }))
    });
    h.bridge.add_client(Role::Channel, channel.clone(), "('127.0.0.1', 12345)");
    let sub_handle = Client::new(sub_client, sub.clone(), "('127.0.0.1', 12345)");
    // when
    h.process(
        r#"{"type": "copilot_chat_message", "request_id": "req-race", "text": "ручной вопрос"}"#,
        Some(&sub_handle),
    )
    .await
    .unwrap();
    // then
    let (before, after) = seen.lock().unwrap().clone();
    assert_eq!(before.as_deref(), Some(""));
    assert_eq!(after.as_deref(), Some("stale:"));
    assert_eq!(pending(&h, "req-race").as_deref(), Some("stale:"));
    let forwarded = frames_of(channel.frames(), "copilot_user_message", None);
    assert_eq!(forwarded.len(), 1);
    assert_eq!(field(&forwarded[0], "meeting_id"), &Value::Null);
    // when
    ext.clear();
    sub.clear();
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-race", "kind": "manual", "text": "поздний ответ", "meeting_id": null}"#,
        None,
    )
    .await
    .unwrap();
    // then
    for client in [&ext, &sub] {
        assert!(copilot_frames(client, "copilot_reply", None).is_empty());
        let errors = copilot_frames(client, "copilot_status", Some("error"));
        assert_eq!(errors.len(), 1);
        assert_eq!(str_field(&errors[0], "request_id"), Some("req-race"));
        assert_eq!(str_field(&errors[0], "message"), Some(MSG_MANUAL_PREEMPTED));
    }
    assert_eq!(pending(&h, "req-race"), None);
}

#[tokio::test]
async fn given_subscriber_blocking_on_preemption_notice_when_conference_joins_then_lifecycle_already_delivered() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-conf"));
    let (channel, _) = h.add(Role::Channel);
    let (extension, _) = h.add(Role::Extension);
    let (supervisor, _) = h.add(Role::Supervisor);
    h.bridge.force_manual_copilot_active(true);
    h.bridge.set_pending_request("req-manual", "");
    let barrier = Barrier::new();
    let subscriber = barrier.sink_when(|f| status_of(f) == Some("manual_preempted"));
    h.bridge.add_client(Role::Subscriber, subscriber, "('127.0.0.1', 12345)");
    // when
    let join = spawn_process(&h.bridge, JOIN, None);
    barrier.wait_started().await;
    // then
    for recipient in [&channel, &extension, &supervisor] {
        let started = copilot_frames(recipient, "meeting_started", None);
        assert_eq!(started.len(), 1);
        assert_eq!(str_field(&started[0], "meeting_id"), Some("m-conf"));
    }
    assert!(!h.bridge.manual_copilot_active());
    assert_eq!(pending(&h, "req-manual").as_deref(), Some("stale:"));
    assert!(!join.is_finished());
    barrier.release();
    tokio::time::timeout(Duration::from_secs(1), join).await.unwrap().unwrap().unwrap();
}

// ---- channel recovery ----

#[tokio::test]
async fn given_online_tui_without_mcp_session_when_zero_persists_past_recovery_window_then_one_throttled_recovery_request()
 {
    // given
    let (h, _ext) = bridge_with_active_ktalk();
    let (sup, _) = h.add(Role::Supervisor);
    let () = h.bridge.with_agent(|a| {
        a.supervisor_status = Some("online".into());
        a.claude_sessions = Some(0);
    });
    let poll = |now: f64| {
        let () = h.bridge.with_agent(|a| a.supervisor_status_ts = now);
        h.bridge.recompute_agent_state(Some(now))
    };
    let recoveries = || copilot_frames(&sup, "copilot_channel_unavailable", None);
    // when
    for now in [100.0, 111.0, 159.0] {
        poll(now).await;
    }
    // then
    assert!(recoveries().is_empty());
    // when
    poll(161.0).await;
    // then
    assert_eq!(recoveries(), [json(r#"{"type": "copilot_channel_unavailable", "meeting_id": "m-1"}"#)]);
    // when
    poll(165.0).await;
    // then
    assert_eq!(recoveries().len(), 1);
}

async fn assert_no_recovery(count: Option<i128>, status: &str) {
    // given
    let (h, _ext) = bridge_with_active_ktalk();
    let (sup, _) = h.add(Role::Supervisor);
    let () = h.bridge.with_agent(|a| {
        a.supervisor_status = Some(status.into());
        a.claude_sessions = count;
    });
    // when
    for now in [100.0, 161.0, 222.0] {
        let () = h.bridge.with_agent(|a| a.supervisor_status_ts = now);
        h.bridge.recompute_agent_state(Some(now)).await;
    }
    // then
    assert!(sup.frames().is_empty(), "{count:?} {status}");
}

#[tokio::test]
async fn given_online_agent_with_unknown_session_count_when_polled_then_no_recovery() {
    assert_no_recovery(None, "online").await;
}

#[tokio::test]
async fn given_online_agent_with_one_session_when_polled_then_no_recovery() {
    assert_no_recovery(Some(1), "online").await;
}

#[tokio::test]
async fn given_spawning_agent_with_zero_sessions_when_polled_then_no_recovery() {
    assert_no_recovery(Some(0), "spawning").await;
}

#[tokio::test]
async fn given_blocked_agent_with_zero_sessions_when_polled_then_no_recovery() {
    assert_no_recovery(Some(0), "blocked").await;
}

// ---- speaker `since` backdating ----

#[tokio::test]
async fn given_alice_rereported_through_grace_when_bob_arrives_with_since_then_bob_holds_the_window_from_since() {
    // given
    let h = Harness::new();
    let now = history_with_alice_then_grace(&h).await;
    // when
    h.process(&speaker_update("b", "Bob", Some(now - 1.0), false, now), None).await.unwrap();
    // then
    let history = h.bridge.get_speaker_history();
    let names: Vec<Option<&str>> = history.iter().map(|e| e.speaker_name.as_deref()).collect();
    assert_eq!(names, [Some("Alice"), Some("Bob")]);
    assert!((history.last().unwrap().timestamp - (now - 1.0)).abs() <= 0.05);
    assert_eq!(h.bridge.get_dominant_speaker_for_period(now - 1.5, now).as_deref(), Some("Bob"));
}

#[tokio::test]
async fn given_alice_current_when_bob_since_is_a_minute_old_then_backdating_is_capped() {
    // given
    let h = Harness::new();
    let now = history_with_alice_then_grace(&h).await;
    // when
    h.process(&speaker_update("b", "Bob", Some(now - 60.0), false, now), None).await.unwrap();
    // then
    let history = h.bridge.get_speaker_history();
    let names: Vec<Option<&str>> = history.iter().map(|e| e.speaker_name.as_deref()).collect();
    assert_eq!(names, [Some("Alice"), Some("Bob")]);
    assert!(history.last().unwrap().timestamp >= now - SPEAKER_ONSET_MAX_BACKDATE_S - 0.05);
}

#[tokio::test]
async fn given_last_event_is_silence_when_bob_arrives_with_earlier_since_then_bob_recorded_at_arrival() {
    // given
    let h = Harness::new();
    h.process(&speaker_update("a", "Alice", None, false, NOW), None).await.unwrap();
    h.process(&format!(r#"{{"type": "speaker_update", "speaker": null, "timestamp": {:.3}}}"#, NOW * 1000.0), None)
        .await
        .unwrap();
    h.clock.advance(0.01);
    let now = h.bridge.now();
    // when
    h.process(&speaker_update("b", "Bob", Some(now - 1.0), false, now), None).await.unwrap();
    // then
    let history = h.bridge.get_speaker_history();
    let names: Vec<Option<&str>> = history.iter().map(|e| e.speaker_name.as_deref()).collect();
    assert_eq!(names, [Some("Alice"), None, Some("Bob")]);
    assert!(history.last().unwrap().timestamp >= now);
}

// ---- the user's own voice ----

#[tokio::test]
async fn given_only_the_user_speaking_when_system_audio_is_attributed_then_no_speaker() {
    // given
    let h = Harness::new();
    let now = h.bridge.now();
    h.process(&speaker_update("me", "Me", None, true, now), None).await.unwrap();
    set_last_history_timestamp(&h, now - 3.0);
    // when
    let dominant = h.bridge.get_dominant_speaker_for_period(now - 3.0, now);
    // then
    assert_eq!(dominant, None);
    assert_eq!(h.bridge.get_current_speaker(), None);
}

#[tokio::test]
async fn given_user_spoke_longer_than_alice_when_system_audio_is_attributed_then_alice() {
    // given
    let h = Harness::new();
    let now = h.bridge.now();
    h.process(&speaker_update("me", "Me", None, true, now), None).await.unwrap();
    set_last_history_timestamp(&h, now - 3.0);
    h.process(&speaker_update("a", "Alice", None, false, now), None).await.unwrap();
    set_last_history_timestamp(&h, now - 1.0);
    // when
    let dominant = h.bridge.get_dominant_speaker_for_period(now - 3.0, now);
    // then
    assert_eq!(dominant.as_deref(), Some("Alice"));
}

// ---- golden recorder ----

#[tokio::test]
async fn given_golden_recorder_when_client_sends_a_frame_and_invalid_json_and_leaves_then_session_recorded_frame_by_frame()
 {
    // given
    let h = Harness::new();
    let golden = Arc::new(RecordingGolden::default());
    h.bridge.set_golden(Some(golden.clone()));
    let (fake, task) = h.connect(None).await;
    // when
    fake.send_text(r#"{"type": "clock_sync", "timestamp": 1}"#);
    fake.send_text("not json");
    fake.close();
    task.await.unwrap();
    // then
    let events = golden.events.lock().unwrap().clone();
    let kinds: Vec<&str> = events.iter().filter_map(|e| str_field(e, "event")).collect();
    assert_eq!(kinds, ["connect", "frame", "disconnect"]);
    assert!(events.iter().all(|e| field(e, "client") == &Value::Int(1)));
    assert_eq!(str_field(field(&events[1], "data"), "type"), Some("clock_sync"));
}
