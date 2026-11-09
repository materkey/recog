//! The channel's behaviour, case by case from the Python tests of
//! `recog/copilot_channel.py`: tests/test_copilot_channel.py, test_copilot_context.py,
//! and the channel cases of test_privacy.py, test_copilot_ai_settings.py and
//! test_user_profile_runtime.py. The bridge is a real WebSocket server or a recording
//! socket; the MCP sessions are real HTTP sessions on a random port.
#![forbid(unsafe_code)]

mod channel_support;

use std::sync::Arc;
use std::time::Duration;

use channel_support::*;
use recog_copilot::channel::context::{build_copilot_context, format_channel_event};
use recog_copilot::channel::handler::{self, ChannelHandler};
use recog_copilot::channel::link::{BridgeLink, Socket};
use recog_copilot::channel::transport::McpServer;
use recog_copilot::channel::{self, SessionCountReporter, ZombieSessionReaper, handle_inbound};
use recog_copilot::log::Capture;
use recog_core::privacy::{PrivacySource as _, PrivacyStore};
use recog_core::pyjson::{Map, Value};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// A link and the `_ws_loop` connected to `bridge`.
async fn start_link(dir: &tempfile::TempDir, bridge: &FakeBridge) -> (Arc<BridgeLink>, tokio::task::JoinHandle<()>) {
    let link = link_in(dir);
    let server = Arc::new(McpServer::new(link.clone(), "127.0.0.1"));
    let task = tokio::spawn(channel::ws_loop(bridge.url.clone(), server, false));
    (link, task)
}

async fn connected(link: &BridgeLink) {
    assert!(wait_for(|| link.is_connected()).await, "the link failed to connect to the fake bridge");
}

fn queued(link: &BridgeLink) -> Vec<Map> {
    std::iter::from_fn(|| link.events.try_get()).collect()
}

// --- tests/test_copilot_channel.py ---

#[tokio::test]
async fn fake_bridge_when_link_connects_then_first_frame_is_hello() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = FakeBridge::start(|_| None).await;
    // when
    let (link, task) = start_link(&dir, &bridge).await;
    // then
    assert_eq!(dumps(&bridge.next().await), r#"{"type": "copilot_channel_hello"}"#);
    connected(&link).await;
    task.abort();
}

#[tokio::test]
async fn connected_link_when_send_copilot_message_called_then_bridge_gets_channel_reply() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = FakeBridge::start(|_| None).await;
    let (link, task) = start_link(&dir, &bridge).await;
    assert_eq!(bridge.next().await.get("type"), Some(&Value::from("copilot_channel_hello")));
    connected(&link).await;
    // when
    let (result, is_error) = call_tool(
        &link,
        "send_copilot_message",
        r#"{"kind": "manual", "text": "Алиса спросила про метрики", "request_id": "req-test-42",
            "meeting_id": "2026-04-26_meet"}"#,
    )
    .await;
    // then
    assert_eq!((result, is_error), (json!({"ok": true}), false));
    let reply = bridge.next().await;
    assert_eq!(reply.get("type"), Some(&Value::from("copilot_channel_reply")));
    assert_eq!(reply.get("request_id"), Some(&Value::from("req-test-42")));
    assert_eq!(reply.get("kind"), Some(&Value::from("manual")));
    assert_eq!(reply.get("text"), Some(&Value::from("Алиса спросила про метрики")));
    assert_eq!(reply.get("meeting_id"), Some(&Value::from("2026-04-26_meet")));
    assert!(matches!(reply.get("created_at"), Some(Value::Float(_))));
    task.abort();
}

#[tokio::test]
async fn blocked_send_when_get_recent_transcript_cancelled_then_pending_is_cleaned() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let started = Arc::new(tokio::sync::Notify::new());
    link.set_socket(Some(Arc::new(BlockingSocket { started: started.clone() })));
    let cancel = tokio_util::sync::CancellationToken::new();
    let handler = ChannelHandler { link: link.clone() };
    let call = {
        let cancel = cancel.clone();
        tokio::spawn(async move { handler.call("get_recent_transcript", &Map::new(), &cancel).await })
    };
    tokio::time::timeout(Duration::from_secs(2), started.notified()).await.unwrap();
    assert_eq!(link.pending_count(), 1);
    // when
    cancel.cancel();
    // then
    let outcome = call.await.unwrap();
    assert_eq!(outcome.unwrap_err().message, "Request cancelled");
    assert_eq!(link.pending_count(), 0);
}

#[tokio::test]
async fn responding_bridge_when_get_recent_transcript_called_then_round_trips() {
    // given
    let segment = object(
        r#"{"segment_id": 5, "speaker": "Alice", "text": "что только что сказала Алиса?", "start_ms": 1000,
            "end_ms": 2000, "refined": false, "meeting_id": "2026-04-26_meet"}"#,
    );
    let answer_segment = segment.clone();
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = FakeBridge::start(move |frame| {
        (frame.get("type") == Some(&Value::from("get_recent_transcript_request"))).then(|| {
            let mut answer = Map::new();
            answer.insert("type", Value::from("get_recent_transcript_response"));
            answer.insert("request_id", frame.get("request_id").cloned().unwrap());
            answer.insert("segments", Value::Array(vec![Value::Object(answer_segment.clone())]));
            answer
        })
    })
    .await;
    let (link, task) = start_link(&dir, &bridge).await;
    bridge.next().await;
    connected(&link).await;
    // when
    let (result, _) = call_tool(&link, "get_recent_transcript", "{}").await;
    // then
    assert_eq!(result["ok"], json!(true));
    assert_eq!(
        result["segments"].to_string(),
        json!([serde_json::from_str::<serde_json::Value>(&dumps(&segment)).unwrap()]).to_string()
    );
    let request = bridge.next().await;
    assert_eq!(request.get("type"), Some(&Value::from("get_recent_transcript_request")));
    assert_eq!(request.get("window_seconds"), Some(&Value::Int(180)));
    assert_eq!(request.get("since_segment_id"), Some(&Value::Null));
    assert_eq!(request.get("include_refined"), Some(&Value::Bool(true)));
    assert!(request.get("request_id").and_then(Value::as_str).unwrap().starts_with("snapshot_"));
    task.abort();
}

#[tokio::test]
async fn bridge_pushing_after_hello_when_awaiting_event_then_returns_the_frame() {
    // given
    let message = object(
        r#"{"type": "copilot_user_message", "request_id": "ext-req-99", "text": "что нового?",
            "meeting_id": "smoke-meet", "meeting_title": "Smoke", "transcript_window": "[14:20] X: ...",
            "created_at": 1.0}"#,
    );
    let pushed = message.clone();
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = FakeBridge::start(move |frame| {
        (frame.get("type") == Some(&Value::from("copilot_channel_hello"))).then(|| pushed.clone())
    })
    .await;
    let (link, task) = start_link(&dir, &bridge).await;
    bridge.next().await;
    connected(&link).await;
    // when
    let (result, _) = call_tool(&link, "await_copilot_event", r#"{"timeout_seconds": 2}"#).await;
    // then
    assert_eq!(result["ok"], json!(true));
    assert_eq!(
        result["event"].to_string(),
        serde_json::from_str::<serde_json::Value>(&dumps(&message)).unwrap().to_string()
    );
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn empty_queue_when_awaiting_event_then_null_after_timeout() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    // when
    let (result, _) = call_tool(&link, "await_copilot_event", r#"{"timeout_seconds": 1}"#).await;
    // then
    assert_eq!(result, json!({"ok": true, "event": null}));
}

#[tokio::test]
async fn non_positive_timeout_when_awaiting_event_then_structured_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    // when
    let (result, _) = call_tool(&link, "await_copilot_event", r#"{"timeout_seconds": 0}"#).await;
    // then
    assert_eq!(result["ok"], json!(false));
    assert!(result["error"].as_str().unwrap().contains("positive int"));
}

#[tokio::test]
async fn full_queue_of_ticks_when_event_arrives_then_oldest_is_evicted() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = BridgeLink::with_queue(PrivacyStore::new(dir.path()), 2);
    for n in 0..2 {
        link.events.push(object(&format!(r#"{{"type": "transcript_tick", "n": {n}}}"#)));
    }
    // when
    handle_inbound(br#"{"type": "transcript_tick", "n": 99}"#, &link, None).await;
    // then
    let numbers: Vec<Value> = queued(&link).iter().map(|e| e.get("n").cloned().unwrap()).collect();
    assert_eq!(numbers, [Value::Int(1), Value::Int(99)]);
}

#[test]
fn built_server_when_initialization_options_created_then_channel_capability_present() {
    // given / when
    let capabilities = serde_json::to_value(handler::capabilities()).unwrap();
    // then
    assert_eq!(capabilities["experimental"]["claude/channel"], json!({}));
}

#[test]
fn tool_arguments_when_taken_from_rmcp_json_then_integers_stay_integers() {
    // given
    let arguments = json!({"n": null, "b": true, "i": -5, "u": u64::MAX, "f": 1.5, "whole": 5.0, "s": "x",
                           "a": [1, [2.5]], "o": {"k": false}});
    let serde_json::Value::Object(arguments) = arguments else { unreachable!() };
    // when
    let map = handler::map_from_json(&arguments);
    // then
    assert_eq!(
        dumps(&map),
        r#"{"n": null, "b": true, "i": -5, "u": 18446744073709551615, "f": 1.5, "whole": 5.0, "s": "x", "a": [1, [2.5]], "o": {"k": false}}"#
    );
    assert_eq!(map.get("u"), Some(&Value::Int(i128::from(u64::MAX))));
    assert_eq!(map.get("whole"), Some(&Value::Float(5.0)));
}

#[tokio::test]
async fn unreadable_privacy_setting_when_event_pushed_then_failure_logged_with_python_class() {
    // given
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("privacy.json")).unwrap();
    let link = link_in(&dir);
    let server = McpServer::new(link.clone(), "127.0.0.1");
    let capture = Capture::start();
    // when
    handle_inbound(br#"{"type": "transcript_tick"}"#, &link, Some(&server)).await;
    // then
    let log = capture.text();
    assert!(
        log.contains("[copilot-channel] channel push failed for 'transcript_tick': IsADirectoryError: [Errno 21]"),
        "{log}"
    );
    assert_eq!(link.events.try_get().map(|e| dumps(&e)), Some(r#"{"type": "transcript_tick"}"#.to_owned()));
}

#[test]
fn user_message_when_formatted_then_reply_hint_and_transport_meta() {
    // given
    let event = object(
        r#"{"type": "copilot_user_message", "request_id": "req-7", "text": "что в репо?", "meeting_id": "meet-1",
            "meeting_title": "Daily"}"#,
    );
    // when
    let (content, meta) = format_channel_event(&event).unwrap();
    // then
    assert!(content.contains("что в репо?"));
    assert!(content.contains("Reply with send_copilot_message"));
    assert_eq!(
        dumps(&meta),
        r#"{"event_type": "copilot_user_message", "request_id": "req-7", "meeting_id": "meet-1", "meeting_title": "Daily"}"#
    );
}

#[test]
fn transcript_tick_when_formatted_then_counts_segments_and_keeps_context() {
    // given
    let event = object(
        r#"{"type": "transcript_tick", "meeting_id": "meet-2", "new_segments": [{"id": 1}, {"id": 2}],
            "recent_context": "[14:20] X: hi", "local_user_name": "Слава"}"#,
    );
    // when
    let (content, meta) = format_channel_event(&event).unwrap();
    // then
    assert!(content.contains("Transcript tick: 2 new segment(s)"));
    assert!(content.contains("[14:20] X: hi"));
    assert_eq!(meta.get("event_type"), Some(&Value::from("transcript_tick")));
    assert_eq!(meta.get("meeting_id"), Some(&Value::from("meet-2")));
    assert_eq!(meta.get("local_user_name"), Some(&Value::from("Слава")));
}

#[test]
fn each_event_kind_when_formatted_then_python_push_text() {
    // given
    let cases = [
        (
            r#"{"type": "copilot_user_message"}"#,
            "User typed in the Copilot tab. Reply with send_copilot_message kind='reply' carrying the original request_id.\n\nText: ",
        ),
        (
            r#"{"type": "copilot_user_message", "text": 7}"#,
            "User typed in the Copilot tab. Reply with send_copilot_message kind='reply' carrying the original request_id.\n\nText: 7",
        ),
        (
            r#"{"type": "transcript_tick", "new_segments": null, "recent_context": ""}"#,
            "Transcript tick: 0 new segment(s). Apply the current editable Copilot instructions.\n\nRecent context:\n",
        ),
        (
            r#"{"type": "transcript_tick", "new_segments": "abc", "recent_context": 5}"#,
            "Transcript tick: 3 new segment(s). Apply the current editable Copilot instructions.\n\nRecent context:\n5",
        ),
        (
            r#"{"type": "transcript_tick", "new_segments": {"a": 1}}"#,
            "Transcript tick: 1 new segment(s). Apply the current editable Copilot instructions.\n\nRecent context:\n",
        ),
        (r#"{"type": "meeting_started"}"#, "Meeting started: (untitled). Reset internal proactive state."),
        (
            r#"{"type": "meeting_started", "meeting_title": "Синк {проекта}"}"#,
            "Meeting started: Синк {проекта}. Reset internal proactive state.",
        ),
        (
            r#"{"type": "user_profile_updated"}"#,
            "Local user profile updated. Replace the previous profile with the current user context. Do not send a reply for this event.",
        ),
        (
            r#"{"type": "copilot_policy_updated"}"#,
            "Copilot instructions updated. Replace the previous policy with the current user configuration, including cleared instructions. Apply it from this event onward. Do not send a reply for this event.",
        ),
        (r#"{"type": "meeting_ended"}"#, "Meeting ended. Stop polling and exit the copilot loop."),
        (r#"{"type": "something_else", "x": "é"}"#, r#"{"type": "something_else", "x": "é"}"#),
        (r#"{"x": 1}"#, r#"{"x": 1}"#),
    ];
    for (event, expected) in cases {
        // when
        let (content, _) = format_channel_event(&object(event)).unwrap();
        // then
        assert_eq!(content, expected, "{event}");
    }
}

#[test]
fn event_without_type_when_formatted_then_unknown_kind_and_json_meta_values() {
    // given
    let event = object(
        r#"{"type": "", "request_id": 5, "meeting_id": null, "meeting_title": ["a"], "local_user_identity_known": false,
            "privacy_mode": true, "copilot_policy_revision": {"r": 1}}"#,
    );
    // when
    let (_, meta) = format_channel_event(&event).unwrap();
    // then
    assert_eq!(
        dumps(&meta),
        r#"{"event_type": "unknown", "request_id": "5", "meeting_title": "[\"a\"]", "local_user_identity_known": "false", "copilot_policy_revision": "{\"r\": 1}", "privacy_mode": "true"}"#
    );
}

#[test]
fn tick_with_unsized_new_segments_when_formatted_then_python_type_error() {
    // given
    let event = object(r#"{"type": "transcript_tick", "new_segments": 5}"#);
    // when
    let error = format_channel_event(&event).unwrap_err();
    // then
    assert_eq!(error, "TypeError: object of type 'int' has no len()");
}

#[test]
fn malformed_meta_key_when_formatted_then_only_known_keys_survive() {
    // given
    let event = object(r#"{"type": "meeting_started", "meeting_id": "abc", "meeting-with-hyphen": "should drop"}"#);
    // when
    let (content, meta) = format_channel_event(&event).unwrap();
    // then
    assert!(!content.contains("abc"));
    assert!(!meta.contains_key("meeting-with-hyphen"));
    assert_eq!(meta.get("event_type"), Some(&Value::from("meeting_started")));
    assert_eq!(meta.get("meeting_id"), Some(&Value::from("abc")));
}

#[tokio::test]
async fn two_sessions_with_get_streams_when_broadcast_then_both_get_channel_notification() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (server, address) = mcp_endpoint(link_in(&dir)).await;
    let (a, b) = (initialize(address).await, initialize(address).await);
    let mut streams = [open_get_stream(address, &a).await, open_get_stream(address, &b).await];
    assert!(wait_for(|| server.reachable_count() == 2).await);
    // when
    let sent = server.broadcast("test content", &object(r#"{"event_type": "transcript_tick"}"#));
    // then
    assert_eq!(sent, 2);
    for stream in &mut streams {
        let notification = tokio::time::timeout(Duration::from_secs(2), stream.recv()).await.unwrap().unwrap();
        assert_eq!(
            notification,
            json!({"jsonrpc": "2.0", "method": "notifications/claude/channel",
                   "params": {"content": "test content", "meta": {"event_type": "transcript_tick"}}})
        );
    }
}

#[tokio::test]
async fn zombie_session_when_broadcast_then_skipped_and_not_counted() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (server, address) = mcp_endpoint(link_in(&dir)).await;
    let live = initialize(address).await;
    let _zombie = initialize(address).await;
    let mut stream = open_get_stream(address, &live).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    // when
    let sent = server.broadcast("content", &Map::new());
    // then
    assert_eq!(sent, 1);
    assert!(tokio::time::timeout(Duration::from_secs(2), stream.recv()).await.unwrap().is_some());
}

#[tokio::test]
async fn live_and_zombie_sessions_when_reporter_polls_then_counts_only_reachable() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let socket = RecordingSocket::new();
    link.set_socket(Some(socket.clone()));
    let (server, address) = mcp_endpoint(link).await;
    let live = initialize(address).await;
    initialize(address).await;
    let _stream = open_get_stream(address, &live).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    let mut reporter = SessionCountReporter::new(server);
    // when
    reporter.poll_once().await;
    // then
    assert_eq!(dumps(socket.frames().last().unwrap()), r#"{"type": "claude_sessions", "count": 1}"#);
}

#[tokio::test]
async fn zombie_session_when_swept_three_times_then_reaped_and_terminated() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (server, address) = mcp_endpoint(link_in(&dir)).await;
    let live = initialize(address).await;
    let zombie = initialize(address).await;
    let _stream = open_get_stream(address, &live).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    let mut reaper = ZombieSessionReaper::new(server.clone(), 3);
    // when
    let sweeps = [reaper.sweep_once(), reaper.sweep_once()];
    let kept = server.session_list().iter().any(|s| s.id == zombie);
    let third = reaper.sweep_once();
    // then
    assert_eq!(sweeps, [Vec::<String>::new(), Vec::new()]);
    assert!(kept);
    assert_eq!(third, std::slice::from_ref(&zombie));
    assert!(!server.session_list().iter().any(|s| s.id == zombie));
    assert!(server.session_list().iter().any(|s| s.id == live));
    let after = post(address, Some(&zombie), &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await;
    assert_eq!(after.status, 404);
}

#[tokio::test]
async fn get_stream_returning_when_swept_past_threshold_then_never_reaped() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (server, address) = mcp_endpoint(link_in(&dir)).await;
    let session = initialize(address).await;
    let mut reaper = ZombieSessionReaper::new(server.clone(), 3);
    reaper.sweep_once();
    reaper.sweep_once();
    // when
    let stream = open_get_stream(address, &session).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    reaper.sweep_once();
    drop(stream);
    assert!(wait_for(|| server.reachable_count() == 0).await);
    reaper.sweep_once();
    reaper.sweep_once();
    // then
    assert!(server.session_list().iter().any(|s| s.id == session && !s.is_terminated()));
}

#[tokio::test]
async fn full_queue_with_question_when_event_arrives_then_tick_is_evicted() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = BridgeLink::with_queue(PrivacyStore::new(dir.path()), 2);
    link.events.push(object(r#"{"type": "copilot_user_message", "text": "вопрос"}"#));
    link.events.push(object(r#"{"type": "transcript_tick", "n": 1}"#));
    // when
    handle_inbound(r#"{"type": "copilot_user_message", "text": "ещё вопрос"}"#.as_bytes(), &link, None).await;
    // then
    let texts: Vec<Value> = queued(&link).iter().map(|e| e.get("text").cloned().unwrap()).collect();
    assert_eq!(texts, [Value::from("вопрос"), Value::from("ещё вопрос")]);
}

#[tokio::test]
async fn no_sessions_when_broadcast_then_zero_without_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::new(link_in(&dir), "127.0.0.1");
    // when / then
    assert_eq!(server.broadcast("x", &Map::new()), 0);
}

#[tokio::test]
async fn every_event_type_when_inbound_then_queued_and_snapshot_resolves_pending() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let types = [
        "copilot_user_message",
        "transcript_tick",
        "meeting_started",
        "meeting_ended",
        "user_profile_updated",
        "copilot_policy_updated",
    ];
    // when
    for kind in types {
        handle_inbound(format!(r#"{{"type": "{kind}", "marker": "{kind}"}}"#).as_bytes(), &link, None).await;
    }
    let answer = link.register_pending("snap-x");
    handle_inbound(
        br#"{"type": "get_recent_transcript_response", "request_id": "snap-x", "segments": []}"#,
        &link,
        None,
    )
    .await;
    // then
    assert!(answer.await.unwrap().is_ok());
    let kinds: Vec<Value> = queued(&link).iter().map(|e| e.get("type").cloned().unwrap()).collect();
    assert_eq!(kinds, types.map(Value::from));
}

#[tokio::test]
async fn changing_count_when_polled_then_frames_only_on_transitions() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let (server, address) = mcp_endpoint(link.clone()).await;
    let mut reporter = SessionCountReporter::new(server.clone());
    let socket = RecordingSocket::new();
    link.set_socket(Some(socket.clone()));
    // when
    let first = reporter.poll_once().await;
    let session = initialize(address).await;
    let stream = open_get_stream(address, &session).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    let up = reporter.poll_once().await;
    let steady = reporter.poll_once().await;
    drop(stream);
    assert!(wait_for(|| server.reachable_count() == 0).await);
    let down = reporter.poll_once().await;
    let steady_zero = reporter.poll_once().await;
    // then
    let ok = Some(object(r#"{"ok": true}"#));
    assert_eq!([first, up, steady, down, steady_zero], [ok.clone(), ok.clone(), None, ok, None]);
    let frames: Vec<String> = socket.frames().iter().map(dumps).collect();
    assert_eq!(
        frames,
        [
            r#"{"type": "claude_sessions", "count": 0}"#,
            r#"{"type": "claude_sessions", "count": 1}"#,
            r#"{"type": "claude_sessions", "count": 0}"#
        ]
    );
}

#[tokio::test]
async fn reconnected_socket_when_count_unchanged_then_reannounced() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let (server, address) = mcp_endpoint(link.clone()).await;
    let session = initialize(address).await;
    let _stream = open_get_stream(address, &session).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    let mut reporter = SessionCountReporter::new(server);
    let first = RecordingSocket::new();
    link.set_socket(Some(first.clone()));
    assert!(reporter.poll_once().await.is_some());
    link.set_socket(None);
    assert!(reporter.poll_once().await.is_none());
    // when
    let second = RecordingSocket::new();
    link.set_socket(Some(second.clone()));
    let result = reporter.poll_once().await;
    // then
    assert_eq!(result, Some(object(r#"{"ok": true}"#)));
    assert_eq!(first.frames().iter().map(dumps).collect::<Vec<_>>(), [r#"{"type": "claude_sessions", "count": 1}"#]);
    assert_eq!(second.frames().iter().map(dumps).collect::<Vec<_>>(), [r#"{"type": "claude_sessions", "count": 1}"#]);
}

#[tokio::test]
async fn endpoint_not_serving_when_reporter_polls_then_reports_zero() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let server = Arc::new(McpServer::new(link.clone(), "127.0.0.1"));
    let socket = RecordingSocket::new();
    link.set_socket(Some(socket.clone()));
    let mut reporter = SessionCountReporter::new(server);
    // when
    let result = reporter.poll_once().await;
    // then
    assert_eq!(result, Some(object(r#"{"ok": true}"#)));
    assert_eq!(socket.frames().iter().map(dumps).collect::<Vec<_>>(), [r#"{"type": "claude_sessions", "count": 0}"#]);
}

#[tokio::test]
async fn link_never_connected_when_reporter_polls_then_nothing_sent() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let mut reporter = SessionCountReporter::new(Arc::new(McpServer::new(link.clone(), "127.0.0.1")));
    // when / then
    assert!(!link.is_connected());
    assert!(reporter.poll_once().await.is_none());
}

#[tokio::test]
async fn event_when_inbound_with_sessions_then_push_logged_with_count() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let (server, address) = mcp_endpoint(link_in(&dir)).await;
    let session = initialize(address).await;
    let mut stream = open_get_stream(address, &session).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    let capture = Capture::start();
    // when
    handle_inbound(br#"{"type": "meeting_ended", "meeting_id": "m-1"}"#, &server.link, Some(&server)).await;
    // then
    assert!(capture.text().contains("[copilot-channel] channel push 'meeting_ended' → 1 session(s)"));
    let pushed = tokio::time::timeout(Duration::from_secs(2), stream.recv()).await.unwrap().unwrap();
    assert_eq!(pushed["params"]["content"], json!("Meeting ended. Stop polling and exit the copilot loop."));
    assert_eq!(pushed["params"]["meta"], json!({"event_type": "meeting_ended", "meeting_id": "m-1"}));
}

#[tokio::test]
async fn frame_over_one_mebibyte_when_received_then_link_reconnects_like_websockets() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let mut bridge = FakeBridge::start(|_| None).await;
    let (link, task) = start_link(&dir, &bridge).await;
    assert_eq!(bridge.next().await.get("type"), Some(&Value::from("copilot_channel_hello")));
    connected(&link).await;
    let capture = Capture::start();
    // when
    let big = format!(r#"{{"type": "transcript_tick", "recent_context": "{}"}}"#, "x".repeat(1 << 20));
    bridge.push.send(big).unwrap();
    // then
    let hello = tokio::time::timeout(Duration::from_secs(5), bridge.received.recv()).await.unwrap().unwrap();
    assert_eq!(hello.get("type"), Some(&Value::from("copilot_channel_hello")));
    assert!(link.events.is_empty());
    assert!(capture.text().contains("[copilot-channel] bridge unavailable ("), "{}", capture.text());
    task.abort();
}

/// A bridge that plays `script` on its first connection (after reading the hello) and
/// then closes it with a close frame; later connections only read their hello. Every
/// hello read is counted.
async fn scripted_bridge(script: Vec<WsMessage>) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use futures_util::{SinkExt as _, StreamExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let hellos = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = hellos.clone();
    tokio::spawn(async move {
        let mut script = Some(script);
        while let Ok((stream, _)) = listener.accept().await {
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            if let Some(Ok(WsMessage::Text(_))) = socket.next().await {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let Some(script) = script.take() else {
                tokio::spawn(async move { while socket.next().await.is_some() {} });
                continue;
            };
            for message in script {
                socket.send(message).await.unwrap();
            }
            socket.close(None).await.unwrap();
        }
    });
    (url, hellos)
}

#[tokio::test]
async fn bridge_closing_cleanly_when_link_reconnects_then_reconnected_and_no_failure_logged() {
    // given
    let (url, hellos) = scripted_bridge(vec![
        WsMessage::Ping(b"p".to_vec().into()),
        WsMessage::Binary(br#"{"type": "meeting_started", "meeting_id": "m-1"}"#.to_vec().into()),
        WsMessage::Text(r#"{"type": "meeting_ended"}"#.into()),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let server = Arc::new(McpServer::new(link.clone(), "127.0.0.1"));
    let capture = Capture::start();
    // when
    let task = tokio::spawn(channel::ws_loop(url.clone(), server, false));
    // then
    assert!(wait_for(|| hellos.load(std::sync::atomic::Ordering::SeqCst) == 2).await);
    let log = capture.text();
    assert!(log.contains(&format!("[copilot-channel] connected to {url}")), "{log}");
    assert!(log.contains(&format!("[copilot-channel] reconnected to {url}")), "{log}");
    assert!(!log.contains("bridge unavailable"), "{log}");
    assert_eq!(
        queued(&link).iter().map(dumps).collect::<Vec<_>>(),
        [r#"{"type": "meeting_started", "meeting_id": "m-1"}"#, r#"{"type": "meeting_ended"}"#]
    );
    task.abort();
}

#[tokio::test]
async fn bridge_unreachable_when_link_retries_then_failure_logged_once_per_minute() {
    // given
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", closed.local_addr().unwrap());
    drop(closed);
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let server = Arc::new(McpServer::new(link.clone(), "127.0.0.1"));
    let capture = Capture::start();
    // when
    let task = tokio::spawn(channel::ws_loop(url, server, false));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // then
    let log = capture.text();
    let failures: Vec<&str> = log.lines().filter(|l| l.contains("bridge unavailable")).collect();
    assert_eq!(failures.len(), 1, "{log}");
    assert!(failures[0].starts_with("[copilot-channel] bridge unavailable (OSError: "), "{log}");
    assert!(failures[0].ends_with("); retry in 1.0s"), "{log}");
    assert!(!link.is_connected());
    task.abort();
}

// --- tests/test_copilot_context.py ---

const IDENTITY: &str = r#""local_user_name": "Имя {{GOAL}}", "local_user_aliases": ["Имя {{GOAL}}", "Alias \"quoted\""],
    "local_user_profile_revision": "rev-1", "local_user_identity_known": true"#;

fn identity_event(kind: &str) -> Map {
    object(&format!(r#"{{"type": "{kind}", {IDENTITY}}}"#))
}

/// The JSON of a block, after its heading line.
fn block_data(block: &str) -> serde_json::Value {
    serde_json::from_str(block.split_once('\n').unwrap().1).unwrap()
}

#[test]
fn identity_when_formatting_push_then_literal_profile_precedes_event() {
    for kind in ["transcript_tick", "meeting_started", "copilot_user_message", "user_profile_updated"] {
        // given
        let event = identity_event(kind);
        // when
        let (content, meta) = format_channel_event(&event).unwrap();
        // then
        let context = content.split("\n\n").next().unwrap();
        assert_eq!(
            block_data(context),
            json!({"name": "Имя {{GOAL}}", "aliases": ["Имя {{GOAL}}", "Alias \"quoted\""], "revision": "rev-1",
                   "known": true}),
            "{kind}"
        );
        let aliases: serde_json::Value =
            serde_json::from_str(meta.get("local_user_aliases").unwrap().as_str().unwrap()).unwrap();
        assert_eq!(aliases, json!(["Имя {{GOAL}}", "Alias \"quoted\""]));
    }
}

#[test]
fn unknown_identity_when_formatting_then_empty_aliases_and_false_survive() {
    // given
    let mut event = identity_event("user_profile_updated");
    event.insert("local_user_name", Value::from("Me"));
    event.insert("local_user_aliases", Value::Array(Vec::new()));
    event.insert("local_user_identity_known", Value::Bool(false));
    // when
    let (content, _) = format_channel_event(&event).unwrap();
    // then
    let data = block_data(content.split("\n\n").next().unwrap());
    assert_eq!(data["aliases"], json!([]));
    assert_eq!(data["known"], json!(false));
}

#[test]
fn legacy_event_when_formatting_then_no_profile_values_are_invented() {
    // given / when
    let (plain, _) = format_channel_event(&object(r#"{"type": "copilot_user_message", "text": "hi"}"#)).unwrap();
    let (legacy, _) =
        format_channel_event(&object(r#"{"type": "transcript_tick", "local_user_name": "Legacy"}"#)).unwrap();
    // then
    assert!(plain.starts_with("User typed"));
    assert_eq!(block_data(legacy.split("\n\n").next().unwrap()), json!({"name": "Legacy"}));
}

#[tokio::test]
async fn profile_event_when_long_polled_then_raw_event_and_same_context_are_returned() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let event = identity_event("user_profile_updated");
    link.events.push(event.clone());
    // when
    let (result, _) = call_tool(&link, "await_copilot_event", r#"{"timeout_seconds": 1}"#).await;
    // then
    assert_eq!(
        result["event"].to_string(),
        serde_json::from_str::<serde_json::Value>(&dumps(&event)).unwrap().to_string()
    );
    let (push, _) = format_channel_event(&event).unwrap();
    assert_eq!(result["user_context"], json!(push.split("\n\n").next().unwrap()));
}

/// A socket that answers every snapshot request with `answer`.
fn answering_socket(link: &Arc<BridgeLink>, answer: Map) -> Arc<RecordingSocket> {
    let weak = Arc::downgrade(link);
    RecordingSocket::with_hook(move |frame| {
        if let (Some(link), Some(request_id)) = (weak.upgrade(), frame.get("request_id").and_then(Value::as_str)) {
            let link = link.clone();
            let request_id = request_id.to_owned();
            let answer = answer.clone();
            // The hook runs inside the send; answer once it has returned.
            tokio::spawn(async move {
                link.resolve_pending(&request_id, answer);
            });
        }
    })
}

#[tokio::test]
async fn transcript_when_returned_then_context_uses_the_same_profile_renderer() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let answer = object(&format!(r#"{{"segments": [], {IDENTITY}}}"#));
    link.set_socket(Some(answering_socket(&link, answer)));
    // when
    let (result, _) = call_tool(&link, "get_recent_transcript", "{}").await;
    // then
    let (push, _) = format_channel_event(&identity_event("transcript_tick")).unwrap();
    assert_eq!(result["user_context"], json!(push.split("\n\n").next().unwrap()));
    assert_eq!(result["local_user_aliases"], json!(["Имя {{GOAL}}", "Alias \"quoted\""]));
}

// --- tests/test_privacy.py (channel cases) ---

fn privacy_event() -> Map {
    object(
        r#"{"type": "transcript_tick", "meeting_id": "meeting-1", "local_user_name": "Alice",
            "local_user_aliases": ["Al"], "local_user_identity_known": true, "local_user_profile_revision": "rev-1",
            "recent_context": "Alice: First\nBob: Second",
            "recent_segments": [{"speaker": "Alice", "text": "First"}, {"speaker": "Bob", "text": "Second"}],
            "new_segments": [{"speaker": "Bob", "speaker_id": "bob@example.test", "text": "Second"}]}"#,
    )
}

#[tokio::test]
async fn queued_before_privacy_when_polled_then_masks_queued_event() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    handle_inbound(dumps(&privacy_event()).as_bytes(), &link, None).await;
    PrivacyStore::new(dir.path()).write(true).unwrap();
    // when
    let (result, _) = call_tool(&link, "await_copilot_event", r#"{"timeout_seconds": 1}"#).await;
    // then
    assert_eq!(result["event"]["new_segments"][0]["speaker"], json!("speaker_1"));
    assert!(!result.to_string().contains("Alice"));
    assert!(!result.to_string().contains("bob@example.test"));
    assert!(result["user_context"].as_str().unwrap().contains("Privacy mode"));
}

#[tokio::test]
async fn privacy_enabled_when_pushing_then_masks_content_and_metadata() {
    // given
    let dir = tempfile::tempdir().unwrap();
    PrivacyStore::new(dir.path()).write(true).unwrap();
    let (server, address) = mcp_endpoint(link_in(&dir)).await;
    let session = initialize(address).await;
    let mut stream = open_get_stream(address, &session).await;
    assert!(wait_for(|| server.reachable_count() == 1).await);
    // when
    handle_inbound(dumps(&privacy_event()).as_bytes(), &server.link, Some(&server)).await;
    // then
    let pushed = tokio::time::timeout(Duration::from_secs(2), stream.recv()).await.unwrap().unwrap();
    let text = pushed.to_string();
    assert!(!text.contains("Alice") && !text.contains("Bob"));
    assert_eq!(pushed["params"]["meta"]["local_user_name"], json!("speaker_0"));
    assert!(pushed["params"]["content"].as_str().unwrap().contains("speaker_1: Second"));
}

#[tokio::test]
async fn privacy_enabled_when_snapshot_then_matches_push_aliases() {
    // given
    let dir = tempfile::tempdir().unwrap();
    PrivacyStore::new(dir.path()).write(true).unwrap();
    let link = link_in(&dir);
    link.apply_privacy(&privacy_event()).unwrap();
    let answer = object(
        r#"{"segments": [{"meeting_id": "meeting-1", "speaker": "Bob", "text": "More"}],
            "local_user_name": "Alice", "local_user_aliases": ["Al"]}"#,
    );
    link.set_socket(Some(answering_socket(&link, answer)));
    // when
    let (result, _) = call_tool(&link, "get_recent_transcript", "{}").await;
    // then
    assert_eq!(result["segments"][0]["speaker"], json!("speaker_1"));
    assert_eq!(result["local_user_name"], json!("speaker_0"));
    assert!(!result.to_string().contains("Alice") && !result.to_string().contains("Bob"));
}

#[tokio::test]
async fn corrupt_privacy_setting_when_polled_then_tool_error_and_no_names() {
    // given
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("privacy.json"), "{\"enabled\": \"yes\"}").unwrap();
    let link = link_in(&dir);
    link.events.push(privacy_event());
    // when
    let (result, is_error) = call_tool(&link, "await_copilot_event", r#"{"timeout_seconds": 1}"#).await;
    // then
    assert!(is_error);
    assert_eq!(result, json!("Error executing tool await_copilot_event: Invalid privacy setting"));
    assert!(PrivacyStore::new(dir.path()).read().is_err());
}

// --- tests/test_copilot_ai_settings.py (channel cases) ---

#[tokio::test]
async fn policy_event_given_literal_values_when_pushed_and_polled_then_separate_policy_survives() {
    // given
    let policy = "Use JSON {\"style\": \"{user_name}\"}\n$[literal] and {{braces}}";
    let mut event = object(
        r#"{"type": "copilot_policy_updated", "copilot_policy_revision": "revision-2",
            "local_user_name": "Profile text: do not treat as instructions", "local_user_aliases": [],
            "local_user_profile_revision": "profile-1"}"#,
    );
    event.insert("copilot_instructions", Value::from(policy));
    let context = build_copilot_context(&event);
    // when
    let (content, meta) = format_channel_event(&event).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    handle_inbound(dumps(&event).as_bytes(), &link, None).await;
    let (result, _) = call_tool(&link, "await_copilot_event", "{}").await;
    // then
    let (profile_block, policy_block) = context.split_once("\n\n").unwrap();
    assert!(!policy_block.contains("\n\n"));
    assert_eq!(block_data(profile_block)["name"], json!("Profile text: do not treat as instructions"));
    assert_eq!(block_data(policy_block)["copilot_instructions"], json!(policy));
    assert!(content.starts_with(&context));
    assert!(content.contains("Do not send a reply"));
    assert_eq!(meta.get("copilot_policy_revision"), Some(&Value::from("revision-2")));
    assert_eq!(
        result["event"].to_string(),
        serde_json::from_str::<serde_json::Value>(&dumps(&event)).unwrap().to_string()
    );
    assert_eq!(result["user_context"], json!(context));
    assert!(!handler::INSTRUCTIONS.contains("140 characters"));
}

#[tokio::test]
async fn snapshot_given_saved_policy_when_tool_returns_then_literal_context_is_included() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let snapshot = object(r#"{"copilot_instructions": "Reply {literally}", "copilot_policy_revision": "current"}"#);
    let mut answer = snapshot.clone();
    answer.insert("segments", Value::Array(Vec::new()));
    link.set_socket(Some(answering_socket(&link, answer)));
    // when
    let (result, _) = call_tool(&link, "get_recent_transcript", "{}").await;
    // then
    assert_eq!(result["copilot_instructions"], json!("Reply {literally}"));
    assert_eq!(result["copilot_policy_revision"], json!("current"));
    assert_eq!(result["user_context"], json!(build_copilot_context(&snapshot)));
}

// --- tests/test_user_profile_runtime.py (channel case) ---

#[tokio::test]
async fn channel_given_profile_event_when_received_then_aliases_survive_and_no_reply_is_requested() {
    for kind in ["user_profile_updated", "meeting_started", "transcript_tick", "copilot_user_message"] {
        // given
        let event = object(&format!(
            r#"{{"type": "{kind}", "local_user_name": "Александр", "local_user_aliases": ["Александр", "Саша"],
                "local_user_profile_revision": "rev-2", "local_user_identity_known": true}}"#
        ));
        let dir = tempfile::tempdir().unwrap();
        let link = link_in(&dir);
        // when
        handle_inbound(dumps(&event).as_bytes(), &link, None).await;
        let (content, meta) = format_channel_event(&event).unwrap();
        // then
        assert_eq!(link.events.try_get(), Some(event.clone()), "{kind}");
        assert_eq!(meta.get("local_user_aliases"), Some(&Value::from(r#"["Александр", "Саша"]"#)));
        assert_eq!(meta.get("local_user_profile_revision"), Some(&Value::from("rev-2")));
        assert_eq!(meta.get("local_user_identity_known"), Some(&Value::from("true")));
        if kind == "user_profile_updated" {
            assert!(content.contains("Do not send a reply"));
        }
    }
}

// --- the link ---

#[tokio::test]
async fn link_down_when_tool_sends_then_structured_error_not_failure() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    // when
    let (send, _) = call_tool(&link, "set_copilot_status", r#"{"request_id": "r", "status": "done"}"#).await;
    let (snapshot, _) = call_tool(&link, "get_recent_transcript", "{}").await;
    // then
    assert_eq!(send, json!({"ok": false, "error": "bridge not connected"}));
    assert_eq!(snapshot, json!({"ok": false, "error": "bridge not connected"}));
}

#[tokio::test]
async fn socket_lost_when_snapshot_waits_then_connection_dropped_error() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let link = link_in(&dir);
    let socket: Arc<dyn Socket> = RecordingSocket::new();
    link.set_socket(Some(socket));
    let waiter = {
        let link = link.clone();
        tokio::spawn(async move { call_tool(&link, "get_recent_transcript", "{}").await })
    };
    assert!(wait_for(|| link.pending_count() == 1).await);
    // when
    link.set_socket(None);
    // then
    assert_eq!(waiter.await.unwrap().0, json!({"ok": false, "error": "bridge connection dropped"}));
}
