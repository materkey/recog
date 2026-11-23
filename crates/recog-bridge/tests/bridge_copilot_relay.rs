//! tests/test_ktalk_bridge.py from `test_copilot_chat_message_forwarded_to_channel_client`
//! through `test_get_recent_transcript_snapshots_decouple_from_buffer`: the Copilot
//! chat relay, the channel's replies and statuses, stale requests across meetings,
//! the lifecycle frames, the recent-transcript buffer and the proactive ticks.
//!
//! Adapted for intent (no Rust counterpart of the Python mechanism):
//! - the `send_transcript` payload tests patch `json.dumps`; here the frame is read
//!   from an extension client of an attached runtime;
//! - tests that rewrite a buffer entry's `end_ms` or inject an entry of another
//!   meeting record it through `send_transcript` with the clock or the storage's
//!   current meeting moved instead;
//! - the eviction test shrinks the deque to `maxlen=3`; the cap is a constant here,
//!   so the test records one more segment than the cap;
//! - the snapshot-decoupling test mutates the entry in place; here a refine rewrites
//!   it after the response was sent;
//! - a Python `Mock()` storage without a conference source (`_make_storage_for_meeting`)
//!   is a `FakeStore` of an `orphan` meeting.
//!
//! Not ported: none.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{Harness, NOW, field, json, settle};
use recog_bridge::copilot::{COPILOT_TICK_INTERVAL_SECONDS, TRANSCRIPT_BUFFER_CAP, TickState};
use recog_bridge::testing::{FakeStore, RecordingSink, SendMode};
use recog_bridge::{Client, ClientId, RefineUpdate, Role, TranscriptLine};
use recog_core::pyjson::Value;

// ---- helpers (the Python fixtures of this slice) ----

/// A websocket that is in no set of the bridge (`RecordingClient()` passed as
/// `websocket=` without being added).
fn unregistered(n: u64) -> (Arc<RecordingSink>, Arc<Client>) {
    let sink = RecordingSink::new();
    let client = Client::new(ClientId(u64::MAX - n), sink.clone(), "('127.0.0.1', 54321)");
    (sink, client)
}

fn sent(sink: &RecordingSink) -> Vec<String> {
    sink.sent.lock().unwrap().clone()
}

/// `_copilot_frames(client, frame_type, status)`.
fn copilot_frames(sink: &RecordingSink, frame_type: &str, status: Option<&str>) -> Vec<Value> {
    sink.of_type(frame_type)
        .into_iter()
        .filter(|f| {
            status.is_none_or(|s| f.as_object().and_then(|m| m.get("status")).and_then(Value::as_str) == Some(s))
        })
        .collect()
}

/// `_make_storage_for_meeting(meeting_id)`: a storage reporting `meeting_id`, its
/// meeting not a conference.
fn storage_for_meeting(meeting_id: &str) -> Arc<FakeStore> {
    let storage = FakeStore::with_source("orphan");
    *storage.current_id.lock().unwrap() = Some(meeting_id.to_owned());
    storage
}

/// `_meeting_storage_mock(meeting_id, new_meeting_id)`: a ktalk meeting whose join
/// moves the path, so the bridge sees a real meeting change.
fn meeting_storage_mock(meeting_id: &str, new_meeting_id: &str) -> Arc<FakeStore> {
    let storage = FakeStore::with_source("ktalk");
    *storage.current_id.lock().unwrap() = Some(meeting_id.to_owned());
    *storage.current_path.lock().unwrap() = Some(PathBuf::from("/tmp/old.jsonl"));
    *storage.leave_id.lock().unwrap() = new_meeting_id.to_owned();
    *storage.join_result.lock().unwrap() =
        (Some(meeting_id.to_owned()), Some(PathBuf::from(format!("/tmp/{meeting_id}.jsonl"))));
    storage
}

/// `_reconnect_storage_mock(meeting_id)`: a join that keeps the path (a reconnect).
fn reconnect_storage_mock(meeting_id: &str) -> Arc<FakeStore> {
    let storage = FakeStore::with_source("ktalk");
    let path = PathBuf::from(format!("/tmp/{meeting_id}.jsonl"));
    *storage.current_id.lock().unwrap() = Some(meeting_id.to_owned());
    *storage.current_path.lock().unwrap() = Some(path.clone());
    *storage.leave_id.lock().unwrap() = "m-next".to_owned();
    *storage.join_result.lock().unwrap() = (Some(meeting_id.to_owned()), Some(path));
    storage
}

fn set_current(storage: &FakeStore, meeting_id: &str) {
    *storage.current_id.lock().unwrap() = Some(meeting_id.to_owned());
}

/// `send_transcript(speaker, text, timestamp, segment_id=...)`.
fn line(speaker: &str, text: &str, timestamp: &str, segment_id: i64) -> TranscriptLine {
    TranscriptLine {
        speaker: speaker.into(),
        text: text.into(),
        timestamp: timestamp.into(),
        segment_id,
        ..TranscriptLine::default()
    }
}

fn refine(segment_id: i64, text: &str) -> RefineUpdate {
    RefineUpdate { segment_id, text: text.into() }
}

/// `_activate_tick_state(bridge)`.
fn activate_tick_state(h: &Harness, meeting_id: &str, meeting_title: &str) {
    h.bridge.set_tick_state(Some(TickState::new(Some(meeting_id.into()), Value::from(meeting_title), h.bridge.now())));
}

fn pending(h: &Harness, request_id: &str) -> Option<String> {
    h.bridge.pending_requests().into_iter().find(|(k, _)| k == request_id).map(|(_, v)| v)
}

fn buffer_ids(h: &Harness) -> Vec<i64> {
    h.bridge.transcript_buffer().iter().filter_map(|e| e.get("segment_id").and_then(int)).collect()
}

fn buffer_entry(h: &Harness, segment_id: i64) -> recog_core::pyjson::Map {
    h.bridge
        .transcript_buffer()
        .into_iter()
        .find(|e| e.get("segment_id").and_then(int) == Some(segment_id))
        .unwrap_or_else(|| panic!("no segment {segment_id} in the buffer"))
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => i64::try_from(*i).ok(),
        _ => None,
    }
}

fn float(v: &Value) -> f64 {
    match v {
        Value::Float(f) => *f,
        #[allow(clippy::cast_precision_loss)]
        Value::Int(i) => *i as f64,
        other => panic!("not a number: {other:?}"),
    }
}

fn array(v: &Value) -> &[Value] {
    match v {
        Value::Array(items) => items,
        other => panic!("not an array: {other:?}"),
    }
}

fn segment_ids(frame: &Value) -> Vec<i64> {
    array(field(frame, "segments")).iter().filter_map(|s| int(field(s, "segment_id"))).collect()
}

fn is_number(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::Float(_))
}

// ---- copilot_chat_message: extension -> channel forwarding ----

#[tokio::test]
async fn given_channel_client_and_conference_meeting_when_chat_message_then_channel_gets_enriched_user_message() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    set_current(&storage, "2026-04-26_test_meeting");
    h.bridge.set_meeting_storage(storage);
    let (channel, _c) = h.add(Role::Channel);
    let (extension, extension_ws) = h.add(Role::Extension);
    let transcript_window = json(r#"[{"speaker": "Alice", "text": "пробуем", "ts": 1700000000.0}]"#);
    // when
    h.process(
        r#"{"type": "copilot_chat_message", "request_id": "req-42", "text": "что только что сказала Алиса?",
            "meeting_title": "Sprint review",
            "transcript_window": [{"speaker": "Alice", "text": "пробуем", "ts": 1700000000.0}],
            "created_at": 1700000123456.0}"#,
        Some(&extension_ws),
    )
    .await
    .unwrap();
    // then
    let frames = channel.frames();
    assert_eq!(frames.len(), 1);
    let payload = &frames[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_user_message"));
    assert_eq!(field(payload, "request_id").as_str(), Some("req-42"));
    assert_eq!(field(payload, "text").as_str(), Some("что только что сказала Алиса?"));
    assert_eq!(field(payload, "meeting_id").as_str(), Some("2026-04-26_test_meeting"));
    assert_eq!(field(payload, "meeting_title").as_str(), Some("Sprint review"));
    assert!(field(payload, "transcript_window").py_eq(&transcript_window));
    assert_eq!(field(payload, "created_at"), &Value::Float(1_700_000_123_456.0));
    assert!(sent(&extension).is_empty());
}

#[tokio::test]
async fn given_webapp_tabs_when_extension_chat_delivered_then_only_subscribers_get_echo() {
    // given
    let h = Harness::new();
    let (_channel, _c) = h.add(Role::Channel);
    let (extension, extension_ws) = h.add(Role::Extension);
    let (subscriber, _s) = h.add(Role::Subscriber);
    // when
    h.process(
        r#"{"type": "copilot_chat_message", "request_id": "req-echo", "text": "к чему ведёт", "created_at": 1700000123456.0}"#,
        Some(&extension_ws),
    )
    .await
    .unwrap();
    // then
    let frames = subscriber.frames();
    assert_eq!(frames.len(), 1);
    assert!(frames[0].py_eq(&json(
        r#"{"type": "copilot_chat_echo", "request_id": "req-echo", "text": "к чему ведёт", "created_at": 1700000123456.0}"#
    )));
    assert!(sent(&extension).is_empty());
}

#[tokio::test]
async fn given_no_channel_when_chat_message_then_no_echo_to_subscribers() {
    // given
    let h = Harness::new();
    let (subscriber, _s) = h.add(Role::Subscriber);
    let (_origin, origin_ws) = unregistered(1);
    // when
    h.process(r#"{"type": "copilot_chat_message", "request_id": "req-lost", "text": "вопрос"}"#, Some(&origin_ws))
        .await
        .unwrap();
    // then
    assert!(sent(&subscriber).is_empty());
}

#[tokio::test]
async fn given_no_channel_when_chat_message_then_originator_gets_channel_disconnected_status() {
    // given
    let h = Harness::new();
    assert!(h.bridge.clients_in(Role::Channel).is_empty());
    let (extension, extension_ws) = unregistered(1);
    // when
    h.process(
        r#"{"type": "copilot_chat_message", "request_id": "req-no-channel", "text": "сводка"}"#,
        Some(&extension_ws),
    )
    .await
    .unwrap();
    // then
    let frames = extension.frames();
    assert_eq!(frames.len(), 1);
    let payload = &frames[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_status"));
    assert_eq!(field(payload, "request_id").as_str(), Some("req-no-channel"));
    assert_eq!(field(payload, "status").as_str(), Some("channel_disconnected"));
    assert!(field(payload, "message").as_str().is_some_and(|m| !m.is_empty()));
}

#[tokio::test]
async fn given_no_channel_when_chat_sent_then_disconnected_echo_is_originator_only() {
    // given
    let h = Harness::new();
    let (ext, _e) = h.add(Role::Extension);
    let (sub, _s) = h.add(Role::Subscriber);
    let (sup, _p) = h.add(Role::Supervisor);
    let (origin, origin_ws) = unregistered(1);
    // when
    h.process(r#"{"type": "copilot_chat_message", "request_id": "req-orig-only", "text": "сводка"}"#, Some(&origin_ws))
        .await
        .unwrap();
    // then
    let echoes = copilot_frames(&origin, "copilot_status", Some("channel_disconnected"));
    assert_eq!(echoes.len(), 1);
    assert_eq!(field(&echoes[0], "request_id").as_str(), Some("req-orig-only"));
    assert_eq!(sent(&origin).len(), 1);
    assert!(sent(&ext).is_empty());
    assert!(sent(&sub).is_empty());
    assert!(sent(&sup).is_empty());
}

#[tokio::test]
async fn given_channel_client_when_chat_message_lacks_request_id_or_text_then_nothing_is_sent() {
    // given
    let h = Harness::new();
    let (channel, _c) = h.add(Role::Channel);
    let (extension, extension_ws) = unregistered(1);
    // when
    for frame in [
        r#"{"type": "copilot_chat_message", "text": "x"}"#,
        r#"{"type": "copilot_chat_message", "request_id": "", "text": "x"}"#,
        r#"{"type": "copilot_chat_message", "request_id": "req-1"}"#,
        r#"{"type": "copilot_chat_message", "request_id": "req-1", "text": ""}"#,
    ] {
        h.process(frame, Some(&extension_ws)).await.unwrap();
    }
    // then
    assert!(sent(&channel).is_empty());
    assert!(sent(&extension).is_empty());
}

// ---- copilot_channel_reply / copilot_channel_status: channel -> extension relay ----

#[tokio::test]
async fn given_extension_client_when_channel_reply_then_it_is_relayed_as_copilot_reply() {
    // given
    let h = Harness::new();
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-42", "kind": "manual",
            "text": "Алиса спросила про метрики.", "meeting_id": "2026-04-26_test_meeting",
            "created_at": 1700000123456.0}"#,
        None,
    )
    .await
    .unwrap();
    // then
    let frames = extension.frames();
    assert_eq!(frames.len(), 1);
    let payload = &frames[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_reply"));
    assert_eq!(field(payload, "request_id").as_str(), Some("req-42"));
    assert_eq!(field(payload, "kind").as_str(), Some("manual"));
    assert_eq!(field(payload, "text").as_str(), Some("Алиса спросила про метрики."));
    assert_eq!(field(payload, "meeting_id").as_str(), Some("2026-04-26_test_meeting"));
    assert_eq!(field(payload, "created_at"), &Value::Float(1_700_000_123_456.0));
}

#[tokio::test]
async fn given_channel_and_extension_clients_when_channel_reply_then_channel_does_not_get_it_back() {
    // given
    let h = Harness::new();
    let (channel, _c) = h.add(Role::Channel);
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-7", "kind": "live", "text": "ok", "meeting_id": null}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert_eq!(sent(&extension).len(), 1);
    assert!(sent(&channel).is_empty());
}

#[tokio::test]
async fn given_extension_client_when_channel_status_then_it_is_relayed_as_copilot_status() {
    // given
    let h = Harness::new();
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "copilot_channel_status", "request_id": "req-42", "status": "working", "message": "Claude is thinking"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    let frames = extension.frames();
    assert_eq!(frames.len(), 1);
    let payload = &frames[0];
    assert_eq!(field(payload, "type").as_str(), Some("copilot_status"));
    assert_eq!(field(payload, "request_id").as_str(), Some("req-42"));
    assert_eq!(field(payload, "status").as_str(), Some("working"));
    assert_eq!(field(payload, "message").as_str(), Some("Claude is thinking"));
}

#[tokio::test]
async fn given_extension_client_when_channel_reply_has_invalid_fields_then_nothing_is_relayed() {
    // given
    let h = Harness::new();
    let (extension, _e) = h.add(Role::Extension);
    // when
    for frame in [
        r#"{"type": "copilot_channel_reply", "kind": "manual", "text": "x"}"#,
        r#"{"type": "copilot_channel_reply", "request_id": "", "kind": "manual", "text": "x"}"#,
        r#"{"type": "copilot_channel_reply", "request_id": "req-1", "text": "x"}"#,
        r#"{"type": "copilot_channel_reply", "request_id": "req-1", "kind": "manual", "text": 123}"#,
    ] {
        h.process(frame, None).await.unwrap();
    }
    // then
    assert!(sent(&extension).is_empty());
}

#[tokio::test]
async fn given_pending_request_when_channel_reply_has_non_string_meeting_id_then_ignored_and_entry_kept() {
    // given
    let h = Harness::new();
    let (extension, _e) = h.add(Role::Extension);
    h.bridge.set_pending_request("req-1", "m-current");
    // when
    for bad_meeting_id in ["123", r#"["m-1"]"#, r#"{"id": "m-1"}"#] {
        let frame = format!(
            r#"{{"type": "copilot_channel_reply", "request_id": "req-1", "kind": "manual", "text": "x", "meeting_id": {bad_meeting_id}}}"#
        );
        h.process(&frame, None).await.unwrap();
    }
    // then
    assert!(sent(&extension).is_empty());
    assert_eq!(pending(&h, "req-1").as_deref(), Some("m-current"));
    let ignored =
        h.logs().iter().filter(|l| l.contains("copilot_channel_reply invalid meeting_id type — ignored")).count();
    assert_eq!(ignored, 3);
}

#[tokio::test]
async fn given_pending_requests_when_channel_reply_has_null_or_absent_meeting_id_then_both_are_relayed() {
    // given
    let h = Harness::new();
    let (extension, _e) = h.add(Role::Extension);
    let payloads = [
        (
            "req-null",
            r#"{"type": "copilot_channel_reply", "request_id": "req-null", "kind": "manual", "text": "ответ", "meeting_id": null}"#,
        ),
        (
            "req-absent",
            r#"{"type": "copilot_channel_reply", "request_id": "req-absent", "kind": "manual", "text": "ещё ответ"}"#,
        ),
    ];
    // when
    for (request_id, payload) in payloads {
        h.bridge.set_pending_request(request_id, "");
        h.process(payload, None).await.unwrap();
    }
    // then
    let relayed: Vec<String> = extension
        .of_type("copilot_reply")
        .iter()
        .map(|f| field(f, "request_id").as_str().unwrap().to_owned())
        .collect();
    assert_eq!(relayed, ["req-null", "req-absent"]);
}

#[tokio::test]
async fn given_extension_client_when_channel_status_has_invalid_fields_then_nothing_is_relayed() {
    // given
    let h = Harness::new();
    let (extension, _e) = h.add(Role::Extension);
    // when
    for frame in [
        r#"{"type": "copilot_channel_status", "status": "working"}"#,
        r#"{"type": "copilot_channel_status", "request_id": "req-1"}"#,
        r#"{"type": "copilot_channel_status", "request_id": "req-1", "status": ""}"#,
    ] {
        h.process(frame, None).await.unwrap();
    }
    // then
    assert!(sent(&extension).is_empty());
}

// ---- meeting_started / meeting_ended: lifecycle frames to channel clients ----

#[tokio::test]
async fn given_channel_and_extension_clients_when_meeting_join_then_both_get_meeting_started() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("2026-04-26_review", "meet-2"));
    let (channel, _c) = h.add(Role::Channel);
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "meeting_join", "url": "https://test.ktalk.ru/abc", "title": "Sprint review", "platform": "ktalk"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    let channel_started = channel.of_type("meeting_started");
    assert_eq!(channel_started.len(), 1);
    let payload = &channel_started[0];
    assert_eq!(field(payload, "meeting_id").as_str(), Some("2026-04-26_review"));
    assert_eq!(field(payload, "meeting_title").as_str(), Some("Sprint review"));
    assert!(is_number(field(payload, "created_at")));
    let extension_started = extension.of_type("meeting_started");
    assert_eq!(extension_started.len(), 1);
    assert_eq!(field(&extension_started[0], "meeting_id").as_str(), Some("2026-04-26_review"));
}

#[tokio::test]
async fn given_channel_and_extension_clients_when_meeting_leave_then_both_get_meeting_ended_for_previous_meeting() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-prev", "m-next"));
    let (channel, _c) = h.add(Role::Channel);
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(r#"{"type": "meeting_leave", "reason": "user_left"}"#, None).await.unwrap();
    // then
    let channel_ended = channel.of_type("meeting_ended");
    assert_eq!(channel_ended.len(), 1);
    assert_eq!(field(&channel_ended[0], "meeting_id").as_str(), Some("m-prev"));
    assert!(is_number(field(&channel_ended[0], "created_at")));
    let extension_ended = extension.of_type("meeting_ended");
    assert_eq!(extension_ended.len(), 1);
    assert_eq!(field(&extension_ended[0], "meeting_id").as_str(), Some("m-prev"));
}

#[tokio::test]
async fn given_nulled_tick_state_when_reconnect_join_keeps_path_then_tick_state_is_rearmed() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(reconnect_storage_mock("m-live"));
    h.bridge.set_tick_state(None);
    // when
    h.process(
        r#"{"type": "meeting_join", "url": "https://test.ktalk.ru/live", "title": "Live interview", "platform": "ktalk"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    let tick = h.bridge.tick_state().expect("tick state re-armed");
    assert_eq!(tick.meeting_id.as_deref(), Some("m-live"));
}

// ---- stale request_id: late replies dropped after meeting_leave ----

#[tokio::test]
async fn given_request_of_left_meeting_when_late_reply_arrives_then_dropped_and_entry_purged() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-prev", "m-next"));
    let (_channel, _c) = h.add(Role::Channel);
    let (extension, _e) = h.add(Role::Extension);
    let (_origin, origin_ws) = unregistered(1);
    h.process(
        r#"{"type": "copilot_chat_message", "request_id": "req-late", "text": "сводка за последние 5 минут"}"#,
        Some(&origin_ws),
    )
    .await
    .unwrap();
    assert_eq!(pending(&h, "req-late").as_deref(), Some("m-prev"));
    h.process(r#"{"type": "meeting_leave", "reason": "user_left"}"#, None).await.unwrap();
    assert!(pending(&h, "req-late").unwrap().starts_with("stale:"));
    extension.clear();
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-late", "kind": "manual", "text": "поздняя сводка"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert!(extension.of_type("copilot_reply").is_empty());
    assert_eq!(pending(&h, "req-late"), None);
}

#[tokio::test]
async fn given_stale_request_when_channel_status_arrives_then_nothing_is_relayed() {
    // given
    let h = Harness::new();
    h.bridge.set_pending_request("req-late", "stale:m-prev");
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(r#"{"type": "copilot_channel_status", "request_id": "req-late", "status": "working"}"#, None)
        .await
        .unwrap();
    // then
    assert!(sent(&extension).is_empty());
}

#[tokio::test]
async fn given_live_request_when_channel_reply_arrives_then_it_is_relayed() {
    // given
    let h = Harness::new();
    h.bridge.set_pending_request("req-active", "m-current");
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-active", "kind": "manual", "text": "ответ", "meeting_id": "m-current"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    let relayed = extension.of_type("copilot_reply");
    assert_eq!(relayed.len(), 1);
    assert_eq!(field(&relayed[0], "request_id").as_str(), Some("req-active"));
    assert_eq!(field(&relayed[0], "text").as_str(), Some("ответ"));
}

#[tokio::test]
async fn given_request_of_previous_meeting_when_join_switches_meeting_then_late_reply_is_dropped() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    set_current(&storage, "m-prev");
    *storage.current_path.lock().unwrap() = Some(PathBuf::from("/tmp/m-prev.jsonl"));
    *storage.join_result.lock().unwrap() = (Some("m-prev".into()), Some(PathBuf::from("/tmp/m-prev.jsonl")));
    h.bridge.set_meeting_storage(storage.clone());
    let (_channel, _c) = h.add(Role::Channel);
    let (extension, _e) = h.add(Role::Extension);
    let (_origin, origin_ws) = unregistered(1);
    h.process(r#"{"type": "meeting_join", "url": "https://example.ktalk.ru/m-prev"}"#, None).await.unwrap();
    h.process(r#"{"type": "copilot_chat_message", "request_id": "req-prev", "text": "вопрос"}"#, Some(&origin_ws))
        .await
        .unwrap();
    assert_eq!(pending(&h, "req-prev").as_deref(), Some("m-prev"));
    *storage.join_result.lock().unwrap() = (Some("m-next".into()), Some(PathBuf::from("/tmp/m-next.jsonl")));
    h.process(r#"{"type": "meeting_join", "url": "https://example.ktalk.ru/m-next"}"#, None).await.unwrap();
    assert_eq!(pending(&h, "req-prev").as_deref(), Some("stale:m-prev"));
    extension.clear();
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-prev", "kind": "manual", "text": "опоздавший ответ"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert!(extension.of_type("copilot_reply").is_empty());
    assert_eq!(pending(&h, "req-prev"), None);
}

// ---- send_transcript: source_id payload serialization ----

#[tokio::test]
async fn given_non_empty_source_id_when_send_transcript_then_payload_carries_it() {
    // given
    let h = Harness::new();
    h.attach();
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.bridge
        .send_transcript(&TranscriptLine { source_id: "pid-a".into(), ..line("Alice", "привет", "10:00:00", 0) });
    settle().await;
    // then
    let frames = extension.of_type("transcript");
    assert_eq!(frames.len(), 1);
    assert_eq!(field(&frames[0], "source_id").as_str(), Some("pid-a"));
}

#[tokio::test]
async fn given_default_source_id_when_send_transcript_then_payload_omits_it() {
    // given
    let h = Harness::new();
    h.attach();
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.bridge.send_transcript(&line("Alice", "привет", "10:00:00", 0));
    settle().await;
    // then
    let frames = extension.of_type("transcript");
    assert_eq!(frames.len(), 1);
    assert!(!frames[0].as_object().unwrap().contains_key("source_id"));
}

#[tokio::test]
async fn given_every_field_set_when_send_transcript_then_extension_gets_them_in_pythons_order() {
    // given
    let h = Harness::new();
    h.attach();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    let (extension, _e) = h.add(Role::Extension);
    let full = TranscriptLine {
        is_continuation: true,
        is_local: true,
        uncertain: true,
        uncertain_reason: "mics_off".into(),
        correction: true,
        source_id: "pid-a".into(),
        ..line("Alice", "hello", "10:00:00", 42)
    };
    // when
    h.bridge.send_transcript(&full);
    settle().await;
    // then
    let sent = extension.sent.lock().unwrap().clone();
    assert_eq!(
        sent,
        [concat!(
            r#"{"type": "transcript", "speaker": "Alice", "text": "hello", "timestamp": "10:00:00", "#,
            r#""is_continuation": true, "is_local": true, "uncertain": true, "uncertain_reason": "mics_off", "#,
            r#""correction": true, "meeting_id": "m-1", "segment_id": 42, "source_id": "pid-a"}"#
        )]
    );
}

#[tokio::test]
async fn given_no_loop_when_send_transcript_then_nothing_sent_but_buffer_records() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.bridge.send_transcript(&line("Alice", "offline", "10:00:00", 3));
    settle().await;
    // then
    assert!(extension.frames().is_empty());
    assert_eq!(buffer_ids(&h), [3]);
}

#[tokio::test]
async fn given_due_ticks_when_segments_sent_then_recent_context_lists_speaker_and_text_with_unknown_as_question_mark() {
    // given
    let h = Harness::new();
    h.attach();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    activate_tick_state(&h, "m-1", "");
    let (channel, _c) = h.add(Role::Channel);
    let make_due = || {
        let mut tick = h.bridge.tick_state().unwrap();
        tick.last_emit_ts = h.bridge.now() - COPILOT_TICK_INTERVAL_SECONDS - 1.0;
        h.bridge.set_tick_state(Some(tick));
    };
    // when
    make_due();
    h.bridge.send_transcript(&line("", "без имени", "10:00:00", 1));
    make_due();
    h.bridge.send_transcript(&line("Bob", "второй", "10:00:01", 2));
    settle().await;
    // then
    let ticks = channel.of_type("transcript_tick");
    assert_eq!(ticks.len(), 2);
    assert_eq!(field(&ticks[0], "recent_context").as_str(), Some("?: без имени"));
    assert_eq!(field(&ticks[1], "recent_context").as_str(), Some("?: без имени\nBob: второй"));
    assert_eq!(field(&ticks[1], "meeting_title").as_str(), Some(""));
    let new_ids: Vec<_> =
        array(field(&ticks[1], "new_segments")).iter().map(|s| field(s, "segment_id").clone()).collect();
    assert_eq!(new_ids, [Value::Int(2)]);
}

// ---- Recent transcript buffer + get_recent_transcript snapshot ----

#[test]
fn given_storage_meeting_when_send_transcript_with_segment_id_then_buffer_gets_entry() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    // when
    h.bridge.send_transcript(&line("Alice", "привет всем", "10:00:00", 42));
    // then
    assert_eq!(buffer_ids(&h), [42]);
    let entry = buffer_entry(&h, 42);
    assert_eq!(entry.get("segment_id"), Some(&Value::Int(42)));
    assert_eq!(entry.get("speaker").and_then(Value::as_str), Some("Alice"));
    assert_eq!(entry.get("text").and_then(Value::as_str), Some("привет всем"));
    assert_eq!(entry.get("meeting_id").and_then(Value::as_str), Some("m-1"));
    assert_eq!(entry.get("refined"), Some(&Value::Bool(false)));
    assert!(float(entry.get("start_ms").unwrap()) <= float(entry.get("end_ms").unwrap()));
    assert!((float(entry.get("end_ms").unwrap()) - NOW * 1000.0).abs() < 1.0);
}

#[test]
fn given_storage_meeting_when_send_transcript_without_segment_id_then_buffer_stays_empty() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    // when
    h.bridge.send_transcript(&line("Alice", "no-id", "10:00:00", 0));
    // then
    assert!(h.bridge.transcript_buffer().is_empty());
}

#[test]
fn given_buffered_segment_when_continuation_arrives_then_entry_text_replaced_start_kept() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    h.bridge.send_transcript(&line("Alice", "первая", "10:00:00", 7));
    let first = buffer_entry(&h, 7);
    h.clock.advance(0.005);
    // when
    h.bridge.send_transcript(&TranscriptLine {
        is_continuation: true, ..line("Alice", "первая часть", "10:00:01", 7)
    });
    // then
    assert_eq!(buffer_ids(&h), [7]);
    let updated = buffer_entry(&h, 7);
    assert_eq!(updated.get("text").and_then(Value::as_str), Some("первая часть"));
    assert_eq!(updated.get("start_ms"), first.get("start_ms"));
    assert!(float(updated.get("end_ms").unwrap()) >= float(first.get("end_ms").unwrap()));
}

#[test]
fn given_buffered_segments_when_send_refine_then_entry_text_replaced_and_marked_refined() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    h.bridge.send_transcript(&line("Alice", "тираж1", "10:00:00", 1));
    h.bridge.send_transcript(&line("Alice", "тираж2", "10:00:01", 2));
    // when
    h.bridge.send_refine(&[refine(1, "тираж 1 точный"), refine(99, "не существует")], None);
    // then
    let refined = buffer_entry(&h, 1);
    assert_eq!(refined.get("text").and_then(Value::as_str), Some("тираж 1 точный"));
    assert_eq!(refined.get("refined"), Some(&Value::Bool(true)));
    let untouched = buffer_entry(&h, 2);
    assert_eq!(untouched.get("refined"), Some(&Value::Bool(false)));
    assert_eq!(untouched.get("text").and_then(Value::as_str), Some("тираж2"));
    assert_eq!(buffer_ids(&h), [1, 2]);
}

#[tokio::test]
async fn given_segment_older_than_window_when_recent_transcript_requested_then_only_newer_go_to_originator() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    h.clock.set(NOW - 5.0 * 60.0);
    h.bridge.send_transcript(&line("Alice", "первая", "10:00:00", 1));
    h.clock.set(NOW);
    h.bridge.send_transcript(&line("Bob", "вторая", "10:00:05", 2));
    h.bridge.send_transcript(&line("Alice", "третья", "10:00:10", 3));
    let (requester, requester_ws) = unregistered(1);
    let (other, _o) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "get_recent_transcript_request", "request_id": "snap-1", "window_seconds": 60}"#,
        Some(&requester_ws),
    )
    .await
    .unwrap();
    // then
    let frames = requester.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(field(&frames[0], "type").as_str(), Some("get_recent_transcript_response"));
    assert_eq!(field(&frames[0], "request_id").as_str(), Some("snap-1"));
    assert_eq!(segment_ids(&frames[0]), [2, 3]);
    assert!(sent(&other).is_empty());
}

#[tokio::test]
async fn given_entries_of_two_meetings_when_recent_transcript_requested_then_only_current_meeting_returned() {
    // given
    let h = Harness::new();
    let storage = storage_for_meeting("m-prev");
    h.bridge.set_meeting_storage(storage.clone());
    h.bridge.send_transcript(&line("Alice", "old", "09:59:00", 10));
    set_current(&storage, "m-now");
    h.bridge.send_transcript(&line("Bob", "new", "10:00:00", 11));
    let (requester, requester_ws) = unregistered(1);
    // when
    h.process(
        r#"{"type": "get_recent_transcript_request", "request_id": "snap-2", "window_seconds": 600}"#,
        Some(&requester_ws),
    )
    .await
    .unwrap();
    // then
    assert_eq!(segment_ids(&requester.frames()[0]), [11]);
}

#[tokio::test]
async fn given_three_segments_when_request_has_since_segment_id_then_only_later_ids_returned() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    h.bridge.send_transcript(&line("Alice", "1", "10:00:00", 1));
    h.bridge.send_transcript(&line("Bob", "2", "10:00:01", 2));
    h.bridge.send_transcript(&line("Alice", "3", "10:00:02", 3));
    let (requester, requester_ws) = unregistered(1);
    // when
    h.process(
        r#"{"type": "get_recent_transcript_request", "request_id": "snap-3", "window_seconds": 600, "since_segment_id": 2}"#,
        Some(&requester_ws),
    )
    .await
    .unwrap();
    // then
    assert_eq!(segment_ids(&requester.frames()[0]), [3]);
}

#[tokio::test]
async fn given_refined_and_tier1_segments_when_request_excludes_refined_then_refined_entry_omitted() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    h.bridge.send_transcript(&line("Alice", "tier1", "10:00:00", 1));
    h.bridge.send_transcript(&line("Bob", "tier2", "10:00:01", 2));
    h.bridge.send_refine(&[refine(2, "tier2 refined")], None);
    let (requester, requester_ws) = unregistered(1);
    // when
    h.process(
        r#"{"type": "get_recent_transcript_request", "request_id": "snap-4", "window_seconds": 600, "include_refined": false}"#,
        Some(&requester_ws),
    )
    .await
    .unwrap();
    // then
    assert_eq!(segment_ids(&requester.frames()[0]), [1]);
}

#[tokio::test]
async fn given_originator_when_recent_transcript_request_lacks_request_id_then_logged_and_dropped() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    let (requester, requester_ws) = unregistered(1);
    // when
    h.process(r#"{"type": "get_recent_transcript_request"}"#, Some(&requester_ws)).await.unwrap();
    // then
    assert!(sent(&requester).is_empty());
    assert!(h.logged("get_recent_transcript_request"));
}

#[test]
fn given_full_buffer_when_one_more_segment_recorded_then_oldest_is_evicted() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    let cap = i64::try_from(TRANSCRIPT_BUFFER_CAP).unwrap();
    // when
    for sid in 1..=cap + 1 {
        h.bridge.send_transcript(&line("Alice", &format!("t{sid}"), "10:00:00", sid));
    }
    // then
    let expected: Vec<i64> = (2..=cap + 1).collect();
    assert_eq!(buffer_ids(&h), expected);
}

// ---- Proactive transcript ticks: trigger conditions and suppression ----

#[tokio::test]
async fn given_active_tick_state_and_channel_when_five_segments_sent_then_one_tick_fires() {
    // given
    let h = Harness::new();
    h.attach();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    activate_tick_state(&h, "m-1", "Sprint review");
    let (channel, _c) = h.add(Role::Channel);
    // when
    for sid in 1..=5 {
        h.bridge.send_transcript(&line("Alice", &format!("t{sid}"), "10:00:00", sid));
    }
    settle().await;
    // then
    let ticks = channel.of_type("transcript_tick");
    assert_eq!(ticks.len(), 1);
    let payload = &ticks[0];
    assert_eq!(field(payload, "meeting_id").as_str(), Some("m-1"));
    assert_eq!(field(payload, "meeting_title").as_str(), Some("Sprint review"));
    assert!(!array(field(payload, "new_segments")).is_empty());
    assert!(field(payload, "recent_context").truthy());
    assert!(payload.as_object().unwrap().contains_key("local_user_name"));
}

#[tokio::test]
async fn given_last_tick_older_than_interval_when_one_segment_sent_then_tick_fires() {
    // given
    let h = Harness::new();
    h.attach();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    activate_tick_state(&h, "m-1", "Sprint review");
    let mut tick = h.bridge.tick_state().unwrap();
    tick.last_emit_ts = h.bridge.now() - COPILOT_TICK_INTERVAL_SECONDS - 1.0;
    h.bridge.set_tick_state(Some(tick));
    let (channel, _c) = h.add(Role::Channel);
    // when
    h.bridge.send_transcript(&line("Alice", "одна реплика", "10:00:00", 1));
    settle().await;
    // then
    let ticks = channel.of_type("transcript_tick");
    assert_eq!(ticks.len(), 1);
    assert_eq!(field(&ticks[0], "meeting_id").as_str(), Some("m-1"));
    let ids: Vec<i64> =
        array(field(&ticks[0], "new_segments")).iter().filter_map(|s| int(field(s, "segment_id"))).collect();
    assert_eq!(ids, [1]);
}

#[tokio::test]
async fn given_no_active_meeting_when_segments_cross_count_trigger_then_no_tick() {
    // given
    let h = Harness::new();
    h.attach();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    assert!(h.bridge.tick_state().is_none());
    let (channel, _c) = h.add(Role::Channel);
    // when
    for sid in 1..=6 {
        h.bridge.send_transcript(&line("Alice", &format!("t{sid}"), "10:00:00", sid));
    }
    settle().await;
    // then
    assert!(channel.of_type("transcript_tick").is_empty());
}

#[tokio::test]
async fn given_no_channel_clients_when_segments_cross_count_trigger_then_tick_counters_untouched() {
    // given
    let h = Harness::new();
    h.attach();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    activate_tick_state(&h, "m-1", "Sprint review");
    let seeded_emit_ts = h.bridge.tick_state().unwrap().last_emit_ts;
    assert!(h.bridge.clients_in(Role::Channel).is_empty());
    // when
    for sid in 1..=6 {
        h.bridge.send_transcript(&line("Alice", &format!("t{sid}"), "10:00:00", sid));
    }
    settle().await;
    // then
    let tick = h.bridge.tick_state().unwrap();
    assert_eq!(tick.last_seg_count, 0);
    assert!((tick.last_emit_ts - seeded_emit_ts).abs() < f64::EPSILON);
}

#[tokio::test]
async fn given_storage_when_meeting_join_then_leave_then_tick_state_seeded_and_cleared() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-now", "m-next"));
    // when
    h.process(
        r#"{"type": "meeting_join", "url": "https://test.ktalk.ru/abc", "title": "Daily", "platform": "ktalk"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    let tick = h.bridge.tick_state().expect("tick state seeded on join");
    assert_eq!(tick.meeting_id.as_deref(), Some("m-now"));
    assert_eq!(tick.seg_count, 0);
    assert_eq!(tick.last_seg_count, 0);
    // when
    h.process(r#"{"type": "meeting_leave", "reason": "user_left"}"#, None).await.unwrap();
    // then
    assert!(h.bridge.tick_state().is_none());
}

#[tokio::test]
async fn given_manual_spawn_when_transcripts_arrive_then_tick_state_stays_unseeded() {
    // given
    let h = Harness::new();
    let (_sup, _p) = h.add(Role::Supervisor);
    let (_sub, _s) = h.add(Role::Subscriber);
    h.attach();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    let (channel, _c) = h.add(Role::Channel);
    let (_origin, origin_ws) = unregistered(1);
    h.process(r#"{"type": "copilot_spawn_request", "request_id": "req-manual-tick"}"#, Some(&origin_ws)).await.unwrap();
    assert!(h.bridge.manual_copilot_active());
    assert!(h.bridge.tick_state().is_none());
    // when
    for sid in 1..=6 {
        h.bridge.send_transcript(&line("Alice", &format!("t{sid}"), "10:00:00", sid));
    }
    settle().await;
    // then
    assert_eq!(h.bridge.transcript_buffer().len(), 6);
    assert!(h.bridge.tick_state().is_none());
    assert!(copilot_frames(&channel, "transcript_tick", None).is_empty());
}

#[tokio::test]
async fn given_manual_hello_rearm_when_transcripts_arrive_then_tick_state_stays_unseeded() {
    // given
    let h = Harness::new();
    h.attach();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    let (channel, _c) = h.add(Role::Channel);
    let (_sup, sup_ws) = unregistered(1);
    h.process(r#"{"type": "supervisor_hello", "current_meeting_id": "manual"}"#, Some(&sup_ws)).await.unwrap();
    assert!(h.bridge.manual_copilot_active());
    assert!(h.bridge.tick_state().is_none());
    // when
    for sid in 1..=6 {
        h.bridge.send_transcript(&line("Alice", &format!("t{sid}"), "10:00:00", sid));
    }
    settle().await;
    // then
    assert_eq!(h.bridge.transcript_buffer().len(), 6);
    assert!(h.bridge.tick_state().is_none());
    assert!(copilot_frames(&channel, "transcript_tick", None).is_empty());
}

#[tokio::test]
async fn given_buffered_segments_when_meeting_leave_then_buffer_is_emptied() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(meeting_storage_mock("m-prev", "m-next"));
    h.bridge.send_transcript(&line("Alice", "первая", "10:00:00", 1));
    h.bridge.send_transcript(&line("Bob", "вторая", "10:00:01", 2));
    assert_eq!(buffer_ids(&h), [1, 2]);
    // when
    h.process(r#"{"type": "meeting_leave", "reason": "user_left"}"#, None).await.unwrap();
    // then
    assert!(h.bridge.transcript_buffer().is_empty());
}

#[tokio::test]
async fn given_buffer_of_prior_meeting_when_join_changes_path_then_buffer_is_cleared() {
    // given
    let h = Harness::new();
    let storage = meeting_storage_mock("m-current", "m-next");
    h.bridge.set_meeting_storage(storage.clone());
    set_current(&storage, "m-prior");
    h.bridge.send_transcript(&line("Alice", "stale", "09:59:00", 42));
    set_current(&storage, "m-current");
    assert_eq!(buffer_ids(&h), [42]);
    // when
    h.process(
        r#"{"type": "meeting_join", "url": "https://test.ktalk.ru/new", "title": "Sprint review", "platform": "ktalk"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert!(h.bridge.transcript_buffer().is_empty());
}

#[tokio::test]
async fn given_pending_request_when_reply_relayed_then_entry_is_dropped() {
    // given
    let h = Harness::new();
    h.bridge.set_pending_request("req-alive", "m-current");
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-alive", "kind": "manual", "text": "ответ"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert_eq!(extension.of_type("copilot_reply").len(), 1);
    assert_eq!(pending(&h, "req-alive"), None);
}

#[tokio::test]
async fn given_pending_requests_when_terminal_statuses_arrive_then_entries_are_dropped() {
    // given
    let h = Harness::new();
    h.bridge.set_pending_request("req-done", "m-current");
    h.bridge.set_pending_request("req-err", "m-current");
    let (_extension, _e) = h.add(Role::Extension);
    // when
    h.process(r#"{"type": "copilot_channel_status", "request_id": "req-done", "status": "done"}"#, None).await.unwrap();
    h.process(r#"{"type": "copilot_channel_status", "request_id": "req-err", "status": "error"}"#, None).await.unwrap();
    // then
    assert_eq!(pending(&h, "req-done"), None);
    assert_eq!(pending(&h, "req-err"), None);
}

#[tokio::test]
async fn given_pending_request_when_non_terminal_statuses_arrive_then_entry_is_kept() {
    // given
    let h = Harness::new();
    h.bridge.set_pending_request("req-working", "m-current");
    let (_extension, _e) = h.add(Role::Extension);
    // when
    for status in ["queued", "working"] {
        let frame =
            format!(r#"{{"type": "copilot_channel_status", "request_id": "req-working", "status": "{status}"}}"#);
        h.process(&frame, None).await.unwrap();
    }
    // then
    assert_eq!(pending(&h, "req-working").as_deref(), Some("m-current"));
}

#[test]
fn given_entry_of_current_meeting_when_refine_names_another_meeting_then_entry_untouched() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-current"));
    h.bridge.send_transcript(&line("Alice", "оригинал", "10:00:00", 1));
    // when
    h.bridge.send_refine(&[refine(1, "из другой встречи")], Some("m-other"));
    // then
    let entry = buffer_entry(&h, 1);
    assert_eq!(entry.get("text").and_then(Value::as_str), Some("оригинал"));
    assert_eq!(entry.get("refined"), Some(&Value::Bool(false)));
}

// ---- _end_meeting_on_extension_gone: copilot cleanup parity with meeting_leave ----

#[tokio::test]
async fn given_copilot_state_of_active_meeting_when_only_extension_disconnects_then_same_cleanup_as_leave() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    set_current(&storage, "m-prev");
    *storage.current_path.lock().unwrap() = Some(PathBuf::from("/tmp/m-prev.jsonl"));
    *storage.leave_id.lock().unwrap() = "m-next".to_owned();
    h.bridge.set_meeting_storage(storage);
    h.bridge.send_transcript(&line("Alice", "first", "10:00:00", 7));
    h.bridge.set_pending_request("req-alive", "m-prev");
    activate_tick_state(&h, "m-prev", "Sprint review");
    let (channel, _c) = h.add(Role::Channel);
    let (fake, task) = h.connect(None).await;
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert_eq!(pending(&h, "req-alive").as_deref(), Some("stale:m-prev"));
    assert!(h.bridge.tick_state().is_none());
    assert!(h.bridge.transcript_buffer().is_empty());
    let ended = channel.of_type("meeting_ended");
    assert_eq!(ended.len(), 1);
    assert_eq!(field(&ended[0], "meeting_id").as_str(), Some("m-prev"));
}

// ---- copilot_channel_reply: cross-meeting filter for proactive replies ----

#[tokio::test]
async fn given_current_meeting_when_proactive_reply_names_another_meeting_then_dropped() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-now"));
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "proactive_abc123", "kind": "proactive",
            "text": "вопрос Алисе про CI", "meeting_id": "m-old"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    assert!(extension.of_type("copilot_reply").is_empty());
}

#[tokio::test]
async fn given_current_meeting_when_proactive_reply_names_it_then_relayed() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-now"));
    let (extension, _e) = h.add(Role::Extension);
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "proactive_abc123", "kind": "proactive",
            "text": "вопрос Алисе про CI", "meeting_id": "m-now"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    let relayed = extension.of_type("copilot_reply");
    assert_eq!(relayed.len(), 1);
    assert_eq!(field(&relayed[0], "meeting_id").as_str(), Some("m-now"));
    assert_eq!(field(&relayed[0], "kind").as_str(), Some("proactive"));
}

#[tokio::test]
async fn given_current_meeting_when_reply_has_no_meeting_id_then_relayed() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-now"));
    let (extension, _e) = h.add(Role::Extension);
    h.bridge.set_pending_request("req-known", "m-now");
    // when
    h.process(
        r#"{"type": "copilot_channel_reply", "request_id": "req-known", "kind": "manual", "text": "ответ без meeting_id"}"#,
        None,
    )
    .await
    .unwrap();
    // then
    let relayed = extension.of_type("copilot_reply");
    assert_eq!(relayed.len(), 1);
    assert_eq!(field(&relayed[0], "request_id").as_str(), Some("req-known"));
}

// ---- _end_meeting_on_extension_gone: defensive _load_meta error path ----

#[tokio::test]
async fn given_load_meta_raises_when_only_extension_disconnects_then_handler_survives_and_state_kept() {
    // given
    let h = Harness::new();
    let storage = FakeStore::with_source("ktalk");
    set_current(&storage, "m-prev");
    *storage.current_path.lock().unwrap() = Some(PathBuf::from("/tmp/m-prev.jsonl"));
    storage.set_meta(Err("disk gone".into()));
    h.bridge.set_meeting_storage(storage.clone());
    h.bridge.set_pending_request("req-x", "m-prev");
    let (fake, task) = h.connect(None).await;
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert_eq!(pending(&h, "req-x").as_deref(), Some("m-prev"));
    assert!(storage.ops().is_empty());
}

// ---- get_recent_transcript_request: snapshot stability vs refine writes ----

#[tokio::test]
async fn given_snapshot_sent_when_buffer_entry_refined_afterwards_then_response_keeps_snapshot_values() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    h.bridge.send_transcript(&line("Alice", "live text", "10:00:00", 1));
    let (channel, channel_ws) = unregistered(1);
    h.process(
        r#"{"type": "get_recent_transcript_request", "request_id": "req-snap", "window_seconds": 600}"#,
        Some(&channel_ws),
    )
    .await
    .unwrap();
    // when
    h.bridge.send_refine(&[refine(1, "refined text")], None);
    // then
    assert_eq!(buffer_entry(&h, 1).get("text").and_then(Value::as_str), Some("refined text"));
    let responses = channel.of_type("get_recent_transcript_response");
    assert_eq!(responses.len(), 1);
    let segments = array(field(&responses[0], "segments"));
    assert_eq!(segments.len(), 1);
    assert_eq!(field(&segments[0], "text").as_str(), Some("live text"));
    assert_eq!(field(&segments[0], "refined"), &Value::Bool(false));
}

// ---- edges of the relay (the scenario copilot_channel.json) ----

#[tokio::test]
async fn given_window_fields_of_the_wrong_type_when_recent_transcript_requested_then_python_defaults_apply() {
    // given: a segment 170 s old, one 200 s old, and a refined one now
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    h.clock.set(NOW - 200.0);
    h.bridge.send_transcript(&line("Alice", "old", "09:56:40", 1));
    h.clock.set(NOW - 170.0);
    h.bridge.send_transcript(&line("Bob", "recent", "09:57:10", 2));
    h.clock.set(NOW);
    h.bridge.send_transcript(&line("Alice", "now", "10:00:00", 3));
    h.bridge.send_refine(&[refine(3, "now refined")], None);
    for request in [
        r#"{"type": "get_recent_transcript_request", "request_id": "r"}"#,
        r#"{"type": "get_recent_transcript_request", "request_id": "r", "window_seconds": "600"}"#,
        r#"{"type": "get_recent_transcript_request", "request_id": "r", "window_seconds": -5}"#,
        r#"{"type": "get_recent_transcript_request", "request_id": "r", "since_segment_id": "1", "include_refined": 0}"#,
    ] {
        let (channel, channel_ws) = unregistered(1);
        // when
        h.process(request, Some(&channel_ws)).await.unwrap();
        // then: 180 s, every id, refined included
        let responses = channel.of_type("get_recent_transcript_response");
        assert_eq!(responses.len(), 1, "{request}");
        assert_eq!(segment_ids(&responses[0]), [2, 3], "{request}");
    }
    // when: a bool since_segment_id is an int
    let (channel, channel_ws) = unregistered(1);
    h.process(
        r#"{"type": "get_recent_transcript_request", "request_id": "r", "window_seconds": 600, "since_segment_id": true}"#,
        Some(&channel_ws),
    )
    .await
    .unwrap();
    // then
    assert_eq!(segment_ids(&channel.of_type("get_recent_transcript_response")[0]), [2, 3]);
}

#[tokio::test]
async fn given_no_socket_or_a_dead_one_when_recent_transcript_requested_then_response_dropped_with_a_log() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    let request = r#"{"type": "get_recent_transcript_request", "request_id": "r"}"#;
    // when
    h.process(request, None).await.unwrap();
    // then
    assert!(h.logged("[KTalk Bridge] get_recent_transcript_request without websocket — response dropped"));
    // when
    let dead = Client::new(ClientId(u64::MAX), RecordingSink::with_mode(SendMode::Fail("gone".into())), "x");
    h.process(request, Some(&dead)).await.unwrap();
    // then
    assert!(h.logged("[KTalk Bridge] Failed to send get_recent_transcript_response: gone"));
}

#[tokio::test]
async fn given_no_channel_when_chat_arrives_without_socket_or_on_a_dead_one_then_dropped_with_a_log() {
    // given
    let h = Harness::new();
    let chat = r#"{"type": "copilot_chat_message", "request_id": "q1", "text": "Hi"}"#;
    // when
    h.process(chat, None).await.unwrap();
    // then
    assert!(h.logged("[KTalk Bridge] copilot_chat_message dropped — no channel client and no originator socket"));
    // when
    let dead = Client::new(ClientId(u64::MAX), RecordingSink::with_mode(SendMode::Fail("gone".into())), "x");
    h.process(chat, Some(&dead)).await.unwrap();
    // then
    assert!(h.logged("[KTalk Bridge] Failed to send channel_disconnected status: gone"));
    assert!(h.bridge.pending_requests().is_empty());
}

#[tokio::test]
async fn given_dead_channel_when_chat_forwarded_then_failure_logged_and_request_tracked() {
    // given
    let h = Harness::new();
    let dead = RecordingSink::with_mode(SendMode::Fail("gone".into()));
    let _channel = h.bridge.add_client(Role::Channel, dead, "x");
    let (subscriber, _s) = h.add(Role::Subscriber);
    // when
    h.process(r#"{"type": "copilot_chat_message", "request_id": "q1", "text": "Hi", "created_at": 5}"#, None)
        .await
        .unwrap();
    // then
    assert!(h.logged("[KTalk Bridge] Failed to forward copilot_chat_message to channel client: gone"));
    assert_eq!(h.bridge.pending_requests(), [("q1".to_owned(), String::new())]);
    assert_eq!(
        sent(&subscriber),
        [r#"{"type": "copilot_chat_echo", "request_id": "q1", "text": "Hi", "created_at": 5}"#]
    );
}

#[tokio::test]
async fn given_manual_request_gone_stale_when_its_reply_arrives_then_error_names_the_interrupted_session() {
    // given
    let h = Harness::new();
    let (extension, _e) = h.add(Role::Extension);
    h.bridge.set_pending_request("q1", "stale:");
    // when
    h.process(r#"{"type": "copilot_channel_reply", "request_id": "q1", "text": "Late", "kind": "answer"}"#, None)
        .await
        .unwrap();
    // then
    assert_eq!(
        sent(&extension),
        [
            r#"{"type": "copilot_status", "request_id": "q1", "status": "error", "message": "Ответ не доставлен: ручная сессия copilot была прервана."}"#
        ]
    );
    assert!(h.bridge.pending_requests().is_empty());
}

#[tokio::test]
async fn given_pending_requests_when_settings_switch_copilot_off_then_each_fails_with_status_before_request_id() {
    // given
    let h = Harness::new();
    let (subscriber, sub) = h.add(Role::Subscriber);
    h.bridge.set_pending_request("q1", "m1");
    h.bridge.set_pending_request("q2", "");
    h.policy.set_enabled(false);
    // when
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    // then
    let sent = sent(&subscriber);
    assert_eq!(
        sent[..2],
        [
            r#"{"type": "copilot_status", "status": "error", "request_id": "q1", "message": "Copilot выключен в настройках."}"#,
            r#"{"type": "copilot_status", "status": "error", "request_id": "q2", "message": "Copilot выключен в настройках."}"#,
        ]
    );
    assert!(h.bridge.pending_requests().is_empty());
    assert_eq!(subscriber.of_type("ai_settings_applied").len(), 1);
}

#[tokio::test]
async fn copilot_off_when_channel_asks_recent_transcript_then_no_segments() {
    // given
    let h = Harness::new();
    h.bridge.set_meeting_storage(storage_for_meeting("m-1"));
    h.clock.set(NOW);
    h.bridge.send_transcript(&line("Alice", "первая", "10:00:00", 1));
    let (_subscriber, sub) = h.add(Role::Subscriber);
    h.policy.set_enabled(false);
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    let (requester, requester_ws) = unregistered(1);
    // when
    h.process(
        r#"{"type": "get_recent_transcript_request", "request_id": "snap-1", "window_seconds": 60}"#,
        Some(&requester_ws),
    )
    .await
    .unwrap();
    // then
    let frames = requester.frames();
    assert_eq!(frames.len(), 1);
    assert_eq!(field(&frames[0], "type").as_str(), Some("get_recent_transcript_response"));
    assert!(segment_ids(&frames[0]).is_empty());
}
