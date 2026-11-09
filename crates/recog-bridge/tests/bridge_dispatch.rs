//! `_process_message`'s dispatch: each frame type reaches its own handler (seen by
//! the line or the storage call only that handler makes), heartbeats and unknown
//! types do nothing, and the Copilot frames stop at the settings switch.

mod common;

use common::Harness;
use recog_bridge::Role;
use recog_bridge::testing::FakeStore;

#[tokio::test]
async fn given_each_frame_type_when_processed_then_its_own_handler_answers() {
    let cases = [
        (r#"{"type": "copilot_chat_message"}"#, "copilot_chat_message missing/invalid request_id — ignored"),
        (r#"{"type": "copilot_channel_reply"}"#, "copilot_channel_reply missing/invalid request_id — ignored"),
        (r#"{"type": "copilot_channel_status"}"#, "copilot_channel_status missing/invalid request_id — ignored"),
        (
            r#"{"type": "get_recent_transcript_request"}"#,
            "get_recent_transcript_request missing/invalid request_id — ignored",
        ),
        (r#"{"type": "copilot_spawn_request"}"#, "copilot_spawn_request missing/invalid request_id — ignored"),
        (r#"{"type": "copilot_stop_request"}"#, "copilot_stop_request missing/invalid request_id — ignored"),
        (r#"{"type": "copilot_agent_status"}"#, "copilot_agent_status missing/invalid status — ignored"),
        (r#"{"type": "claude_sessions", "count": "two"}"#, "claude_sessions invalid count — ignored"),
        (r#"{"type": "copilot_channel_hello"}"#, "copilot_channel_hello without websocket — ignored"),
        (r#"{"type": "supervisor_hello"}"#, "supervisor_hello without websocket — ignored"),
        (r#"{"type": "pcm_transport_stats", "stats": []}"#, "Malformed pcm_transport_stats frame, skipped"),
        (r#"{"type": "connection_status", "connected": true}"#, "Conference connected"),
        (r#"{"type": "connection_status", "connected": false}"#, "Conference disconnected"),
        (r#"{"type": "clock_sync", "browser_time": 1783159199000}"#, "Clock sync: offset=1.000s"),
        (r#"{"type": "meeting_title_update", "title": "Retro"}"#, "Updated meeting title: Retro"),
        (r#"{"type": "meeting_join", "url": "https://test.ktalk.ru/room"}"#, "Joined ktalk meeting"),
        (r#"{"type": "meeting_leave"}"#, "Left meeting (reason: user_left)"),
        (r#"{"type": "pcm_chunk"}"#, "Malformed pcm_chunk frame, skipped: 'pid'"),
        (r#"{"type": "speaker_update", "speaker": {"name": "Alice"}}"#, "First speaker received: Alice"),
    ];
    for (frame, line) in cases {
        // given
        let h = Harness::new();
        h.bridge.set_meeting_storage(FakeStore::with_source("orphan"));
        // when
        let result = h.process(frame, None).await;
        // then
        assert_eq!(result, Ok(()), "{frame}");
        assert!(h.logged(line), "{frame}: {:?}", h.logs());
    }
}

#[tokio::test]
async fn given_frames_with_observable_state_when_processed_then_their_handler_changed_it() {
    // given
    let h = Harness::new();
    let (sink, client) = h.add(Role::Extension);
    // when
    h.process(r#"{"type": "request_transcript"}"#, Some(&client)).await.unwrap();
    h.process(r#"{"type": "participants_update", "participants": [{"id": "a", "name": "Alice"}]}"#, None)
        .await
        .unwrap();
    // then: no storage, so no history; the participant is kept
    assert!(sink.frames().is_empty());
    assert_eq!(h.bridge.get_participants().len(), 1);
    // given: a storage
    h.bridge.set_meeting_storage(FakeStore::with_source("ktalk"));
    // when
    h.process(r#"{"type": "request_transcript"}"#, Some(&client)).await.unwrap();
    // then
    assert_eq!(sink.of_type("transcript_history").len(), 1);
}

#[tokio::test]
async fn given_heartbeat_or_unknown_type_when_processed_then_nothing_happens() {
    for frame in [r#"{"type": "ping"}"#, r#"{"type": "no_such_type"}"#, r#"{"text": "no type"}"#] {
        // given
        let h = Harness::new();
        let (sink, client) = h.add(Role::Extension);
        // when
        let result = h.process(frame, Some(&client)).await;
        // then
        assert_eq!(result, Ok(()), "{frame}");
        assert!(sink.frames().is_empty(), "{frame}");
        assert!(h.logs().is_empty(), "{frame}: {:?}", h.logs());
    }
}

#[tokio::test]
async fn given_copilot_switched_off_when_frames_without_request_id_or_channel_status_arrive_then_dropped_silently() {
    // given
    let h = Harness::new();
    let (subscriber, sub) = h.add(Role::Subscriber);
    let (channel, channel_client) = h.add(Role::Channel);
    h.policy.set_enabled(false);
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    subscriber.clear();
    channel.clear();
    // when
    for frame in [
        r#"{"type": "copilot_spawn_request"}"#,
        r#"{"type": "copilot_chat_message", "request_id": "", "text": "Hi"}"#,
        r#"{"type": "copilot_channel_status", "request_id": "r1", "status": "thinking"}"#,
    ] {
        h.process(frame, Some(&sub)).await.unwrap();
    }
    h.process(r#"{"type": "copilot_channel_status", "request_id": "r1", "status": "done"}"#, Some(&channel_client))
        .await
        .unwrap();
    // then
    assert!(subscriber.frames().is_empty(), "{:?}", subscriber.frames());
    assert!(channel.frames().is_empty());
    assert!(!h.logged("missing/invalid"), "{:?}", h.logs());
    // when: switched on again, the same status is relayed
    h.policy.set_enabled(true);
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    h.process(r#"{"type": "copilot_channel_status", "request_id": "r1", "status": "done"}"#, Some(&channel_client))
        .await
        .unwrap();
    // then
    assert_eq!(subscriber.of_type("copilot_status").len(), 1, "{:?}", subscriber.frames());
}
