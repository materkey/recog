//! `meeting_join` paths the other suites leave out (the steps of the former
//! differential scenarios `meeting_lifecycle.json`, `second_tab.json` and
//! `ktalk_only.json`): a join without a url, a url that is not a string, a second
//! tab refused while the owner's is connected, a meta.json that is not an object, the
//! live side's `meeting_changed` failing, and the sends of a refusal failing; and an
//! extension gone from a meeting the storage has no id for.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{Harness, field};
use recog_bridge::testing::{FakeStore, RecordingCapture, RecordingLive, RecordingSink, SendMode};
use recog_bridge::{Client, Role};
use recog_core::pyjson::Value;

fn rig(source: &str) -> (Harness, Arc<FakeStore>, Arc<RecordingLive>) {
    let h = Harness::new();
    let store = FakeStore::with_source(source);
    h.bridge.set_meeting_storage(store.clone());
    let live = RecordingLive::new();
    h.bridge.set_live_events(Some(live.clone()));
    (h, store, live)
}

fn ktalk_only_rig() -> (Harness, Arc<FakeStore>, Arc<RecordingLive>, Arc<RecordingCapture>) {
    let h = Harness::with(|c| c.ktalk_only = true);
    let store = FakeStore::with_source("orphan");
    h.bridge.set_meeting_storage(store.clone());
    let live = RecordingLive::new();
    h.bridge.set_live_events(Some(live.clone()));
    let capture = RecordingCapture::new();
    h.bridge.set_meeting_capture(capture.clone());
    (h, store, live, capture)
}

const JOIN: &str =
    r#"{"type": "meeting_join", "url": "https://test.ktalk.ru/room-1", "title": "Weekly", "platform": "ktalk"}"#;

fn failing_client(h: &Harness) -> Arc<Client> {
    h.bridge.add_client(
        Role::Extension,
        RecordingSink::with_mode(SendMode::Fail("socket gone".into())),
        "('127.0.0.1', 1)",
    )
}

fn sent(sink: &RecordingSink) -> Vec<String> {
    sink.sent.lock().unwrap().clone()
}

#[tokio::test]
async fn given_join_without_url_when_processed_then_storage_and_tabs_untouched() {
    for frame in [r#"{"type": "meeting_join", "title": "T"}"#, r#"{"type": "meeting_join", "url": ""}"#] {
        // given
        let (h, store, live) = rig("orphan");
        let (sink, client) = h.add(Role::Extension);
        // when
        let result = h.process(frame, Some(&client)).await;
        // then
        assert_eq!(result, Ok(()), "{frame}");
        assert!(store.ops().is_empty(), "{frame}");
        assert!(live.calls().is_empty(), "{frame}");
        assert!(sink.frames().is_empty(), "{frame}");
    }
}

#[tokio::test]
async fn given_url_that_is_not_a_string_when_joined_then_python_error_before_the_storage() {
    // given
    let (h, store, live) = rig("orphan");
    // when
    let result = h.process(r#"{"type": "meeting_join", "url": 5}"#, None).await;
    // then
    assert_eq!(result, Err("'int' object has no attribute 'decode'".to_owned()));
    assert!(store.ops().is_empty());
    assert!(live.calls().is_empty());
}

#[tokio::test]
async fn given_owner_tab_connected_when_second_tab_joins_then_refused_with_active_meeting() {
    // given
    let (h, store, _live) = rig("orphan");
    let (_owner_sink, owner) = h.add(Role::Extension);
    h.process(JOIN, Some(&owner)).await.unwrap();
    let (second_sink, second) = h.add(Role::Extension);
    // when
    let other =
        r#"{"type": "meeting_join", "url": "https://test.ktalk.ru/room-2", "title": "Other", "platform": "ktalk"}"#;
    h.process(other, Some(&second)).await.unwrap();
    // then
    assert_eq!(
        sent(&second_sink),
        [r#"{"type": "meeting_join_rejected", "reason": "another_tab_active", "active_meeting_id": "m1"}"#]
    );
    assert!(h.logged(
        "[KTalk Bridge] Rejecting meeting_join from second tab (url=https://test.ktalk.ru/room-2); owner=('127.0.0.1', 12345) still active"
    ));
    assert_eq!(store.ops(), ["on_meeting_join"]);
    assert_eq!(h.bridge.meeting_owner(), Some(owner.id));
    // when: a join from no socket while the owner is there
    h.process(other, None).await.unwrap();
    // then
    assert_eq!(store.ops(), ["on_meeting_join"]);
    // when: the owner's own rejoin
    h.process(JOIN, Some(&owner)).await.unwrap();
    // then
    assert_eq!(store.ops(), ["on_meeting_join", "on_meeting_join"]);
}

#[tokio::test]
async fn given_second_tab_refused_when_its_socket_is_dead_then_refusal_send_error_is_swallowed() {
    // given
    let (h, store, _live) = rig("orphan");
    let (_owner_sink, owner) = h.add(Role::Extension);
    h.process(JOIN, Some(&owner)).await.unwrap();
    let second = failing_client(&h);
    // when
    let result = h.process(JOIN, Some(&second)).await;
    // then
    assert_eq!(result, Ok(()));
    assert_eq!(store.ops(), ["on_meeting_join"]);
}

#[tokio::test]
async fn given_owner_tab_gone_when_another_tab_joins_then_it_becomes_owner() {
    // given
    let (h, store, _live) = rig("orphan");
    let (_owner_sink, owner) = h.add(Role::Extension);
    h.process(JOIN, Some(&owner)).await.unwrap();
    h.bridge.remove_client(owner.id);
    let (sink, next) = h.add(Role::Extension);
    // when
    h.process(JOIN, Some(&next)).await.unwrap();
    // then
    assert_eq!(store.ops(), ["on_meeting_join", "on_meeting_join"]);
    assert_eq!(h.bridge.meeting_owner(), Some(next.id));
    assert_eq!(sink.of_type("transcript_history").len(), 1);
}

#[tokio::test]
async fn given_meta_that_is_not_an_object_when_same_meeting_joined_then_treated_as_a_reconnect() {
    for meta in [Value::Null, Value::Array(vec![Value::from("orphan")])] {
        // given
        let (h, store, live) = rig("orphan");
        store.set_meta(Ok(meta.clone()));
        let (sink, client) = h.add(Role::Extension);
        // when
        h.process(JOIN, Some(&client)).await.unwrap();
        // then: no new meeting, the tick armed for it, the history sent
        assert!(sink.of_type("meeting_started").is_empty(), "{meta:?}");
        assert_eq!(live.names(), ["before_meeting_switch"], "{meta:?}");
        assert_eq!(h.bridge.tick_state().and_then(|t| t.meeting_id).as_deref(), Some("m1"), "{meta:?}");
        assert_eq!(sink.of_type("transcript_history").len(), 1, "{meta:?}");
    }
}

#[tokio::test]
async fn given_live_side_failing_when_join_moves_to_a_new_meeting_then_error_and_no_meeting_started() {
    // given
    let (h, store, live) = rig("orphan");
    *store.join_result.lock().unwrap() = (Some("m2".into()), Some(PathBuf::from("/meetings/m2")));
    *live.meeting_change_error.lock().unwrap() = Some("writer failed".into());
    let (sink, client) = h.add(Role::Extension);
    // when
    let result = h.process(JOIN, Some(&client)).await;
    // then
    assert_eq!(result, Err("writer failed".to_owned()));
    assert_eq!(live.names(), ["before_meeting_switch", "meeting_changed"]);
    assert!(sink.frames().is_empty());
    assert_eq!(h.bridge.meeting_owner(), None);
}

#[tokio::test]
async fn given_ktalk_only_when_other_platform_joins_on_a_dead_socket_then_send_error_is_raised() {
    // given
    let (h, store, _live, _capture) = ktalk_only_rig();
    let client = failing_client(&h);
    // when
    let result = h
        .process(
            r#"{"type": "meeting_join", "url": "https://meet.google.com/abc-defg-hij", "platform": "gmeet"}"#,
            Some(&client),
        )
        .await;
    // then
    assert_eq!(result, Err("socket gone".to_owned()));
    assert!(store.ops().is_empty());
}

#[tokio::test]
async fn given_ktalk_only_when_live_side_cannot_switch_meeting_then_capture_closed_and_join_refused() {
    // given
    let (h, _store, live, capture) = ktalk_only_rig();
    *live.meeting_change_error.lock().unwrap() = Some("writer failed".into());
    *capture.stop_error.lock().unwrap() = Some("device busy".into());
    let (sink, client) = h.add(Role::Extension);
    // when
    let result = h.process(JOIN, Some(&client)).await;
    // then
    assert_eq!(result, Ok(()));
    assert_eq!(*capture.calls.lock().unwrap(), [false]);
    assert!(h.logged("[KTalk Bridge] Capture cleanup failed: device busy"), "{:?}", h.logs());
    assert!(h.logged("[KTalk Bridge] Capture start failed: writer failed"), "{:?}", h.logs());
    assert_eq!(sent(&sink), [r#"{"type": "meeting_join_rejected", "reason": "capture_unavailable"}"#]);
    assert_eq!(h.bridge.meeting_owner(), None);
}

#[tokio::test]
async fn given_ktalk_only_capture_refused_when_refusal_cannot_be_sent_then_send_error_is_raised() {
    // given
    let (h, _store, _live, capture) = ktalk_only_rig();
    *capture.start_error.lock().unwrap() = Some("no microphone".into());
    let client = failing_client(&h);
    // when
    let result = h.process(JOIN, Some(&client)).await;
    // then
    assert_eq!(result, Err("socket gone".to_owned()));
    assert_eq!(*capture.calls.lock().unwrap(), [true, false]);
}

#[tokio::test]
async fn given_conference_without_meeting_id_when_extension_is_gone_then_left_without_meeting_ended() {
    // given
    let (h, store, live) = rig("ktalk");
    *store.current_id.lock().unwrap() = None;
    let (supervisor, _s) = h.add(Role::Supervisor);
    // when
    h.bridge.end_meeting_on_extension_gone().await.unwrap();
    // then
    assert_eq!(store.ops(), ["on_ktalk_leave"]);
    assert_eq!(live.names(), ["before_meeting_switch", "meeting_changed"]);
    assert!(supervisor.of_type("meeting_ended").is_empty());
    assert!(h.logged("[KTalk Bridge] Ended KTalk meeting due to client disconnect"));
    let changed = live.calls().pop().unwrap();
    assert_eq!(field(&changed, "meeting_id").as_str(), Some("new_meeting_id"));
}
