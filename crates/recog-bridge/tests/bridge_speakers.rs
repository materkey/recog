//! `speaker_update` as the extension sends it and the queries the live process asks
//! afterwards: segments completed on a change, on silence, past 20 s (by a frame and
//! by the server's periodic check), the user's own turns, the `seq` ordering and the
//! frames Python refuses (the steps of the former differential scenario
//! `speakers.json`, with the values Python answered).

mod common;

use std::sync::Arc;

use common::{Harness, NOW, field};
use recog_bridge::hooks::SpeakerSegment;
use recog_bridge::testing::RecordingLive;
use recog_core::pyjson::Value;

fn rig() -> (Harness, Arc<RecordingLive>) {
    let h = Harness::new();
    let live = RecordingLive::new();
    h.bridge.set_live_events(Some(live.clone()));
    (h, live)
}

/// A `speaker_update` of `speaker` (JSON) stamped `at` (bridge seconds).
fn update(seq: &str, at: f64, speaker: &str) -> String {
    format!(r#"{{"type": "speaker_update", "seq": {seq}, "timestamp": {}, "speaker": {speaker}}}"#, at * 1000.0)
}

const ALICE: &str = r#"{"id": "a", "name": "Alice", "isLocal": false}"#;
const BOB: &str = r#"{"id": "b", "name": "Bob", "isLocal": false}"#;
const ME: &str = r#"{"id": "me", "name": "Me", "isLocal": true}"#;

fn segment(name: &str, id: &str, start: f64, end: f64, duration_ms: i64) -> SpeakerSegment {
    SpeakerSegment {
        speaker_name: name.into(),
        speaker_id: id.into(),
        start_time: start,
        end_time: end,
        duration_ms,
        is_local: false,
    }
}

/// The segments handed to the live side's `segment_completed`, in order.
fn completed_calls(live: &RecordingLive) -> Vec<Value> {
    live.calls()
        .into_iter()
        .filter(|c| field(c, "call").as_str() == Some("segment_completed"))
        .map(|c| field(&c, "segment").clone())
        .collect()
}

#[tokio::test]
async fn given_alice_speaking_when_bob_takes_over_then_alices_segment_is_completed() {
    // given
    let (h, live) = rig();
    h.process(&update("1", NOW, ALICE), None).await.unwrap();
    h.clock.advance(2.0);
    // when
    h.process(&update("2", NOW + 2.0, BOB), None).await.unwrap();
    // then
    let calls = completed_calls(&live);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(field(&calls[0], "speaker_name").as_str(), Some("Alice"));
    assert_eq!(field(&calls[0], "duration_ms"), &Value::Int(2000));
    assert_eq!(h.bridge.get_all_completed_segments(), vec![segment("Alice", "a", NOW, NOW + 2.0, 2000)]);
    assert_eq!(h.bridge.get_current_speaker().as_deref(), Some("Bob"));
    assert!(h.logged("[KTalk Bridge] First speaker received: Alice"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_alice_speaking_when_she_falls_silent_then_segment_completed_and_remembered_0_8_s_from_her_last_frame() {
    // given
    let (h, live) = rig();
    h.process(&update("1", NOW, ALICE), None).await.unwrap();
    h.clock.advance(0.5);
    // when
    h.process(&update("2", NOW + 0.5, "null"), None).await.unwrap();
    // then
    assert_eq!(h.bridge.get_all_completed_segments(), vec![segment("Alice", "a", NOW, NOW + 0.5, 500)]);
    assert_eq!(completed_calls(&live).len(), 1);
    assert!(!h.bridge.is_someone_speaking());
    assert_eq!(h.bridge.get_current_speaker().as_deref(), Some("Alice"));
    assert!(!h.bridge.is_current_speaker_local());
    h.clock.advance(0.25);
    assert_eq!(h.bridge.get_current_speaker().as_deref(), Some("Alice"));
    h.clock.advance(0.25);
    assert_eq!(h.bridge.get_current_speaker(), None);
    // and a silence that follows a silence completes nothing more
    h.process(&update("3", NOW + 3.0, "null"), None).await.unwrap();
    assert!(h.bridge.get_all_completed_segments().is_empty());
}

#[tokio::test]
async fn given_turns_of_100_ms_or_less_when_they_end_then_no_segment_is_completed() {
    // given
    let (h, live) = rig();
    h.process(&update("1", NOW, ALICE), None).await.unwrap();
    // when: Bob 0 ms later, then silence 0 ms after Bob
    h.process(&update("2", NOW, BOB), None).await.unwrap();
    h.process(&update("3", NOW, "null"), None).await.unwrap();
    // then
    assert!(h.bridge.get_all_completed_segments().is_empty());
    assert!(completed_calls(&live).is_empty());
    // and the next turn starts its own segment
    h.process(&update("4", NOW + 1.0, BOB), None).await.unwrap();
    h.process(&update("5", NOW + 2.0, ALICE), None).await.unwrap();
    assert_eq!(h.bridge.get_all_completed_segments(), vec![segment("Bob", "b", NOW + 1.0, NOW + 2.0, 1000)]);
}

#[tokio::test]
async fn given_one_speaker_for_20_s_when_she_is_reported_again_then_segment_cut_and_restarted() {
    // given
    let (h, _live) = rig();
    h.process(&update("1", NOW, ALICE), None).await.unwrap();
    h.clock.advance(20.0);
    // when
    h.process(&update("2", NOW + 20.0, ALICE), None).await.unwrap();
    h.clock.advance(5.0);
    h.process(&update("3", NOW + 25.0, BOB), None).await.unwrap();
    // then
    assert_eq!(
        h.bridge.get_all_completed_segments(),
        vec![segment("Alice", "a", NOW, NOW + 20.0, 20000), segment("Alice", "a", NOW + 20.0, NOW + 25.0, 5000)]
    );
}

#[tokio::test]
async fn given_long_turn_without_frames_when_periodic_check_runs_then_segment_cut_at_now() {
    // given
    let (h, live) = rig();
    h.attach();
    h.process(&update("1", NOW, BOB), None).await.unwrap();
    h.clock.advance(19.5);
    // when: before 20 s
    h.bridge.periodic().await;
    // then
    assert!(completed_calls(&live).is_empty());
    // when: past 20 s
    h.clock.advance(1.5);
    h.bridge.periodic().await;
    // then
    let calls = completed_calls(&live);
    assert_eq!(calls.len(), 1);
    assert_eq!(field(&calls[0], "duration_ms"), &Value::Int(21000));
    assert_eq!(h.bridge.get_all_completed_segments(), vec![segment("Bob", "b", NOW, NOW + 21.0, 21000)]);
    // and the next segment starts at that check
    h.clock.advance(1.0);
    h.process(&update("2", NOW + 22.0, ALICE), None).await.unwrap();
    assert_eq!(h.bridge.get_all_completed_segments(), vec![segment("Bob", "b", NOW + 21.0, NOW + 22.0, 1000)]);
}

#[tokio::test]
async fn given_no_named_segment_when_periodic_check_runs_then_nothing_is_cut() {
    // given: nobody spoke yet
    let (h, live) = rig();
    h.attach();
    h.clock.advance(30.0);
    // when
    h.bridge.periodic().await;
    // then
    assert!(completed_calls(&live).is_empty());
    // given: a speaker without a name
    h.process(&update("1", NOW + 30.0, r#"{"id": "7", "name": null}"#), None).await.unwrap();
    h.clock.advance(30.0);
    // when
    h.bridge.periodic().await;
    // then
    assert!(completed_calls(&live).is_empty());
    assert!(h.bridge.get_all_completed_segments().is_empty());
}

#[tokio::test]
async fn given_user_speaking_when_asked_then_local_and_no_system_speaker() {
    // given
    let (h, _live) = rig();
    assert!(!h.bridge.is_current_speaker_local());
    // when
    h.process(&update("1", NOW, ME), None).await.unwrap();
    // then
    assert!(h.bridge.is_current_speaker_local());
    assert_eq!(h.bridge.get_current_speaker(), None);
    // when: the user falls silent
    h.clock.advance(0.5);
    h.process(&update("2", NOW + 0.5, "null"), None).await.unwrap();
    // then: still the user within the memory, nobody after it
    assert!(h.bridge.is_current_speaker_local());
    h.clock.advance(0.5);
    assert!(!h.bridge.is_current_speaker_local());
}

#[tokio::test]
async fn given_remote_speaker_when_asked_then_not_local() {
    // given
    let (h, _live) = rig();
    // when
    h.process(&update("1", NOW, ALICE), None).await.unwrap();
    // then
    assert!(!h.bridge.is_current_speaker_local());
}

#[tokio::test]
async fn given_seq_1_seen_when_seq_1_arrives_again_then_logged_out_of_order_and_still_applied() {
    // given
    let (h, _live) = rig();
    h.process(&update("1", NOW, ALICE), None).await.unwrap();
    // when
    h.process(&update("1", NOW + 1.0, BOB), None).await.unwrap();
    // then
    assert!(h.logged("[KTalk Bridge] Out-of-order speaker_update: seq=1, last=1"), "{:?}", h.logs());
    assert_eq!(h.bridge.last_seq(), Value::Int(1));
    assert_eq!(h.bridge.get_current_speaker().as_deref(), Some("Bob"));
    // when: a float and a bool seq
    h.process(&update("2.0", NOW + 2.0, ALICE), None).await.unwrap();
    h.process(&update("true", NOW + 3.0, BOB), None).await.unwrap();
    // then
    assert_eq!(h.bridge.last_seq(), Value::Float(2.0));
    assert!(h.logged("[KTalk Bridge] Out-of-order speaker_update: seq=True, last=2.0"), "{:?}", h.logs());
}

#[tokio::test]
async fn given_seq_that_is_not_a_number_when_update_arrives_then_type_error_and_nothing_changes() {
    // given
    let (h, _live) = rig();
    h.process(&update("1", NOW, ALICE), None).await.unwrap();
    // when
    let result = h.process(&update(r#""nine""#, NOW + 1.0, "null"), None).await;
    // then
    assert_eq!(result, Err("'<=' not supported between instances of 'str' and 'int'".to_owned()));
    assert!(h.bridge.is_someone_speaking());
    assert_eq!(h.bridge.last_seq(), Value::Int(1));
}

#[tokio::test]
async fn given_null_seq_or_none_when_update_arrives_then_order_not_checked() {
    // given
    let (h, _live) = rig();
    h.process(&update("5", NOW, ALICE), None).await.unwrap();
    // when
    h.process(&update("null", NOW + 1.0, BOB), None).await.unwrap();
    h.process(r#"{"type": "speaker_update", "speaker": null}"#, None).await.unwrap();
    // then
    assert_eq!(h.bridge.last_seq(), Value::Int(5));
    assert!(!h.logged("Out-of-order"), "{:?}", h.logs());
    assert!(!h.bridge.is_someone_speaking());
}

#[tokio::test]
async fn given_clock_offset_when_update_has_no_timestamp_then_stamped_now_without_the_offset() {
    // given
    let (h, _live) = rig();
    h.bridge.set_clock_offset(2.0);
    h.process(&update("1", NOW - 2.0, ALICE), None).await.unwrap();
    h.clock.advance(3.0);
    // when: no timestamp
    h.process(r#"{"type": "speaker_update", "seq": 2, "speaker": null}"#, None).await.unwrap();
    // then: Alice's segment ran from her stamp, moved onto the bridge's clock by the
    // offset, to the bridge's own time, which is on that clock already
    assert_eq!(h.bridge.get_all_completed_segments(), vec![segment("Alice", "a", NOW, NOW + 3.0, 3000)]);
}

#[tokio::test]
async fn given_malformed_frames_when_they_arrive_then_python_errors() {
    // given
    let (h, _live) = rig();
    // when / then
    let late = h.process(r#"{"type": "speaker_update", "seq": 1, "timestamp": "late", "speaker": null}"#, None).await;
    assert_eq!(late, Err("unsupported operand type(s) for /: 'str' and 'int'".to_owned()));
    let named = h.process(&update("2", NOW, r#""Bob""#), None).await;
    assert_eq!(named, Err("'str' object has no attribute 'get'".to_owned()));
    assert!(!h.bridge.is_someone_speaking());
}

#[tokio::test]
async fn given_late_change_with_since_when_outgoing_repeats_follow_the_onset_then_they_are_dropped() {
    // given: Alice reported at 0, 1 and 2 s
    let (h, _live) = rig();
    for (seq, at) in [(1, 0.0), (2, 1.0), (3, 2.0)] {
        h.clock.set(NOW + at);
        h.process(&update(&seq.to_string(), NOW + at, ALICE), None).await.unwrap();
    }
    h.clock.set(NOW + 2.5);
    // when: Bob, reported at 2.5 s, began at 1.25 s
    let bob = format!(r#"{{"id": "b", "name": "Bob", "isLocal": false, "since": {}}}"#, (NOW + 1.25) * 1000.0);
    h.process(&update("4", NOW + 2.5, &bob), None).await.unwrap();
    // then: the repeat at 2 s goes, the one at 1 s stays, Bob starts at his onset
    let history: Vec<(Option<String>, f64)> =
        h.bridge.get_speaker_history().into_iter().map(|e| (e.speaker_name, e.timestamp - NOW)).collect();
    assert_eq!(history, vec![(Some("Alice".into()), 0.0), (Some("Alice".into()), 1.0), (Some("Bob".into()), 1.25)]);
    assert_eq!(h.bridge.get_dominant_speaker_for_period(NOW, NOW + 3.0).as_deref(), Some("Bob"));
}
