//! The bridge cases of tests/test_ktalk_only.py: a `ktalk_only` installation records
//! only while a KTalk meeting's owner tab is in it, so the bridge switches the live
//! capture on after an accepted join and off before any leave.
//!
//! The Python tests drive a real `LiveTranscriber` with fake devices; here the live
//! side is a recording double whose capture switch shares one timeline with the
//! meeting hooks, so the order the bridge calls them in is what is asserted. What the
//! live side does with a switch (devices, buffers, WAV writers, the generation) is
//! the subject of the Python tests only.
//!
//! Not ported (subject is `LiveTranscriber`, `PerSpeakerIngest`, `RefineWorker`,
//! `SingleDeviceCapture` or the CLI, not the bridge):
//! test_leave_during_asr_drops_late_transcript, test_no_save_audio_still_gates_capture,
//! test_cli_ktalk_only_is_opt_in, test_leave_discards_pcm_retry_before_asr_recovers,
//! test_late_asr_failure_cannot_repopulate_retry_after_leave[mic|pcm],
//! test_leave_discards_refine_queue_and_rejects_old_generation,
//! test_bridge_stop_updates_persisted_recording_status (the bridge part is the
//! capture switch of the stop case below), test_late_refine_failure_cannot_requeue_after_rejoin,
//! test_device_errors_close_stream_and_allow_restart[*],
//! test_run_finalizes_recording_and_session_on_shutdown[*],
//! test_pcm_drained_before_leave_cannot_run_after_rejoin,
//! test_leave_during_capture_read_cannot_write_into_rejoined_meeting.

mod common;

use std::sync::Arc;

use common::{Harness, field, json};
use recog_bridge::hooks::MeetingCapture;
use recog_bridge::testing::{FakeStore, RecordingCapture, RecordingLive, RecordingSink, frame_type};
use recog_bridge::{Client, Role, obj};
use recog_core::pyjson::{Map, Value};

/// The capture switch, recorded on the live double's timeline.
struct TimelineCapture {
    live: Arc<RecordingLive>,
    inner: Arc<RecordingCapture>,
}

impl MeetingCapture for TimelineCapture {
    fn set_capture(&self, active: bool) -> Result<(), String> {
        self.live.calls.lock().unwrap().push(obj! {"call" => "set_capture", "active" => active});
        self.inner.set_capture(active)
    }
    fn recording_active(&self) -> bool {
        self.inner.recording_active()
    }
}

/// The `transcriber` fixture: a `ktalk_only` bridge with storage, live hooks and the
/// capture switch wired.
struct Rig {
    h: Harness,
    store: Arc<FakeStore>,
    live: Arc<RecordingLive>,
    capture: Arc<RecordingCapture>,
}

fn rig() -> Rig {
    let h = Harness::with(|c| c.ktalk_only = true);
    let store = FakeStore::with_source("orphan");
    h.bridge.set_meeting_storage(store.clone());
    let live = RecordingLive::new();
    h.bridge.set_live_events(Some(live.clone()));
    let capture = RecordingCapture::new();
    h.bridge.set_meeting_capture(Arc::new(TimelineCapture { live: live.clone(), inner: capture.clone() }));
    Rig { h, store, live, capture }
}

impl Rig {
    /// `join(t, ws, platform)`.
    async fn join(&self, client: &Arc<Client>, platform: &str) -> Result<(), String> {
        let frame = format!(
            r#"{{"type": "meeting_join", "platform": "{platform}", "url": "https://example.ktalk.ru/work", "title": "Work"}}"#
        );
        self.h.process(&frame, Some(client)).await
    }

    async fn leave(&self, client: &Arc<Client>) -> Result<(), String> {
        self.h.process(r#"{"type": "meeting_leave"}"#, Some(client)).await
    }

    /// The calls into the live side, in order: `set_capture(true)`,
    /// `meeting_changed(m1)`, `before_meeting_switch`, `pcm_chunk`.
    fn timeline(&self) -> Vec<String> {
        self.live
            .calls()
            .iter()
            .map(|call| {
                let call = call.as_object().unwrap();
                let name = call.get("call").and_then(Value::as_str).unwrap().to_owned();
                match name.as_str() {
                    "set_capture" => format!("set_capture({})", call.get("active").unwrap().py_repr().to_lowercase()),
                    "meeting_changed" => format!("meeting_changed({})", call.get("meeting_id").unwrap().py_str()),
                    _ => name,
                }
            })
            .collect()
    }

    fn capture_calls(&self) -> Vec<bool> {
        self.capture.calls.lock().unwrap().clone()
    }
}

fn last_frame(sink: &RecordingSink) -> Value {
    sink.frames().pop().expect("a frame was sent")
}

fn types(sink: &RecordingSink) -> Vec<String> {
    sink.frames().iter().filter_map(|f| frame_type(f).map(str::to_owned)).collect()
}

const PCM_CHUNK: &str = r#"{"type": "pcm_chunk", "pid": "p1", "speaker_name": "Alice", "seq": 1,
    "chunk_start_ms": 1000, "chunk_end_ms": 1100, "sample_rate": 16000, "pcm_base64": "AAAAAA=="}"#;

/// tests/test_ktalk_only.py::test_startup_does_not_open_devices_or_write_audio
#[tokio::test]
async fn given_ktalk_only_bridge_when_no_meeting_joined_then_capture_is_never_switched_on() {
    // given
    let r = rig();
    let bare = Harness::with(|c| c.ktalk_only = true);
    // when
    let (_sink, _tab) = r.h.add(Role::Extension);
    // then
    assert!(r.capture_calls().is_empty());
    assert!(!r.h.bridge.is_recording_active());
    assert!(!bare.bridge.is_recording_active(), "without a capture owner ktalk_only records nothing");
}

/// tests/test_ktalk_only.py::test_join_records_leave_discards_buffers_and_rejoin_is_fresh
#[tokio::test]
async fn given_ktalk_join_when_left_and_rejoined_then_capture_follows_meeting_and_writers_open_first() {
    // given
    let r = rig();
    let (_sink, ws) = r.h.add(Role::Extension);
    // when
    r.join(&ws, "ktalk").await.unwrap();
    let recording_after_join = r.h.bridge.is_recording_active();
    r.leave(&ws).await.unwrap();
    let recording_after_leave = r.h.bridge.is_recording_active();
    r.join(&ws, "ktalk").await.unwrap();
    // then
    assert!(recording_after_join);
    assert!(!recording_after_leave);
    assert!(r.h.bridge.is_recording_active());
    assert_eq!(
        r.timeline(),
        [
            "before_meeting_switch",
            "meeting_changed(m1)",
            "set_capture(true)",
            "set_capture(false)",
            "before_meeting_switch",
            "meeting_changed(new_meeting_id)",
            "before_meeting_switch",
            "meeting_changed(m1)",
            "set_capture(true)",
        ]
    );
    assert_eq!(r.store.ops(), ["on_meeting_join", "on_ktalk_leave", "on_meeting_join"]);
    assert_eq!(r.h.bridge.meeting_owner(), Some(ws.id));
}

/// tests/test_ktalk_only.py::test_same_room_join_reopens_writers_and_capture
#[tokio::test]
async fn given_recording_meeting_when_same_room_joined_again_then_writers_and_capture_reopen_in_same_meeting() {
    // given
    let r = rig();
    let (sink, ws) = r.h.add(Role::Extension);
    r.join(&ws, "ktalk").await.unwrap();
    let old_path = r.store.current_path.lock().unwrap().clone();
    r.live.calls.lock().unwrap().clear();
    // when
    r.join(&ws, "ktalk").await.unwrap();
    // then
    assert_eq!(*r.store.current_path.lock().unwrap(), old_path);
    assert_eq!(r.timeline(), ["before_meeting_switch", "meeting_changed(m1)", "set_capture(true)"]);
    assert!(r.h.bridge.is_recording_active());
    assert_eq!(sink.of_type("meeting_started").len(), 1, "a same-room reconnect is no new meeting");
}

/// tests/test_ktalk_only.py::test_other_platform_does_not_start_recording[jitsi|vas3k|matrix|gmeet|zoom]
#[tokio::test]
async fn given_ktalk_only_bridge_when_other_platform_joins_then_rejected_and_nothing_records() {
    for platform in ["jitsi", "vas3k", "matrix", "gmeet", "zoom"] {
        // given
        let r = rig();
        let (sink, ws) = r.h.add(Role::Extension);
        // when
        r.join(&ws, platform).await.unwrap();
        // then
        assert_eq!(field(&last_frame(&sink), "reason").as_str(), Some("ktalk_only"), "{platform}");
        assert!(r.store.ops().is_empty(), "{platform}: the orphan meeting stays");
        assert!(r.timeline().is_empty(), "{platform}");
        assert!(!r.h.bridge.is_recording_active(), "{platform}");
        assert_eq!(r.h.bridge.meeting_owner(), None, "{platform}");
    }
}

/// tests/test_ktalk_only.py::test_idle_tab_cannot_stop_owner_meeting
#[tokio::test]
async fn given_owner_recording_when_idle_tab_sends_leave_then_capture_stays_on() {
    // given
    let r = rig();
    let (_sink, owner) = r.h.add(Role::Extension);
    let (_idle_sink, idle) = r.h.add(Role::Extension);
    r.join(&owner, "ktalk").await.unwrap();
    // when
    r.leave(&idle).await.unwrap();
    // then
    assert!(r.h.bridge.is_recording_active());
    assert_eq!(r.capture_calls(), [true]);
    assert_eq!(r.store.ops(), ["on_meeting_join"]);
    assert_eq!(r.h.bridge.meeting_owner(), Some(owner.id));
}

/// tests/test_ktalk_only.py::test_owner_socket_disconnect_stops_capture_with_idle_tab_connected
#[tokio::test]
async fn given_idle_tab_connected_when_owner_socket_disconnects_then_capture_stops_and_meeting_ends() {
    // given
    let r = rig();
    let (_idle_sink, idle) = r.h.add(Role::Extension);
    let (fake, task) = r.h.connect(None).await;
    fake.send(&json(r#"{"type": "meeting_join", "url": "https://example.ktalk.ru/work"}"#));
    fake.idle().await;
    let recording_in_meeting = r.h.bridge.is_recording_active();
    // when
    fake.close();
    task.await.unwrap();
    // then
    assert!(recording_in_meeting);
    assert!(!r.h.bridge.is_recording_active());
    assert_eq!(r.capture_calls(), [true, false]);
    assert_eq!(r.store.ops(), ["on_meeting_join", "on_ktalk_leave"]);
    assert_eq!(field(&r.store.calls()[1], "reason").as_str(), Some("disconnected"));
    assert_eq!(r.h.bridge.clients_in(Role::Extension), [idle.id], "the unrelated idle tab is still open");
}

/// tests/test_ktalk_only.py::test_connection_status_false_stops_capture
#[tokio::test]
async fn given_owner_recording_when_owner_reports_conference_disconnected_then_capture_stops_and_meeting_ends() {
    // given
    let r = rig();
    let (_sink, ws) = r.h.add(Role::Extension);
    r.join(&ws, "ktalk").await.unwrap();
    // when
    r.h.process(r#"{"type": "connection_status", "connected": false}"#, Some(&ws)).await.unwrap();
    // then
    assert!(!r.h.bridge.is_recording_active());
    assert_eq!(r.capture_calls(), [true, false]);
    assert_eq!(r.store.ops(), ["on_meeting_join", "on_ktalk_leave"]);
    assert_eq!(r.h.bridge.meeting_owner(), None);
}

/// tests/test_ktalk_only.py::test_disconnect_stops_even_if_storage_metadata_is_unreadable
#[tokio::test]
async fn given_unreadable_meeting_metadata_when_extension_gone_then_capture_still_stops() {
    // given
    let r = rig();
    let (_sink, ws) = r.h.add(Role::Extension);
    r.join(&ws, "ktalk").await.unwrap();
    r.store.set_meta(Err("disk error".into()));
    // when
    let result = r.h.bridge.end_meeting_on_extension_gone().await;
    // then
    assert_eq!(result, Ok(()));
    assert_eq!(r.capture_calls(), [true, false]);
    assert!(!r.h.bridge.is_recording_active());
    assert_eq!(r.store.ops(), ["on_meeting_join"]);
    assert!(r.h.logged("_load_meta failed during disconnect cleanup: disk error"));
}

/// tests/test_ktalk_only.py::test_partial_capture_start_failure_closes_microphone
#[tokio::test]
async fn given_capture_cannot_start_when_ktalk_joins_then_capture_closed_and_join_rejected() {
    // given
    let r = rig();
    *r.capture.start_error.lock().unwrap() = Some("device gone".into());
    let (sink, ws) = r.h.add(Role::Extension);
    // when
    r.join(&ws, "ktalk").await.unwrap();
    // then
    assert_eq!(
        r.timeline(),
        ["before_meeting_switch", "meeting_changed(m1)", "set_capture(true)", "set_capture(false)"]
    );
    assert!(!r.h.bridge.is_recording_active());
    assert_eq!(r.h.bridge.meeting_owner(), None);
    assert_eq!(field(&last_frame(&sink), "reason").as_str(), Some("capture_unavailable"));
    assert!(sink.of_type("meeting_started").is_empty());
    assert!(r.h.logged("[KTalk Bridge] Capture start failed: device gone"));
}

/// tests/test_ktalk_only.py::test_join_before_main_loop_initialization_waits_for_ready
#[tokio::test]
async fn given_captures_not_ready_when_joined_then_refused_and_a_later_join_starts_capture() {
    // given
    let r = rig();
    *r.capture.start_error.lock().unwrap() = Some("Audio capture is not ready".into());
    let (sink, ws) = r.h.add(Role::Extension);
    r.join(&ws, "ktalk").await.unwrap();
    let recording_before_ready = r.h.bridge.is_recording_active();
    // when
    *r.capture.start_error.lock().unwrap() = None;
    r.join(&ws, "ktalk").await.unwrap();
    // then
    assert!(!recording_before_ready);
    assert!(r.h.bridge.is_recording_active());
    assert_eq!(r.capture_calls(), [true, false, true]);
    assert_eq!(types(&sink).iter().filter(|t| *t == "meeting_join_rejected").count(), 1);
}

/// tests/test_ktalk_only.py::test_foreign_tab_pcm_does_not_reach_ingest
#[tokio::test]
async fn given_owner_recording_when_foreign_tab_sends_pcm_then_ingest_does_not_get_it() {
    // given
    let r = rig();
    let (_sink, owner) = r.h.add(Role::Extension);
    let (_foreign_sink, foreign) = r.h.add(Role::Extension);
    r.join(&owner, "ktalk").await.unwrap();
    r.live.calls.lock().unwrap().clear();
    // when
    r.h.process(PCM_CHUNK, Some(&foreign)).await.unwrap();
    let after_foreign = r.timeline();
    r.h.process(PCM_CHUNK, Some(&owner)).await.unwrap();
    // then
    assert!(after_foreign.is_empty());
    assert_eq!(r.timeline(), ["pcm_chunk"], "the same chunk from the owner does reach it");
}

/// tests/test_ktalk_only.py::test_bridge_stop_closes_capture_and_invalidates_inflight_results
#[tokio::test]
async fn given_recording_meeting_when_bridge_stops_then_capture_switched_off() {
    // given
    let r = rig();
    let (_sink, ws) = r.h.add(Role::Extension);
    r.join(&ws, "ktalk").await.unwrap();
    // when
    r.h.bridge.stop();
    // then
    assert_eq!(r.capture_calls(), [true, false]);
    assert!(!r.h.bridge.is_recording_active());
}

/// tests/test_ktalk_only.py::test_one_failed_stop_still_closes_the_other_device
#[tokio::test]
async fn given_capture_stop_fails_when_owner_leaves_then_error_raised_before_meeting_switch() {
    // given
    let r = rig();
    let (_sink, ws) = r.h.add(Role::Extension);
    r.join(&ws, "ktalk").await.unwrap();
    *r.capture.stop_error.lock().unwrap() = Some("stop failed".into());
    // when
    let result = r.leave(&ws).await;
    // then
    assert_eq!(result, Err("stop failed".to_owned()));
    assert_eq!(r.capture_calls(), [true, false]);
    assert_eq!(r.store.ops(), ["on_meeting_join"]);
}

/// tests/test_ktalk_only.py::test_failed_join_can_retry_after_device_recovers
#[tokio::test]
async fn given_join_failed_on_busy_device_when_retried_after_recovery_then_meeting_starts() {
    // given
    let r = rig();
    *r.capture.start_error.lock().unwrap() = Some("busy".into());
    let (sink, ws) = r.h.add(Role::Extension);
    r.join(&ws, "ktalk").await.unwrap();
    let recording_after_failure = r.h.bridge.is_recording_active();
    *r.capture.start_error.lock().unwrap() = None;
    sink.clear();
    // when
    r.join(&ws, "ktalk").await.unwrap();
    // then
    assert!(!recording_after_failure);
    assert!(r.h.bridge.is_recording_active());
    assert!(r.h.bridge.tick_state().is_some());
    let kinds = types(&sink);
    assert!(kinds.iter().any(|t| t == "meeting_started"), "{kinds:?}");
    assert!(kinds.iter().any(|t| t == "transcript_history"), "{kinds:?}");
    assert_eq!(r.h.bridge.meeting_owner(), Some(ws.id));
}

/// tests/test_ktalk_only.py::test_debug_status_follows_capture_owner
#[tokio::test]
async fn given_debug_file_when_meeting_joined_and_left_then_recording_status_follows_capture() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("debug.json");
    let h = Harness::with(|c| {
        c.ktalk_only = true;
        c.debug_file = Some(path.clone());
    });
    let store = FakeStore::with_source("orphan");
    h.bridge.set_meeting_storage(store);
    let live = RecordingLive::new();
    let mut stats = Map::new();
    stats.insert("queued", Value::from(0_i64));
    *live.stats.lock().unwrap() = Some(Ok(stats));
    h.bridge.set_live_events(Some(live.clone()));
    h.bridge.set_meeting_capture(RecordingCapture::new());
    let read = || {
        h.bridge.reset_debug_throttle();
        h.bridge.write_debug_state();
        json(&std::fs::read_to_string(&path).unwrap())
    };
    let (_sink, ws) = h.add(Role::Extension);
    let idle = field(&read(), "recording").clone();
    // when
    h.process(
        r#"{"type": "meeting_join", "platform": "ktalk", "url": "https://example.ktalk.ru/work", "title": "Work"}"#,
        Some(&ws),
    )
    .await
    .unwrap();
    let joined = field(&read(), "recording").clone();
    h.process(r#"{"type": "meeting_leave"}"#, Some(&ws)).await.unwrap();
    let state = read();
    // then
    assert_eq!(idle, json(r#"{"policy": "ktalk_only", "active": false}"#));
    assert_eq!(joined, json(r#"{"policy": "ktalk_only", "active": true}"#));
    assert_eq!(field(&state, "recording"), &json(r#"{"policy": "ktalk_only", "active": false}"#));
    assert_eq!(field(&state, "pcm"), &json(r#"{"queued": 0}"#));
}
