//! tests/test_ktalk_bridge.py from `test_real_bridge_delivers_meeting_started_to_supervisor`
//! through `test_multiple_stalled_clients_time_out_concurrently`: the supervisor
//! lifecycle over a real server, `meeting_state`, the agent-state merge, the Copilot
//! fan-out to extensions and subscribers, and the bounded sends (DW-6).
//!
//! The Python fakes map to: `RecordingClient` → [`RecordingSink`] in a set,
//! `StalledClient` → [`SendMode::Stall`], `StalledClientWithFailingClose` →
//! `close_error`, `HandlerStalledClient` → a [`FakeConnection`] whose sink is switched
//! to `Stall` (its pending close is the connection left open until `fake.close()`),
//! `ExplodingClient` → [`SendMode::Fail`], `BarrierStalledClient` → [`BarrierSink`]
//! below.
//!
//! Not ported:
//! - `test_given_timed_out_channel_when_handler_reaps_it_then_pending_requests_are_closed`:
//!   only its `_bg_close_tasks` assertions (one task, not done, then emptied) — the
//!   asyncio task set has no counterpart; the Rust close is spawned and the fake's
//!   close resolves at once. The rest of the case is ported.
//! - `test_given_close_task_failure_when_timeout_fires_then_failure_is_retrieved`: the
//!   wait on `_bg_close_tasks` emptying becomes a wait on the log line (same intent).
//! - `test_timed_out_client_without_close_does_not_raise`: a fake without a `close`
//!   attribute cannot exist behind `ClientSink`; ported for its intent (a stalled
//!   client does not stop the healthy one from receiving the frame).

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Harness, field, json, settle, wait_for};
use futures_util::{SinkExt, StreamExt};
use recog_bridge::hooks::{BoxFuture, Clock, MeetingStore};
use recog_bridge::obj;
use recog_bridge::testing::{FakeStore, RecordingIncrements, RecordingSink, SendMode, frame_type};
use recog_bridge::{BridgeConfig, ClientSink, Role, SendError};
use recog_core::meeting_storage::{CONFERENCE_SOURCES, MeetingStorage, StorageOptions};
use recog_core::pyjson::{self, Value};
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// ---- helpers (the Python module's fixtures) ----

/// `_serve_bridge`: a real server on a free port.
fn serve(h: &Harness) -> String {
    let addrs = h.bridge.start().expect("bind");
    format!("ws://{}", addrs[0])
}

async fn ws_connect(url: &str) -> Ws {
    tokio_tungstenite::connect_async(url).await.expect("connect").0
}

async fn ws_send(ws: &mut Ws, text: &str) {
    ws.send(Message::text(text)).await.expect("send");
}

/// `_recv_type(ws, want, timeout)`: frames until one of type `want`; None on timeout.
async fn recv_type(ws: &mut Ws, want: &str, timeout: Duration) -> Option<Value> {
    loop {
        let message = tokio::time::timeout(timeout, ws.next()).await.ok()?;
        let Some(Ok(message)) = message else { panic!("socket closed while waiting for {want}") };
        if let Message::Text(text) = message {
            let frame = pyjson::loads(text.as_str()).expect("json");
            if frame_type(&frame) == Some(want) {
                return Some(frame);
            }
        }
    }
}

async fn recv_required(ws: &mut Ws, want: &str) -> Value {
    recv_type(ws, want, Duration::from_secs(5)).await.unwrap_or_else(|| panic!("no {want}"))
}

const JOIN_ABC_STANDUP: &str =
    r#"{"type": "meeting_join", "url": "https://ktalk.test/abc", "title": "Standup", "platform": "ktalk"}"#;

/// `_meeting_storage_mock(meeting_id, new_meeting_id)`, a `Mock()` storage: a join
/// moves the current path to `/tmp/<meeting_id>.jsonl`; the id and `_load_meta` stay
/// as set, and `on_ktalk_leave` changes nothing (unlike [`FakeStore`], whose leave
/// opens an orphan — a supervisor hello with no extension would then end the meeting
/// and turn a same-room rejoin into a new start).
struct MockStore {
    meeting_id: String,
    new_meeting_id: String,
    current_path: Mutex<Option<PathBuf>>,
    meta: Mutex<Value>,
    calls: Mutex<Vec<Value>>,
}

impl MockStore {
    fn ops(&self) -> Vec<String> {
        self.calls().iter().filter_map(|c| field(c, "op").as_str().map(str::to_owned)).collect()
    }

    fn calls(&self) -> Vec<Value> {
        self.calls.lock().unwrap().clone()
    }

    fn set_meta(&self, meta: Value) {
        *self.meta.lock().unwrap() = meta;
    }
}

impl MeetingStore for MockStore {
    fn current_meeting_id(&self) -> Option<String> {
        Some(self.meeting_id.clone())
    }
    fn current_meeting_path(&self) -> Option<PathBuf> {
        self.current_path.lock().unwrap().clone()
    }
    fn load_meta(&self) -> Result<Value, String> {
        Ok(self.meta.lock().unwrap().clone())
    }
    fn is_conference_source(&self, source: Option<&str>) -> bool {
        source.is_some_and(|s| CONFERENCE_SOURCES.contains(&s))
    }
    fn on_meeting_join(&self, url: &str, title: Option<&str>, platform: &str) -> Result<Option<String>, String> {
        self.calls
            .lock()
            .unwrap()
            .push(obj! {"op" => "on_meeting_join", "url" => url, "title" => title, "platform" => platform});
        *self.current_path.lock().unwrap() = Some(PathBuf::from(format!("/tmp/{}.jsonl", self.meeting_id)));
        Ok(Some(self.meeting_id.clone()))
    }
    fn on_ktalk_leave(&self, reason: &str) -> Result<String, String> {
        self.calls.lock().unwrap().push(obj! {"op" => "on_ktalk_leave", "reason" => reason});
        Ok(self.new_meeting_id.clone())
    }
    fn update_title(&self, title: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(obj! {"op" => "update_title", "title" => title});
        Ok(())
    }
    fn get_current_transcript(&self) -> Result<Vec<Value>, String> {
        Ok(Vec::new())
    }
}

fn meeting_storage_mock(meeting_id: &str, new_meeting_id: &str) -> Arc<MockStore> {
    Arc::new(MockStore {
        meeting_id: meeting_id.to_owned(),
        new_meeting_id: new_meeting_id.to_owned(),
        current_path: Mutex::new(Some(PathBuf::from("/tmp/old.jsonl"))),
        meta: Mutex::new(obj! {"source" => "ktalk"}),
        calls: Mutex::new(Vec::new()),
    })
}

/// `_ktalk_storage(meeting_id, title, source)`.
fn ktalk_storage(meeting_id: &str, title: &str, source: &str) -> Arc<FakeStore> {
    let storage = FakeStore::with_source(source);
    storage.set_meta(Ok(obj! {"source" => source, "title" => title}));
    *storage.current_id.lock().unwrap() = Some(meeting_id.to_owned());
    *storage.current_path.lock().unwrap() = Some(PathBuf::from(format!("/tmp/{meeting_id}.jsonl")));
    storage
}

/// `_bridge_with_active_ktalk()`.
fn bridge_with_active_ktalk() -> (Harness, Arc<RecordingSink>) {
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "ktalk"));
    let (ext, _client) = h.add(Role::Extension);
    (h, ext)
}

/// `_bridge_with_extension_and_subscriber()`.
fn bridge_with_extension_and_subscriber() -> (Harness, Arc<RecordingSink>, Arc<RecordingSink>) {
    let h = Harness::new();
    let (ext, _) = h.add(Role::Extension);
    let (sub, _) = h.add(Role::Subscriber);
    (h, ext, sub)
}

/// A bridge whose sends time out after 0.1 s (`BROADCAST_SEND_TIMEOUT_S = 0.1`).
fn bounded() -> Harness {
    Harness::with(|c| c.send_timeout = Duration::from_millis(100))
}

/// `_agent_states(client)`.
fn agent_states(sink: &RecordingSink) -> Vec<String> {
    sink.of_type("copilot_status")
        .into_iter()
        .filter(|f| field(f, "status").as_str() == Some("agent_state"))
        .map(|f| field(&f, "agent_state").as_str().expect("str").to_owned())
        .collect()
}

/// `_copilot_frames(client, frame_type, status)`.
fn copilot_frames(sink: &RecordingSink, kind: &str, status: Option<&str>) -> Vec<Value> {
    sink.of_type(kind).into_iter().filter(|f| status.is_none_or(|s| field(f, "status").as_str() == Some(s))).collect()
}

fn sent(sink: &RecordingSink) -> Vec<String> {
    sink.sent.lock().unwrap().clone()
}

fn closes(sink: &RecordingSink) -> usize {
    sink.closes.lock().unwrap().len()
}

/// `_await_timeout_scenario`: a bounded-send regression fails in 2 s, not an hour.
async fn within_two_seconds<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), future).await.expect("did not finish within 2 s")
}

fn set_supervisor(h: &Harness, status: Option<&str>, ts: f64) {
    let () = h.bridge.with_agent(|a| {
        a.supervisor_status = status.map(str::to_owned);
        a.supervisor_status_ts = ts;
    });
}

fn set_sessions(h: &Harness, count: Option<i128>) {
    let () = h.bridge.with_agent(|a| a.claude_sessions = count);
}

fn set_ts(h: &Harness, ts: f64) {
    let () = h.bridge.with_agent(|a| a.supervisor_status_ts = ts);
}

// ---- supervisor lifecycle over a real server ----

#[tokio::test]
async fn given_real_server_and_supervisor_when_extension_joins_then_supervisor_gets_meeting_started() {
    // given
    let h = Harness::new();
    let storage = meeting_storage_mock("m-join", "meet-2");
    *storage.current_path.lock().unwrap() = None;
    h.bridge.set_meeting_storage(storage);
    let url = serve(&h);
    let mut supervisor = ws_connect(&url).await;
    ws_send(&mut supervisor, r#"{"type": "supervisor_hello"}"#).await;
    let state = recv_required(&mut supervisor, "meeting_state").await;
    assert_eq!(field(&state, "active"), &Value::Bool(false));
    // when
    let mut extension = ws_connect(&url).await;
    ws_send(
        &mut extension,
        r#"{"type": "meeting_join", "url": "https://ktalk.test/abc", "title": "Sprint review", "platform": "ktalk"}"#,
    )
    .await;
    // then
    let started = recv_required(&mut supervisor, "meeting_started").await;
    assert_eq!(field(&started, "meeting_id").as_str(), Some("m-join"));
    assert_eq!(field(&started, "meeting_title").as_str(), Some("Sprint review"));
    let _ = extension.close(None).await;
    let _ = supervisor.close(None).await;
    h.bridge.stop();
}

#[tokio::test]
async fn given_real_server_and_active_meeting_when_extension_leaves_then_supervisor_gets_meeting_ended() {
    // given
    let h = Harness::new();
    let storage = meeting_storage_mock("m-prev", "m-next");
    *storage.current_path.lock().unwrap() = None;
    h.bridge.set_meeting_storage(storage);
    let url = serve(&h);
    let mut supervisor = ws_connect(&url).await;
    ws_send(&mut supervisor, r#"{"type": "supervisor_hello"}"#).await;
    recv_required(&mut supervisor, "meeting_state").await;
    let mut extension = ws_connect(&url).await;
    ws_send(&mut extension, JOIN_ABC_STANDUP).await;
    recv_required(&mut supervisor, "meeting_started").await;
    // when
    ws_send(&mut extension, r#"{"type": "meeting_leave", "reason": "user_left"}"#).await;
    // then
    let ended = recv_required(&mut supervisor, "meeting_ended").await;
    assert_eq!(field(&ended, "meeting_id").as_str(), Some("m-prev"));
    let _ = extension.close(None).await;
    let _ = supervisor.close(None).await;
    h.bridge.stop();
}

/// `test_owner_disconnect_ends_meeting[alone|idle-sibling]`.
async fn owner_disconnect_ends_meeting(idle_tab: bool) {
    // given
    let h = Harness::new();
    let storage = meeting_storage_mock("m-live", "meet-2");
    *storage.current_path.lock().unwrap() = None;
    h.bridge.set_meeting_storage(storage.clone());
    let url = serve(&h);
    let mut supervisor = ws_connect(&url).await;
    ws_send(&mut supervisor, r#"{"type": "supervisor_hello"}"#).await;
    recv_required(&mut supervisor, "meeting_state").await;
    let idle = if idle_tab { Some(ws_connect(&url).await) } else { None };
    let mut owner = ws_connect(&url).await;
    ws_send(&mut owner, JOIN_ABC_STANDUP).await;
    recv_required(&mut supervisor, "meeting_started").await;
    // when
    owner.close(None).await.expect("close");
    // then
    let ended = recv_required(&mut supervisor, "meeting_ended").await;
    assert_eq!(field(&ended, "meeting_id").as_str(), Some("m-live"));
    assert!(storage.ops().contains(&"on_ktalk_leave".to_owned()));
    if let Some(mut idle) = idle {
        let _ = idle.close(None).await;
    }
    let _ = supervisor.close(None).await;
    h.bridge.stop();
}

#[tokio::test]
async fn given_owner_alone_when_owner_disconnects_then_supervisor_gets_meeting_ended() {
    owner_disconnect_ends_meeting(false).await;
}

#[tokio::test]
async fn given_owner_and_idle_sibling_tab_when_owner_disconnects_then_supervisor_gets_meeting_ended() {
    owner_disconnect_ends_meeting(true).await;
}

#[tokio::test]
async fn given_orphan_reused_in_place_when_extension_joins_then_supervisor_gets_meeting_started() {
    // given
    let h = Harness::new();
    let storage = meeting_storage_mock("m-orphan-reuse", "meet-2");
    *storage.current_path.lock().unwrap() = Some(PathBuf::from("/tmp/m-orphan-reuse.jsonl"));
    storage.set_meta(obj! {"source" => "orphan"});
    h.bridge.set_meeting_storage(storage);
    let url = serve(&h);
    let mut supervisor = ws_connect(&url).await;
    ws_send(&mut supervisor, r#"{"type": "supervisor_hello"}"#).await;
    recv_required(&mut supervisor, "meeting_state").await;
    // when
    let mut extension = ws_connect(&url).await;
    ws_send(
        &mut extension,
        r#"{"type": "meeting_join", "url": "https://ktalk.test/room-after-leave", "title": "Обед — Толк", "platform": "ktalk"}"#,
    )
    .await;
    // then
    let started = recv_required(&mut supervisor, "meeting_started").await;
    assert_eq!(field(&started, "meeting_id").as_str(), Some("m-orphan-reuse"));
    let _ = extension.close(None).await;
    let _ = supervisor.close(None).await;
    h.bridge.stop();
}

#[tokio::test]
async fn given_active_ktalk_meeting_when_same_room_rejoined_then_supervisor_gets_no_second_meeting_started() {
    // given
    let h = Harness::new();
    let storage = meeting_storage_mock("m-reconnect", "meet-2");
    *storage.current_path.lock().unwrap() = Some(PathBuf::from("/tmp/m-reconnect.jsonl"));
    storage.set_meta(obj! {"source" => "ktalk"});
    h.bridge.set_meeting_storage(storage);
    let url = serve(&h);
    let mut supervisor = ws_connect(&url).await;
    ws_send(&mut supervisor, r#"{"type": "supervisor_hello"}"#).await;
    recv_required(&mut supervisor, "meeting_state").await;
    // when
    let mut extension = ws_connect(&url).await;
    ws_send(
        &mut extension,
        r#"{"type": "meeting_join", "url": "https://ktalk.test/same-room", "title": "Standup", "platform": "ktalk"}"#,
    )
    .await;
    // then
    assert!(recv_type(&mut supervisor, "meeting_started", Duration::from_secs(1)).await.is_none());
    let _ = extension.close(None).await;
    let _ = supervisor.close(None).await;
    h.bridge.stop();
}

// ---- meeting_state ----

#[test]
fn given_ktalk_meeting_and_extension_when_meeting_state_built_then_active_with_id_and_title() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "ktalk"));
    let _ext = h.add(Role::Extension);
    // when
    let frame = h.bridge.compute_meeting_state();
    // then
    assert_eq!(field(&frame, "type").as_str(), Some("meeting_state"));
    assert_eq!(field(&frame, "active"), &Value::Bool(true));
    assert_eq!(field(&frame, "meeting_id").as_str(), Some("m-1"));
    assert_eq!(field(&frame, "meeting_title").as_str(), Some("Standup"));
}

#[test]
fn given_orphan_meeting_and_extension_when_meeting_state_built_then_inactive_without_id() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("orphan-1", "Standup", "orphan"));
    let _ext = h.add(Role::Extension);
    // when
    let frame = h.bridge.compute_meeting_state();
    // then
    assert_eq!(field(&frame, "active"), &Value::Bool(false));
    assert_eq!(field(&frame, "meeting_id"), &Value::Null);
    assert_eq!(field(&frame, "meeting_title"), &Value::Null);
}

#[test]
fn given_ktalk_meeting_without_extension_when_meeting_state_built_then_inactive() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "ktalk"));
    // when
    let frame = h.bridge.compute_meeting_state();
    // then
    assert_eq!(field(&frame, "active"), &Value::Bool(false));
    assert_eq!(field(&frame, "meeting_id"), &Value::Null);
}

#[test]
fn given_conference_and_other_sources_when_is_conference_source_asked_then_only_conferences_are() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let storage = MeetingStorage::new(&dir.path().join("meetings"), StorageOptions::new(dir.path())).unwrap();
    // when / then
    for source in ["ktalk", "vas3k", "jitsi", "matrix", "gmeet", "zoom"] {
        assert!(storage.is_conference_source(Some(source)), "{source}");
    }
    for source in [Some("orphan"), Some("local"), None, Some(""), Some("unknown")] {
        assert!(!storage.is_conference_source(source), "{source:?}");
    }
}

#[test]
fn given_non_ktalk_conference_meeting_and_extension_when_meeting_state_built_then_active() {
    for source in ["matrix", "vas3k", "jitsi", "gmeet", "zoom"] {
        // given
        let h = Harness::new();
        h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", source));
        let _ext = h.add(Role::Extension);
        // when
        let frame = h.bridge.compute_meeting_state();
        // then
        assert_eq!(field(&frame, "active"), &Value::Bool(true), "{source}");
        assert_eq!(field(&frame, "meeting_id").as_str(), Some("m-1"), "{source}");
        assert_eq!(field(&frame, "meeting_title").as_str(), Some("Standup"), "{source}");
    }
}

#[test]
fn given_local_meeting_and_extension_when_meeting_state_built_then_inactive() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "local"));
    let _ext = h.add(Role::Extension);
    // when
    let frame = h.bridge.compute_meeting_state();
    // then
    assert_eq!(field(&frame, "active"), &Value::Bool(false));
    assert_eq!(field(&frame, "meeting_id"), &Value::Null);
}

#[tokio::test]
async fn given_non_ktalk_conference_meeting_when_last_extension_disconnects_then_meeting_ends_and_increment_runs() {
    for source in ["matrix", "vas3k", "jitsi", "gmeet", "zoom"] {
        // given
        let h = Harness::new();
        let storage = FakeStore::with_source(source);
        *storage.current_id.lock().unwrap() = Some("m-prev".into());
        *storage.current_path.lock().unwrap() = Some(PathBuf::from("/tmp/m-prev.jsonl"));
        *storage.leave_id.lock().unwrap() = "m-next".into();
        h.bridge.set_meeting_storage(storage.clone());
        let increments = Arc::new(RecordingIncrements::default());
        h.bridge.set_increment_generator(increments.clone());
        let (fake, task) = h.connect(None).await;
        // when
        fake.close();
        task.await.unwrap();
        h.bridge.drain_background_tasks(Duration::from_secs(5)).await;
        // then
        assert_eq!(storage.ops(), ["on_ktalk_leave"], "{source}");
        assert_eq!(field(&storage.calls()[0], "reason").as_str(), Some("disconnected"));
        assert_eq!(
            *increments.runs.lock().unwrap(),
            [("m-prev".to_owned(), PathBuf::from("/tmp/m-prev.jsonl"))],
            "{source}"
        );
    }
}

#[tokio::test]
async fn given_local_meeting_when_last_extension_disconnects_then_meeting_not_ended() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("local");
    h.bridge.set_meeting_storage(storage.clone());
    let (fake, task) = h.connect(None).await;
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert!(storage.ops().is_empty());
}

// ---- the agent-state merge ----

#[tokio::test]
async fn given_supervisor_working_and_one_session_when_recomputed_then_chip_is_working() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    set_supervisor(&h, Some("working"), 50.0);
    set_sessions(&h, Some(1));
    // when
    h.bridge.recompute_agent_state(Some(50.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["working"]);
}

#[tokio::test]
async fn given_supervisor_online_and_zero_sessions_when_zero_outlasts_debounce_then_chip_goes_offline() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    set_supervisor(&h, Some("online"), 100.0);
    set_sessions(&h, Some(0));
    // when
    h.bridge.recompute_agent_state(Some(100.0)).await;
    set_ts(&h, 105.0);
    h.bridge.recompute_agent_state(Some(105.0)).await;
    set_ts(&h, 111.0);
    h.bridge.recompute_agent_state(Some(111.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["online", "offline"]);
}

#[tokio::test]
async fn given_supervisor_online_when_it_falls_silent_past_threshold_then_chip_goes_offline() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    set_supervisor(&h, Some("online"), 10.0);
    // when
    h.bridge.recompute_agent_state(Some(15.0)).await;
    h.bridge.recompute_agent_state(Some(25.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["online", "offline"]);
}

#[tokio::test]
async fn given_spawning_with_zero_sessions_when_supervisor_goes_online_then_no_spurious_offline() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    set_supervisor(&h, Some("spawning"), 200.0);
    set_sessions(&h, Some(0));
    // when
    h.bridge.recompute_agent_state(Some(200.0)).await;
    set_supervisor(&h, Some("online"), 202.0);
    h.bridge.recompute_agent_state(Some(202.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["spawning", "online"]);
}

#[tokio::test]
async fn given_stale_zero_sessions_anchor_when_meeting_starts_then_debounce_anchored_at_online() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    set_sessions(&h, Some(0));
    let () = h.bridge.with_agent(|a| a.sessions_zero_since = Some(0.0));
    // when
    set_supervisor(&h, Some("spawning"), 100.0);
    h.bridge.recompute_agent_state(Some(100.0)).await;
    set_supervisor(&h, Some("online"), 103.0);
    h.bridge.recompute_agent_state(Some(103.0)).await;
    set_ts(&h, 108.0);
    h.bridge.recompute_agent_state(Some(108.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["spawning", "online"]);
}

#[tokio::test]
async fn given_zero_sessions_without_active_meeting_when_past_debounce_then_chip_stays_online() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(ktalk_storage("m-1", "Standup", "orphan"));
    let (ext, _) = h.add(Role::Extension);
    set_supervisor(&h, Some("online"), 500.0);
    set_sessions(&h, Some(0));
    // when
    h.bridge.recompute_agent_state(Some(500.0)).await;
    set_ts(&h, 520.0);
    h.bridge.recompute_agent_state(Some(520.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["online"]);
}

#[tokio::test]
async fn given_unknown_session_count_when_recomputed_then_chip_stays_online() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    set_supervisor(&h, Some("online"), 300.0);
    set_sessions(&h, None);
    // when
    h.bridge.recompute_agent_state(Some(300.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["online"]);
}

#[tokio::test]
async fn given_stable_effective_state_when_recomputed_repeatedly_then_broadcast_once() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    set_supervisor(&h, Some("working"), 50.0);
    set_sessions(&h, Some(2));
    // when
    h.bridge.recompute_agent_state(Some(50.0)).await;
    set_ts(&h, 51.0);
    h.bridge.recompute_agent_state(Some(51.0)).await;
    set_ts(&h, 52.0);
    h.bridge.recompute_agent_state(Some(52.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["working"]);
}

#[tokio::test]
async fn given_active_meeting_when_copilot_agent_status_arrives_then_stored_and_broadcast() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    // when
    h.process(r#"{"type": "copilot_agent_status", "status": "online"}"#, None).await.unwrap();
    // then
    assert_eq!(h.bridge.with_agent(|a| a.supervisor_status.clone()).as_deref(), Some("online"));
    assert_eq!(agent_states(&ext), ["online"]);
}

#[tokio::test]
async fn given_active_meeting_when_copilot_agent_status_blank_then_ignored() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    // when
    h.process(r#"{"type": "copilot_agent_status", "status": ""}"#, None).await.unwrap();
    // then
    assert_eq!(h.bridge.with_agent(|a| a.supervisor_status.clone()), None);
    assert!(agent_states(&ext).is_empty());
}

#[tokio::test]
async fn given_supervisor_online_when_claude_sessions_null_then_count_unknown_and_chip_online() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    set_supervisor(&h, Some("online"), h.clock.monotonic());
    // when
    h.process(r#"{"type": "claude_sessions", "count": null}"#, None).await.unwrap();
    // then
    assert_eq!(h.bridge.with_agent(|a| a.claude_sessions), None);
    assert_eq!(agent_states(&ext), ["online"]);
}

#[tokio::test]
async fn given_steady_online_chip_when_client_reconnects_then_primed_with_agent_state() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(FakeStore::with_source("orphan"));
    let () = h.bridge.with_agent(|a| a.last_effective = "online".to_owned());
    // when
    let (fake, task) = h.connect(None).await;
    fake.close();
    task.await.unwrap();
    // then
    let primers: Vec<Value> =
        fake.sink.frames().into_iter().filter(|f| field(f, "status").as_str() == Some("agent_state")).collect();
    assert_eq!(
        primers,
        [json(r#"{"type": "copilot_status", "request_id": "", "status": "agent_state", "agent_state": "online"}"#)]
    );
}

// ---- Copilot fan-out to extensions and subscribers (CAP-1) ----

#[tokio::test]
async fn given_extension_and_subscriber_when_channel_reply_arrives_then_both_get_same_copilot_reply() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-42", "kind": "manual", "text": "Алиса спросила про метрики.", "meeting_id": "2026-04-26_test_meeting", "created_at": 1700000123456.0}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert_eq!(sent(&ext).len(), 1);
    assert_eq!(sent(&ext), sent(&sub));
    let payload = &sub.frames()[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_reply"));
    assert_eq!(field(payload, "request_id").as_str(), Some("req-42"));
    assert_eq!(field(payload, "kind").as_str(), Some("manual"));
    assert_eq!(field(payload, "text").as_str(), Some("Алиса спросила про метрики."));
    assert_eq!(field(payload, "meeting_id").as_str(), Some("2026-04-26_test_meeting"));
    assert_eq!(field(payload, "created_at"), &Value::Float(1_700_000_123_456.0));
}

#[tokio::test]
async fn given_extension_and_subscriber_when_channel_status_arrives_then_both_get_same_copilot_status() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    // when
    h.process(
        r#"{"type": "copilot_channel_status", "request_id": "req-42", "status": "working", "message": "Claude is thinking"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert_eq!(sent(&ext).len(), 1);
    assert_eq!(sent(&ext), sent(&sub));
    let payload = &sub.frames()[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_status"));
    assert_eq!(field(payload, "request_id").as_str(), Some("req-42"));
    assert_eq!(field(payload, "status").as_str(), Some("working"));
    assert_eq!(field(payload, "message").as_str(), Some("Claude is thinking"));
}

#[tokio::test]
async fn given_active_meeting_extension_and_subscriber_when_agent_state_flips_then_both_get_chip() {
    // given
    let (h, ext) = bridge_with_active_ktalk();
    let (sub, _) = h.add(Role::Subscriber);
    set_supervisor(&h, Some("working"), 50.0);
    set_sessions(&h, Some(1));
    // when
    h.bridge.recompute_agent_state(Some(50.0)).await;
    // then
    assert_eq!(agent_states(&ext), ["working"]);
    assert_eq!(agent_states(&sub), ["working"]);
    assert_eq!(sent(&ext), sent(&sub));
}

#[tokio::test]
async fn given_no_channel_when_first_channel_says_hello_then_extension_and_subscriber_get_channel_connected() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    let (fake, task) = h.connect(None).await;
    // when
    fake.send(&json(r#"{"type": "copilot_channel_hello"}"#));
    fake.idle().await;
    // then
    assert_eq!(h.bridge.clients_in(Role::Channel).len(), 1);
    assert!(!copilot_frames(&ext, "copilot_status", Some("channel_connected")).is_empty());
    assert_eq!(sent(&ext), sent(&sub));
    fake.close();
    task.await.unwrap();
}

#[tokio::test]
async fn given_one_channel_when_its_socket_closes_then_extension_and_subscriber_get_channel_disconnected() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    let (fake, task) = h.connect(None).await;
    fake.send(&json(r#"{"type": "copilot_channel_hello"}"#));
    fake.idle().await;
    assert_eq!(h.bridge.clients_in(Role::Channel).len(), 1);
    ext.clear();
    sub.clear();
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert_eq!(copilot_frames(&ext, "copilot_status", Some("channel_disconnected")).len(), 1);
    assert_eq!(sent(&ext), sent(&sub));
}

#[tokio::test]
async fn given_subscriber_and_no_extension_when_copilot_frame_broadcast_then_subscriber_gets_it() {
    // given
    let h = Harness::new();
    let (sub, _) = h.add(Role::Subscriber);
    assert!(h.bridge.clients_in(Role::Extension).is_empty());
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-9", "kind": "manual", "text": "ответ без расширения"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert_eq!(sent(&sub).len(), 1);
    let payload = &sub.frames()[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_reply"));
    assert_eq!(field(payload, "text").as_str(), Some("ответ без расширения"));
}

#[tokio::test]
async fn given_dead_client_before_live_ones_when_copilot_frame_broadcast_then_live_clients_still_get_it() {
    // given
    let h = Harness::new();
    let (dead, _) = h.add(Role::Extension);
    dead.set_mode(SendMode::Fail("socket is gone".into()));
    let (ext, _) = h.add(Role::Extension);
    let (sub, _) = h.add(Role::Subscriber);
    // when
    let result =
        h.process(r#"{"type": "copilot_channel_status", "request_id": "req-5", "status": "done"}"#, None).await;
    // then
    assert!(result.is_ok());
    assert_eq!(copilot_frames(&ext, "copilot_status", Some("done")).len(), 1);
    assert_eq!(copilot_frames(&sub, "copilot_status", Some("done")).len(), 1);
}

#[tokio::test]
async fn given_extension_and_subscriber_when_non_copilot_frame_broadcast_then_only_extension_gets_it() {
    // given
    let (h, ext, sub) = bridge_with_extension_and_subscriber();
    // when
    h.bridge.broadcast_message(&json(r#"{"type": "transcript_history", "meeting_id": "m-1", "segments": []}"#)).await;
    // then
    assert_eq!(sent(&ext).len(), 1);
    assert_eq!(frame_type(&ext.frames()[0]), Some("transcript_history"));
    assert!(sent(&sub).is_empty());
}

// ---- bounded fan-out sends (DW-6) ----

/// `SendBarrier` + `BarrierStalledClient`: each send blocks until released, and the
/// barrier opens once `expected` sends entered.
struct BarrierSink {
    started: Arc<AtomicUsize>,
    expected: usize,
    all_started: Arc<tokio::sync::Semaphore>,
    release: tokio::sync::watch::Receiver<bool>,
}

impl ClientSink for BarrierSink {
    fn send(&self, _text: String) -> BoxFuture<Result<(), SendError>> {
        if self.started.fetch_add(1, Ordering::SeqCst) + 1 == self.expected {
            self.all_started.add_permits(1);
        }
        let mut release = self.release.clone();
        Box::pin(async move {
            let _ = release.wait_for(|released| *released).await;
            Ok(())
        })
    }

    fn close(&self, _code: u16, _reason: String) -> BoxFuture<Result<(), SendError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn given_default_configuration_when_bridge_sends_then_timeout_is_five_seconds() {
    assert_eq!(BridgeConfig::default().send_timeout, Duration::from_secs(5));
}

#[tokio::test]
async fn given_stalled_extension_and_healthy_subscriber_when_copilot_frame_broadcast_then_subscriber_gets_it_bounded() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Extension);
    stalled.set_mode(SendMode::Stall);
    let (healthy, _) = h.add(Role::Subscriber);
    // when
    let start = Instant::now();
    within_two_seconds(h.bridge.broadcast_copilot_frame(&json(r#"{"type": "copilot_reply", "text": "hi"}"#))).await;
    let elapsed = start.elapsed();
    // then
    assert_eq!(sent(&healthy).len(), 1);
    assert_eq!(frame_type(&healthy.frames()[0]), Some("copilot_reply"));
    assert!(elapsed < Duration::from_secs(2));
}

#[tokio::test]
async fn given_stalled_extension_when_broadcast_times_out_then_close_scheduled_and_later_sends_fail_fast() {
    // given
    let h = bounded();
    let (stalled, client) = h.add(Role::Extension);
    stalled.set_mode(SendMode::Stall);
    // when
    within_two_seconds(h.bridge.broadcast_message(&json(r#"{"type": "transcript", "text": "x"}"#))).await;
    assert!(wait_for(|| closes(&stalled) > 0).await);
    // then
    assert_eq!(closes(&stalled), 1);
    assert_eq!(h.bridge.roles_of(client.id), [Role::Extension]);
    assert!(h.bridge.is_timed_out(client.id));
    let start = Instant::now();
    within_two_seconds(h.bridge.broadcast_message(&json(r#"{"type": "transcript", "text": "again"}"#))).await;
    assert!(start.elapsed() < Duration::from_millis(50));
    assert_eq!(closes(&stalled), 1);
}

#[tokio::test]
async fn given_overlapping_timeouts_when_client_stalls_then_close_is_scheduled_once() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Extension);
    stalled.set_mode(SendMode::Stall);
    let one = json(r#"{"type": "transcript", "text": "one"}"#);
    let two = json(r#"{"type": "transcript", "text": "two"}"#);
    // when
    within_two_seconds(futures_util::future::join(h.bridge.broadcast_message(&one), h.bridge.broadcast_message(&two)))
        .await;
    assert!(wait_for(|| closes(&stalled) > 0).await);
    settle().await;
    // then
    assert_eq!(closes(&stalled), 1);
    assert_eq!(h.logs().iter().filter(|l| l.contains("Send timed out")).count(), 1);
}

#[tokio::test]
async fn given_close_failure_when_timeout_fires_then_failure_is_logged() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Extension);
    stalled.set_mode(SendMode::Stall);
    *stalled.close_error.lock().unwrap() = Some("close failed".into());
    // when
    within_two_seconds(h.bridge.broadcast_message(&json(r#"{"type": "transcript", "text": "x"}"#))).await;
    let logged = wait_for(|| h.logged("Failed to close stalled client: close failed")).await;
    // then
    assert_eq!(closes(&stalled), 1);
    assert!(logged, "{:?}", h.logs());
}

#[tokio::test]
async fn given_handler_reaped_client_when_send_times_out_then_marker_is_not_readded() {
    // given
    let h = bounded();
    let (stalled, client) = h.add(Role::Extension);
    stalled.set_mode(SendMode::Stall);
    let bridge = h.bridge.clone();
    let target = client.clone();
    let send_task = tokio::spawn(async move { bridge.send_bounded(&target, "frame").await });
    settle().await;
    // when
    h.bridge.remove_client(client.id);
    let delivered = within_two_seconds(send_task).await.unwrap();
    settle().await;
    // then
    assert_eq!(delivered, Ok(false));
    assert!(!h.bridge.is_timed_out(client.id));
    assert_eq!(closes(&stalled), 0);
}

#[tokio::test]
async fn given_timed_out_channel_when_handler_reaps_it_then_pending_requests_are_closed() {
    // given
    let h = bounded();
    let (extension, _) = h.add(Role::Extension);
    let (channel, task) = h.connect(None).await;
    channel.send(&json(r#"{"type": "copilot_channel_hello"}"#));
    channel.idle().await;
    let channel_ids = h.bridge.clients_in(Role::Channel);
    assert_eq!(channel_ids.len(), 1);
    extension.clear();
    h.bridge.set_pending_request("req-timeout", "meeting-A");
    channel.sink.set_mode(SendMode::Stall);
    // when
    within_two_seconds(
        h.bridge.broadcast_to_channel_clients(&json(r#"{"type": "meeting_started", "meeting_id": "meeting-A"}"#)),
    )
    .await;
    assert!(wait_for(|| closes(&channel.sink) > 0).await);
    assert_eq!(h.bridge.clients_in(Role::Channel), channel_ids);
    channel.close();
    within_two_seconds(task).await.unwrap();
    settle().await;
    // then
    assert!(!h.bridge.pending_requests().iter().any(|(id, _)| id == "req-timeout"));
    assert!(h.bridge.clients_in(Role::Channel).is_empty());
    assert!(!h.bridge.is_timed_out(channel_ids[0]));
    assert_eq!(copilot_frames(&extension, "copilot_status", Some("channel_disconnected")).len(), 1);
}

#[tokio::test]
async fn given_timed_out_supervisor_when_handler_reaps_it_then_agent_goes_offline() {
    // given
    let h = bounded();
    let (extension, _) = h.add(Role::Extension);
    let (supervisor, task) = h.connect(None).await;
    supervisor.send(&json(r#"{"type": "supervisor_hello"}"#));
    supervisor.idle().await;
    let supervisor_ids = h.bridge.clients_in(Role::Supervisor);
    assert_eq!(supervisor_ids.len(), 1);
    let now = h.clock.monotonic();
    let () = h.bridge.with_agent(|a| {
        a.supervisor_status = Some("online".into());
        a.supervisor_status_ts = now;
        a.last_effective = "online".into();
    });
    extension.clear();
    supervisor.sink.set_mode(SendMode::Stall);
    // when
    within_two_seconds(h.bridge.emit_lifecycle(json(r#"{"type": "meeting_started", "meeting_id": "meeting-A"}"#)))
        .await;
    assert!(wait_for(|| closes(&supervisor.sink) > 0).await);
    assert_eq!(h.bridge.clients_in(Role::Supervisor), supervisor_ids);
    supervisor.close();
    within_two_seconds(task).await.unwrap();
    // then
    assert_eq!(h.bridge.with_agent(|a| a.supervisor_status.clone()), None);
    assert!(h.bridge.clients_in(Role::Supervisor).is_empty());
    assert!(!h.bridge.is_timed_out(supervisor_ids[0]));
    assert_eq!(agent_states(&extension), ["offline"]);
}

#[tokio::test]
async fn given_timed_out_sole_extension_when_close_is_pending_then_meeting_stays_active() {
    // given
    let h = bounded();
    let storage = meeting_storage_mock("meeting-A", "meet-2");
    h.bridge.set_meeting_storage(storage.clone());
    let (extension, task) = h.connect(None).await;
    assert_eq!(h.bridge.clients_in(Role::Extension).len(), 1);
    extension.sink.set_mode(SendMode::Stall);
    // when
    within_two_seconds(h.bridge.broadcast_message(&json(r#"{"type": "meeting_started", "meeting_id": "meeting-A"}"#)))
        .await;
    assert!(wait_for(|| closes(&extension.sink) > 0).await);
    assert!(h.bridge.is_conference_active());
    assert!(storage.ops().is_empty());
    extension.close();
    within_two_seconds(task).await.unwrap();
    // then
    assert_eq!(storage.ops(), ["on_ktalk_leave"]);
    assert_eq!(field(&storage.calls()[0], "reason").as_str(), Some("disconnected"));
}

#[tokio::test]
async fn given_stalled_sole_supervisor_when_frame_relayed_then_not_delivered_within_timeout() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Supervisor);
    stalled.set_mode(SendMode::Stall);
    // when
    let start = Instant::now();
    let delivered = within_two_seconds(
        h.bridge.relay_to_supervisors(&json(r#"{"type": "copilot_stop_request", "request_id": "req-stalled"}"#)),
    )
    .await;
    let elapsed = start.elapsed();
    // then
    assert!(!delivered);
    assert!(elapsed < Duration::from_secs(2));
}

#[tokio::test]
async fn given_stalled_supervisor_when_stop_is_requested_then_originator_is_rejected() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Supervisor);
    stalled.set_mode(SendMode::Stall);
    h.bridge.force_manual_copilot_active(true);
    let (origin, origin_client) = h.add(Role::Extension);
    h.bridge.remove_client(origin_client.id);
    // when
    within_two_seconds(
        h.process(r#"{"type": "copilot_stop_request", "request_id": "req-stalled-stop"}"#, Some(&origin_client)),
    )
    .await
    .unwrap();
    // then
    let frames = copilot_frames(&origin, "copilot_status", Some("supervisor_disconnected"));
    assert_eq!(frames.len(), 1);
    assert_eq!(field(&frames[0], "request_id").as_str(), Some("req-stalled-stop"));
    assert!(!h.bridge.manual_copilot_active());
}

#[tokio::test]
async fn given_stalled_and_healthy_supervisor_when_frame_relayed_then_delivered() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Supervisor);
    stalled.set_mode(SendMode::Stall);
    let (healthy, _) = h.add(Role::Supervisor);
    // when
    let delivered = within_two_seconds(
        h.bridge.relay_to_supervisors(&json(r#"{"type": "copilot_stop_request", "request_id": "req-mixed"}"#)),
    )
    .await;
    // then
    assert!(delivered);
    assert_eq!(sent(&healthy).len(), 1);
    assert_eq!(field(&healthy.frames()[0], "request_id").as_str(), Some("req-mixed"));
}

#[tokio::test]
async fn given_stalled_and_healthy_extension_when_broadcast_times_out_then_healthy_gets_frame() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Extension);
    stalled.set_mode(SendMode::Stall);
    let (healthy, _) = h.add(Role::Extension);
    // when
    within_two_seconds(h.bridge.broadcast_message(&json(r#"{"type": "transcript", "text": "x"}"#))).await;
    // then
    assert_eq!(sent(&healthy).len(), 1);
}

#[tokio::test]
async fn given_stalled_and_healthy_channel_when_channel_broadcast_then_healthy_gets_it_bounded() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Channel);
    stalled.set_mode(SendMode::Stall);
    let (healthy, _) = h.add(Role::Channel);
    // when
    let start = Instant::now();
    within_two_seconds(
        h.bridge.broadcast_to_channel_clients(&json(r#"{"type": "meeting_started", "meeting_id": "m-ch"}"#)),
    )
    .await;
    let elapsed = start.elapsed();
    // then
    assert_eq!(sent(&healthy).len(), 1);
    assert_eq!(frame_type(&healthy.frames()[0]), Some("meeting_started"));
    assert!(elapsed < Duration::from_secs(2));
}

#[tokio::test]
async fn given_stalled_and_healthy_supervisor_when_lifecycle_emitted_then_healthy_recipients_get_it_bounded() {
    // given
    let h = bounded();
    let (ext, _) = h.add(Role::Extension);
    let (stalled, _) = h.add(Role::Supervisor);
    stalled.set_mode(SendMode::Stall);
    let (healthy_supervisor, _) = h.add(Role::Supervisor);
    // when
    let start = Instant::now();
    within_two_seconds(h.bridge.emit_lifecycle(json(r#"{"type": "meeting_started", "meeting_id": "m-lc"}"#))).await;
    let elapsed = start.elapsed();
    // then
    assert_eq!(sent(&ext).len(), 1);
    assert_eq!(frame_type(&ext.frames()[0]), Some("meeting_started"));
    assert_eq!(sent(&healthy_supervisor).len(), 1);
    assert_eq!(frame_type(&healthy_supervisor.frames()[0]), Some("meeting_started"));
    assert!(elapsed < Duration::from_secs(2));
}

#[tokio::test]
async fn given_stalled_and_healthy_subscriber_when_manual_session_preempted_then_healthy_gets_notice_bounded() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Subscriber);
    stalled.set_mode(SendMode::Stall);
    let (healthy, _) = h.add(Role::Subscriber);
    h.bridge.force_manual_copilot_active(true);
    // when
    let start = Instant::now();
    let preempted = within_two_seconds(h.bridge.preempt_manual_copilot(true)).await;
    let elapsed = start.elapsed();
    // then
    assert!(preempted);
    assert_eq!(sent(&healthy).len(), 1);
    let payload = &healthy.frames()[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_status"));
    assert_eq!(field(payload, "status").as_str(), Some("manual_preempted"));
    assert!(elapsed < Duration::from_secs(2));
}

#[tokio::test]
async fn given_stalled_and_healthy_channel_when_chat_message_processed_then_healthy_gets_it_bounded() {
    // given
    let h = bounded();
    let (stalled, _) = h.add(Role::Channel);
    stalled.set_mode(SendMode::Stall);
    let (healthy, _) = h.add(Role::Channel);
    // when
    let start = Instant::now();
    within_two_seconds(
        h.process(r#"{"type": "copilot_chat_message", "request_id": "req-chat-stall", "text": "привет"}"#, None),
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    // then
    assert_eq!(sent(&healthy).len(), 1);
    let payload = &healthy.frames()[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_user_message"));
    assert_eq!(field(payload, "request_id").as_str(), Some("req-chat-stall"));
    assert!(elapsed < Duration::from_secs(2));
}

#[tokio::test]
async fn given_three_stalled_clients_behind_barrier_when_broadcast_then_all_enter_concurrently() {
    // given
    let h = Harness::with(|c| c.send_timeout = Duration::from_secs(10));
    let started = Arc::new(AtomicUsize::new(0));
    let all_started = Arc::new(tokio::sync::Semaphore::new(0));
    let (release, release_rx) = tokio::sync::watch::channel(false);
    for _ in 0..3 {
        let sink = Arc::new(BarrierSink {
            started: started.clone(),
            expected: 3,
            all_started: all_started.clone(),
            release: release_rx.clone(),
        });
        h.bridge.add_client(Role::Extension, sink, "('127.0.0.1', 12345)");
    }
    let (healthy, _) = h.add(Role::Extension);
    let bridge = h.bridge.clone();
    // when
    let broadcast = tokio::spawn(async move {
        bridge.broadcast_message(&json(r#"{"type": "transcript", "text": "x"}"#)).await;
    });
    let entered = tokio::time::timeout(Duration::from_secs(1), all_started.acquire()).await;
    release.send(true).unwrap();
    within_two_seconds(broadcast).await.unwrap();
    // then
    assert!(entered.is_ok(), "the three sends did not enter concurrently");
    assert_eq!(started.load(Ordering::SeqCst), 3);
    assert_eq!(sent(&healthy).len(), 1);
}
