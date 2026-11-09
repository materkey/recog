//! The bridge cases of the Python suites other than tests/test_ktalk_bridge.py
//! (tests/test_ktalk_only.py is in bridge_ktalk_only.rs): the profile and AI-settings
//! refreshes of the webapp, the Copilot switch, the supervisor's hello, and the
//! speaker queries the live attribution chain relies on.
//!
//! Where Python reads the profile or the settings from a file
//! (`UserProfileStore`, `AiSettingsStore`), the source here is a test double whose
//! answer the test sets: what the file holds is the stores' subject, what the bridge
//! does with the answer is this file's.
//!
//! Not ported:
//! - tests/test_user_profile_runtime.py: test_cli_given_optional_override_when_parsed_then_absence_is_distinct
//!   (subject is the CLI), test_live_given_profile_and_override_when_started_then_both_labels_agree beyond the
//!   snapshot (subject is `LiveTranscriber`'s name precedence),
//!   test_profile_given_new_name_when_mic_chunk_arrives_then_history_is_preserved (subject is `LiveTranscriber`),
//!   test_channel_given_profile_event_when_received_then_aliases_survive_and_no_reply_is_requested[*] (subject is
//!   the Copilot channel), not the bridge.
//! - tests/test_copilot_ai_settings.py: test_policy_event_given_literal_values_when_pushed_and_polled_then_separate_policy_survives,
//!   test_snapshot_given_saved_policy_when_tool_returns_then_literal_context_is_included (subject is the Copilot
//!   channel), test_runner_given_model_change_when_command_rebuilt_then_new_spawn_uses_new_model,
//!   test_manual_launcher_given_saved_model_when_invoked_then_argument_is_literal[*] (subject is the supervisor's
//!   runner and recog-service.sh), not the bridge.
//! - tests/test_copilot_toggle.py: every case but the last (subject is `AiSettingsStore` or the supervisor), not
//!   the bridge; of the last, the supervisor's runner stopping (subject is the supervisor).
//! - tests/test_copilot_supervisor.py: every case but the two below (subject is the supervisor), not the bridge.
//! - tests/test_live_speaker_attribution.py: every case but the sole-unmuted and remote-participant queries below
//!   (subject is `LiveTranscriber`, with a `Mock()` bridge or none), not the bridge.
//! - tests/test_acceptance_ktalk_unknown.py: test_retroactive_correction_end_to_end,
//!   test_orphan_meeting_no_sole_unmuted_check, test_orphan_meeting_no_retroactive_correction,
//!   test_mic_speaker_path_unchanged, test_fallback_chain_order_behavioral,
//!   test_meeting_boundary_clears_recent_segments (subject is `LiveTranscriber`), not the bridge.
//! - tests/test_golden.py, tests/test_retroactive_correction.py, tests/test_live_decomposed.py,
//!   tests/test_refine_integration.py, tests/test_audio_retention.py, tests/test_live_buffering.py,
//!   tests/test_refine_fallback.py, tests/test_refine_alignment.py, tests/test_system_audio_cli.py: every case
//!   (subject is `GoldenRecorder`, `MeetingStorage` or `LiveTranscriber`, with a `Mock()` bridge or none), not
//!   the bridge.
//! - tests/test_per_speaker.py, tests/test_per_speaker_refine_e2e.py: every case (subject is `PerSpeakerIngest`
//!   and the refine path; only the `PcmChunk` dataclass or a `MagicMock` bridge is used), not the bridge.
//! - tests/test_copilot_channel.py: every case (subject is the Copilot channel against a fake bridge server), not
//!   the bridge.

mod common;

use std::sync::{Arc, Mutex};

use common::{Harness, field, settle};
use recog_bridge::copilot::{MANUAL_MEETING_ID, TickState};
use recog_bridge::hooks::{BoxFuture, CopilotPolicy, FixedPolicy, Participant};
use recog_bridge::testing::{RecordingSink, StaticIdentity, collecting_log, frame_type};
use recog_bridge::{Bridge, BridgeConfig, Client, ClientSink, Role, SendError, TranscriptLine, obj};
use recog_core::pyjson::{Map, Value};

/// A sink that records like [`RecordingSink`] and, at the moment a frame is sent,
/// asks `check` about the state the receiver would see (the Python tests'
/// `Client(on_send=...)`).
struct CheckingSink {
    inner: Arc<RecordingSink>,
    check: Box<dyn Fn(&Value) + Send + Sync>,
}

impl ClientSink for CheckingSink {
    fn send(&self, text: String) -> BoxFuture<Result<(), SendError>> {
        (self.check)(&recog_core::pyjson::loads(&text).unwrap());
        self.inner.send(text)
    }
    fn close(&self, code: u16, reason: String) -> BoxFuture<Result<(), SendError>> {
        self.inner.close(code, reason)
    }
}

fn identity(name: &str, aliases: &[&str], revision: &str, known: bool) -> Value {
    obj! {
        "local_user_name" => name,
        "local_user_aliases" => Value::Array(aliases.iter().map(|a| Value::from(*a)).collect()),
        "local_user_profile_revision" => revision,
        "local_user_identity_known" => known,
    }
}

fn policy(instructions: &str, revision: &str, enabled: bool) -> CopilotPolicy {
    CopilotPolicy {
        instructions: Value::from(instructions),
        revision: Value::from(revision),
        enabled: Value::Bool(enabled),
    }
}

fn last(sink: &RecordingSink) -> Value {
    sink.frames().pop().expect("a frame was sent")
}

fn str_of<'a>(frame: &'a Value, key: &str) -> &'a str {
    field(frame, key).as_str().unwrap_or_else(|| panic!("{key} is not a string in {frame:?}"))
}

fn participants(list: &[(&str, &str, bool, bool)]) -> indexmap::IndexMap<String, Participant> {
    list.iter()
        .map(|(id, name, is_local, mic_muted)| {
            let p = Participant { id: (*id).into(), name: (*name).into(), is_local: *is_local, mic_muted: *mic_muted };
            ((*id).to_owned(), p)
        })
        .collect()
}

fn set_participants(h: &Harness, list: &[(&str, &str, bool, bool)]) {
    let list = participants(list);
    let () = h.bridge.with_speakers(|s| s.participants = list);
}

// ---- tests/test_user_profile_runtime.py ----

/// The `runtime` fixture: a bridge configured with the live process's identity.
fn profile_runtime(initial: Value) -> (Harness, Arc<StaticIdentity>) {
    let h = Harness::new();
    let source = StaticIdentity::new(initial.clone());
    h.bridge.configure_user_profile(&initial, source.clone()).unwrap();
    (h, source)
}

/// tests/test_user_profile_runtime.py::test_live_given_profile_and_override_when_started_then_both_labels_agree
/// (the bridge's side: the snapshot is the identity live configured)
#[tokio::test]
async fn given_live_identity_when_configured_then_snapshot_is_that_identity() {
    // given
    let saved = identity("Александр", &["Александр", "Саша", "Alex"], "rev-1", true);
    let h = Harness::new();
    // when
    h.bridge.configure_user_profile(&saved, StaticIdentity::new(saved.clone())).unwrap();
    // then
    assert_eq!(Value::Object(h.bridge.user_identity_snapshot()), saved);
}

/// tests/test_user_profile_runtime.py::test_subscriber_given_stale_revision_when_refreshed_then_latest_applies_before_ack
#[tokio::test]
async fn given_stale_refresh_hint_when_subscriber_refreshes_then_saved_identity_applied_before_ack() {
    // given
    let (h, source) = profile_runtime(identity("Александр", &["Александр", "Саша"], "rev-old", true));
    let saved = identity("Александр", &["Александр", "Alex"], "rev-saved", true);
    source.set(Ok(saved.clone()));
    let (channel, _c) = h.add(Role::Channel);
    h.bridge.force_manual_copilot_active(true);
    let applied_ok = Arc::new(Mutex::new(Vec::new()));
    let (bridge, applied, seen) = (h.bridge.clone(), source.clone(), applied_ok.clone());
    let subscriber_sink = RecordingSink::new();
    let checking = Arc::new(CheckingSink {
        inner: subscriber_sink.clone(),
        check: Box::new(move |frame| {
            if frame_type(frame) == Some("user_profile_applied") {
                let mic = applied.applied.lock().unwrap().last().cloned();
                let snapshot = Value::Object(bridge.user_identity_snapshot());
                seen.lock()
                    .unwrap()
                    .push(mic.as_ref() == Some(&snapshot) && Some(&snapshot) == field_opt(frame, "identity"));
            }
        }),
    });
    let subscriber = h.bridge.add_client(Role::Subscriber, checking, "('127.0.0.1', 12345)");
    // when
    h.process(
        r#"{"type": "user_profile_refresh", "revision": "rev-old", "name": "Untrusted", "aliases": ["Untrusted"]}"#,
        Some(&subscriber),
    )
    .await
    .unwrap();
    h.process(r#"{"type": "copilot_chat_message", "request_id": "manual-1", "text": "Hi"}"#, Some(&subscriber))
        .await
        .unwrap();
    // then
    assert_eq!(*applied_ok.lock().unwrap(), [true]);
    assert_eq!(str_of(&subscriber_sink.frames()[0], "revision"), "rev-saved");
    let event = &channel.frames()[0];
    assert_eq!(frame_type(event), Some("user_profile_updated"));
    assert_eq!(field(event, "local_user_aliases"), field(&saved, "local_user_aliases"));
    assert_eq!(str_of(event, "local_user_profile_revision"), "rev-saved");
    assert!(h.bridge.tick_state().is_none());
    let message = last(&channel);
    assert_eq!(frame_type(&message), Some("copilot_user_message"));
    assert_eq!(field(&message, "meeting_id"), &Value::Null);
    for (key, value) in h.bridge.user_identity_snapshot().iter() {
        assert_eq!(field(&message, key), value, "{key}");
    }
}

#[tokio::test]
async fn given_saved_profile_equal_to_current_when_subscriber_refreshes_then_acked_without_channel_update() {
    // given
    let current = identity("Александр", &["Александр", "Саша"], "rev-1", true);
    let (h, _source) = profile_runtime(current.clone());
    let (channel, _c) = h.add(Role::Channel);
    let (subscriber, sub) = h.add(Role::Subscriber);
    // when
    h.process(r#"{"type": "user_profile_refresh"}"#, Some(&sub)).await.unwrap();
    // then
    assert!(channel.frames().is_empty());
    let acks = subscriber.frames();
    assert_eq!(acks.len(), 1);
    assert_eq!(frame_type(&acks[0]), Some("user_profile_applied"));
    assert_eq!(str_of(&acks[0], "revision"), "rev-1");
    assert_eq!(field(&acks[0], "identity"), &current);
}

fn field_opt<'a>(frame: &'a Value, key: &str) -> Option<&'a Value> {
    frame.as_object().and_then(|m| m.get(key))
}

/// tests/test_user_profile_runtime.py::test_extension_given_saved_change_when_refresh_requested_then_identity_is_unchanged
#[tokio::test]
async fn given_saved_profile_change_when_extension_requests_refresh_then_identity_unchanged() {
    // given
    let initial = identity("Legacy", &["Legacy"], "rev-1", false);
    let (h, source) = profile_runtime(initial.clone());
    source.set(Ok(identity("Александр", &["Александр"], "rev-2", true)));
    let (extension, ext) = h.add(Role::Extension);
    // when
    h.process(r#"{"type": "user_profile_refresh"}"#, Some(&ext)).await.unwrap();
    // then
    assert!(source.applied.lock().unwrap().is_empty());
    assert_eq!(Value::Object(h.bridge.user_identity_snapshot()), initial);
    assert!(extension.frames().is_empty());
}

async fn refresh_fails(h: &Harness, source: &StaticIdentity, initial: &Value) {
    let (subscriber, sub) = h.add(Role::Subscriber);
    // when
    h.process(r#"{"type": "user_profile_refresh"}"#, Some(&sub)).await.unwrap();
    // then
    let kinds: Vec<_> = subscriber.frames().iter().filter_map(|f| frame_type(f).map(str::to_owned)).collect();
    assert_eq!(kinds, ["user_profile_error"]);
    assert!(!str_of(&subscriber.frames()[0], "message").contains("private"));
    assert_eq!(&Value::Object(h.bridge.user_identity_snapshot()), initial);
    assert!(source.applied.lock().unwrap().is_empty());
}

/// tests/test_user_profile_runtime.py::test_profile_given_failure_when_refreshed_then_error_without_success[read]
#[tokio::test]
async fn given_unreadable_profile_when_refreshed_then_error_without_success() {
    // given
    let initial = identity("Legacy", &["Legacy"], "rev-1", false);
    let (h, source) = profile_runtime(initial.clone());
    source.set(Err("Expecting property name enclosed in double quotes: private path".into()));
    refresh_fails(&h, &source, &initial).await;
}

/// tests/test_user_profile_runtime.py::test_profile_given_failure_when_refreshed_then_error_without_success[apply]
#[tokio::test]
async fn given_microphone_refuses_identity_when_refreshed_then_error_without_success() {
    // given
    let initial = identity("Legacy", &["Legacy"], "rev-1", false);
    let (h, source) = profile_runtime(initial.clone());
    source.set(Ok(identity("Александр", &["Александр"], "rev-2", true)));
    *source.apply_error.lock().unwrap() = Some("private path/name".into());
    refresh_fails(&h, &source, &initial).await;
}

/// tests/test_user_profile_runtime.py::test_tick_given_queued_old_identity_when_profile_changes_then_events_use_latest
#[tokio::test]
async fn given_tick_queued_with_old_identity_when_profile_changes_then_events_carry_latest() {
    // given
    let (h, source) = profile_runtime(identity("Александр", &["Александр", "Саша"], "rev-1", true));
    let (channel, _c) = h.add(Role::Channel);
    h.attach();
    h.bridge.set_tick_state(Some(TickState {
        meeting_id: Some("m-1".into()),
        meeting_title: Value::from("Meeting"),
        seg_count: 0,
        last_seg_count: 0,
        last_emit_ts: 0.0,
    }));
    // Queued on the audio path, before the event loop dispatches it.
    h.bridge.send_transcript(&TranscriptLine {
        speaker: "Bob".into(),
        text: "hello".into(),
        segment_id: 1,
        ..Default::default()
    });
    source.set(Ok(identity("Александр", &["Александр", "Alex"], "rev-2", true)));
    let (subscriber, sub) = h.add(Role::Subscriber);
    // when
    h.process(r#"{"type": "user_profile_refresh"}"#, Some(&sub)).await.unwrap();
    settle().await;
    h.bridge.emit_lifecycle(obj! {"type" => "meeting_started", "meeting_id" => "m-1"}).await;
    h.process(r#"{"type": "get_recent_transcript_request", "request_id": "snap"}"#, Some(&sub)).await.unwrap();
    // then
    let mut relevant: Vec<Value> = channel
        .frames()
        .into_iter()
        .filter(|f| matches!(frame_type(f), Some("transcript_tick" | "meeting_started")))
        .collect();
    let mut kinds: Vec<_> = relevant.iter().filter_map(|f| frame_type(f).map(str::to_owned)).collect();
    kinds.sort();
    kinds.dedup();
    assert_eq!(kinds, ["meeting_started", "transcript_tick"]);
    relevant.push(last(&subscriber));
    for frame in &relevant {
        assert_eq!(field(frame, "local_user_aliases"), &common::json(r#"["Александр", "Alex"]"#), "{frame:?}");
        assert_eq!(str_of(frame, "local_user_profile_revision"), "rev-2");
        assert_eq!(field(frame, "local_user_identity_known"), &Value::Bool(true));
    }
}

/// tests/test_user_profile_runtime.py::test_clear_given_profile_when_refreshed_then_removed_aliases_leave_manual_session
#[tokio::test]
async fn given_cleared_profile_when_refreshed_then_removed_aliases_leave_channel_identity() {
    // given
    let (h, source) = profile_runtime(identity("Александр", &["Александр", "Саша"], "rev-1", true));
    let (_subscriber, sub) = h.add(Role::Subscriber);
    let (channel, _c) = h.add(Role::Channel);
    source.set(Ok(identity("Legacy", &["Legacy"], "rev-cleared", false)));
    // when
    h.process(r#"{"type": "user_profile_refresh"}"#, Some(&sub)).await.unwrap();
    // then
    assert_eq!(str_of(&source.applied.lock().unwrap()[0], "local_user_name"), "Legacy");
    assert_eq!(field(&channel.frames()[0], "local_user_aliases"), &common::json(r#"["Legacy"]"#));
    assert_eq!(str_of(&channel.frames()[0], "local_user_profile_revision"), "rev-cleared");
}

// ---- tests/test_copilot_ai_settings.py ----

/// The `runtime` fixture: a channel and a subscriber registered.
struct Settings {
    h: Harness,
    channel: Arc<RecordingSink>,
    channel_client: Arc<Client>,
    subscriber: Arc<RecordingSink>,
    sub: Arc<Client>,
}

fn settings_runtime() -> Settings {
    let h = Harness::new();
    let (channel, channel_client) = h.add(Role::Channel);
    let (subscriber, sub) = h.add(Role::Subscriber);
    Settings { h, channel, channel_client, subscriber, sub }
}

/// tests/test_copilot_ai_settings.py::test_saved_policy_given_initial_bridge_when_loaded_then_literal_policy_is_bound
#[test]
fn given_saved_policy_when_bridge_built_then_literal_policy_is_bound() {
    // given
    let prompt = r#"Answer with JSON {"name": "{user_name}"}; preserve $[]\n literally."#;
    let source = Arc::new(FixedPolicy(policy(prompt, "rev-1", true)));
    let config = BridgeConfig { host: "127.0.0.1".into(), port: 0, debug_file: None, ..BridgeConfig::default() };
    let (log, _lines) = collecting_log();
    // when
    let bridge = Bridge::with_clock(config, source, recog_bridge::testing::ManualClock::at(common::NOW), log).unwrap();
    // then
    let mut expected = Map::new();
    expected.insert("copilot_instructions", Value::from(prompt));
    expected.insert("copilot_policy_revision", Value::from("rev-1"));
    expected.insert("copilot_enabled", Value::Bool(true));
    assert_eq!(bridge.copilot_policy_snapshot(), expected);
}

/// tests/test_copilot_ai_settings.py::test_manual_session_given_saved_policy_when_refreshed_then_applied_before_ack
#[tokio::test]
async fn given_manual_session_and_saved_policy_when_refreshed_then_applied_before_ack() {
    // given
    let h = Harness::new();
    let (channel, _c) = h.add(Role::Channel);
    let (supervisor, _s) = h.add(Role::Supervisor);
    h.bridge.force_manual_copilot_active(true);
    h.policy.set(Ok(policy("Reply in English. No proactive notes.", "rev-2", true)));
    let checks = Arc::new(Mutex::new(Vec::new()));
    let (bridge, seen, channel_seen) = (h.bridge.clone(), checks.clone(), channel.clone());
    let subscriber_sink = RecordingSink::new();
    let checking = Arc::new(CheckingSink {
        inner: subscriber_sink.clone(),
        check: Box::new(move |frame| {
            if frame_type(frame) == Some("ai_settings_applied") {
                let revision = bridge.copilot_policy_snapshot().get("copilot_policy_revision").cloned();
                let told = channel_seen.frames().pop().and_then(|f| field_opt(&f, "copilot_instructions").cloned());
                seen.lock().unwrap().push(
                    revision.as_ref() == field_opt(frame, "revision")
                        && told == Some(Value::from("Reply in English. No proactive notes.")),
                );
            }
        }),
    });
    let subscriber = h.bridge.add_client(Role::Subscriber, checking, "('127.0.0.1', 12345)");
    // when
    h.process(
        r#"{"type": "ai_settings_refresh", "revision": "stale", "copilot_instructions": "Untrusted frame policy"}"#,
        Some(&subscriber),
    )
    .await
    .unwrap();
    let after_refresh = subscriber_sink.frames();
    h.process(r#"{"type": "copilot_chat_message", "request_id": "manual-1", "text": "Hi"}"#, Some(&subscriber))
        .await
        .unwrap();
    // then
    assert_eq!(*checks.lock().unwrap(), [true]);
    assert_eq!(
        after_refresh,
        [common::json(r#"{"type": "ai_settings_applied", "revision": "rev-2", "model_application": "next_session"}"#)]
    );
    assert!(h.bridge.manual_copilot_active());
    assert_eq!(supervisor.frames(), [common::json(r#"{"type": "copilot_settings_changed"}"#)]);
    let frames = channel.frames();
    assert_eq!(frame_type(&frames[frames.len() - 2]), Some("copilot_policy_updated"));
    let message = last(&channel);
    assert_eq!(field(&message, "meeting_id"), &Value::Null);
    assert_eq!(str_of(&message, "copilot_policy_revision"), "rev-2");
}

/// tests/test_copilot_ai_settings.py::test_unregistered_client_given_saved_change_when_refresh_requested_then_policy_unchanged
#[tokio::test]
async fn given_saved_change_when_non_subscriber_requests_refresh_then_policy_unchanged() {
    // given
    let Settings { h, channel, channel_client, subscriber, .. } = settings_runtime();
    let initial = h.bridge.copilot_policy_snapshot();
    h.policy.set(Ok(policy("New policy", "rev-2", true)));
    let (extension, ext) = h.add(Role::Extension);
    // when
    for client in [Some(&ext), Some(&channel_client), None] {
        h.process(r#"{"type": "ai_settings_refresh"}"#, client).await.unwrap();
    }
    // then
    assert_eq!(h.bridge.copilot_policy_snapshot(), initial);
    assert!(channel.frames().is_empty());
    assert!(extension.frames().is_empty());
    assert!(subscriber.frames().is_empty());
}

/// tests/test_copilot_ai_settings.py::test_custom_policy_given_reset_when_refreshed_then_default_replaces_previous
#[tokio::test]
async fn given_custom_policy_when_reset_and_refreshed_then_default_replaces_previous() {
    // given
    let Settings { h, channel, sub, .. } = settings_runtime();
    let default = recog_bridge::testing::default_policy();
    h.policy.set(Ok(policy("Always use long English replies", "rev-2", true)));
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    // when
    h.policy.set(Ok(CopilotPolicy { revision: Value::from("rev-reset"), ..default.clone() }));
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    // then
    let latest = last(&channel);
    assert_eq!(field(&latest, "copilot_instructions"), &default.instructions);
    assert_eq!(str_of(&latest, "copilot_policy_revision"), "rev-reset");
    assert!(!str_of(&latest, "copilot_instructions").contains("Always use long English replies"));
}

/// tests/test_copilot_ai_settings.py::test_changed_policy_given_repeat_refresh_when_reconnected_then_latest_revision_acked
#[tokio::test]
async fn given_changed_policy_when_refreshed_twice_and_channel_reconnects_then_latest_revision_acked_once() {
    // given
    let Settings { h, channel, subscriber, sub, .. } = settings_runtime();
    h.policy.set(Ok(policy("Only explicit replies", "rev-2", true)));
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    let (reconnect, again) = h.add(Role::Extension);
    // when
    h.process(r#"{"type": "copilot_channel_hello"}"#, Some(&again)).await.unwrap();
    // then
    assert_eq!(channel.frames().len(), 1);
    assert_eq!(subscriber.frames().len(), 2);
    let event = reconnect.of_type("copilot_policy_updated").into_iter().next().expect("policy sent on hello");
    assert_eq!(str_of(&event, "copilot_policy_revision"), "rev-2");
    assert_eq!(str_of(&event, "copilot_instructions"), "Only explicit replies");
}

/// tests/test_copilot_ai_settings.py::test_invalid_saved_file_given_refresh_when_read_fails_then_old_policy_and_no_ack
#[tokio::test]
async fn given_invalid_saved_settings_when_refreshed_then_old_policy_kept_and_error_sent() {
    // given
    let Settings { h, channel, subscriber, sub, .. } = settings_runtime();
    let initial = h.bridge.copilot_policy_snapshot();
    h.policy.set(Err("Expecting property name enclosed in double quotes".into()));
    // when
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    // then
    assert_eq!(h.bridge.copilot_policy_snapshot(), initial);
    assert!(channel.frames().is_empty());
    assert_eq!(frame_type(&subscriber.frames()[0]), Some("ai_settings_error"));
}

/// tests/test_copilot_ai_settings.py::test_hot_policy_given_tick_lifecycle_and_snapshot_when_sent_then_full_policy_included
#[tokio::test]
async fn given_hot_policy_when_tick_lifecycle_and_snapshot_sent_then_each_carries_full_policy() {
    // given
    let Settings { h, channel, channel_client, sub, .. } = settings_runtime();
    h.attach();
    h.policy.set(Ok(policy("Use current policy", "rev-2", true)));
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    h.bridge.set_tick_state(Some(TickState {
        meeting_id: Some("meeting-1".into()),
        meeting_title: Value::from("Meeting"),
        seg_count: 0,
        last_seg_count: 0,
        last_emit_ts: 0.0,
    }));
    // when
    h.bridge.send_transcript(&TranscriptLine {
        speaker: "Bob".into(),
        text: "hello".into(),
        segment_id: 1,
        ..Default::default()
    });
    settle().await;
    h.bridge.emit_lifecycle(obj! {"type" => "meeting_started", "meeting_id" => "meeting-1"}).await;
    h.process(r#"{"type": "get_recent_transcript_request", "request_id": "snapshot-1"}"#, Some(&channel_client))
        .await
        .unwrap();
    // then
    let frames = channel.frames();
    let tail = &frames[frames.len() - 3..];
    let mut kinds: Vec<_> = tail.iter().filter_map(|f| frame_type(f).map(str::to_owned)).collect();
    kinds.sort();
    assert_eq!(kinds, ["get_recent_transcript_response", "meeting_started", "transcript_tick"]);
    for frame in tail {
        assert_eq!(str_of(frame, "copilot_instructions"), "Use current policy");
        assert_eq!(str_of(frame, "copilot_policy_revision"), "rev-2");
    }
}

// ---- tests/test_copilot_toggle.py ----

/// tests/test_copilot_toggle.py::test_bridge_given_disabled_policy_when_chat_start_or_late_reply_arrives_then_no_delivery
#[tokio::test]
async fn given_copilot_switched_off_when_chat_spawn_or_late_reply_arrives_then_nothing_delivered() {
    // given
    let h = Harness::new();
    let (subscriber, sub) = h.add(Role::Subscriber);
    let (channel, channel_client) = h.add(Role::Channel);
    let (relay, _s) = h.add(Role::Supervisor);
    h.bridge.force_manual_copilot_active(true);
    h.policy.set(Ok(policy("Be brief.", "rev-off", false)));
    h.process(r#"{"type": "ai_settings_refresh"}"#, Some(&sub)).await.unwrap();
    let manual_after_refresh = h.bridge.manual_copilot_active();
    let relayed_on_refresh = relay.frames();
    let acked = last(&subscriber);
    channel.clear();
    relay.clear();
    // when
    let mut answers = Vec::new();
    for kind in ["copilot_spawn_request", "copilot_chat_message"] {
        let frame = format!(r#"{{"type": "{kind}", "request_id": "{kind}", "text": "Hello"}}"#);
        h.process(&frame, Some(&sub)).await.unwrap();
        answers.push(last(&subscriber));
    }
    let count = subscriber.frames().len();
    h.process(r#"{"type": "copilot_channel_reply", "request_id": "late", "text": "Late"}"#, Some(&channel_client))
        .await
        .unwrap();
    // then
    assert!(!manual_after_refresh);
    assert_eq!(relayed_on_refresh, [common::json(r#"{"type": "copilot_settings_changed"}"#)]);
    assert_eq!(str_of(&acked, "revision"), "rev-off");
    for answer in &answers {
        assert_eq!(str_of(answer, "status"), "error");
        assert!(str_of(answer, "message").contains("выключен"));
    }
    assert_eq!(subscriber.frames().len(), count);
    assert!(channel.frames().is_empty());
    assert!(relay.frames().is_empty());
}

// ---- tests/test_copilot_supervisor.py ----

/// tests/test_copilot_supervisor.py::test_manual_meeting_id_matches_bridge_constant
#[test]
fn given_bridge_manual_constant_when_compared_then_it_is_manual() {
    // given
    let supervisor_constant = "manual";
    // when
    let bridge_constant = MANUAL_MEETING_ID;
    // then
    assert_eq!(bridge_constant, supervisor_constant);
}

/// tests/test_copilot_supervisor.py::test_given_live_manual_session_when_hello_crosses_wire_then_bridge_rearms
/// (the bridge's side: the hello a supervisor with a live manual session sends)
#[tokio::test]
async fn given_supervisor_with_live_manual_session_when_its_hello_arrives_then_bridge_rearms_and_reports_manual() {
    // given
    let h = Harness::new();
    let (stub, client) = h.add(Role::Extension);
    // when
    h.process(r#"{"type": "supervisor_hello", "current_meeting_id": "manual"}"#, Some(&client)).await.unwrap();
    // then
    assert!(h.bridge.manual_copilot_active());
    let states = stub.of_type("meeting_state");
    assert_eq!(states.len(), 1);
    assert_eq!(field(&states[0], "active"), &Value::Bool(true));
    assert_eq!(str_of(&states[0], "meeting_id"), MANUAL_MEETING_ID);
}

// ---- tests/test_live_speaker_attribution.py ----

/// tests/test_live_speaker_attribution.py::test_sole_unmuted_fallback_used_when_all_other_signals_fail
#[tokio::test]
async fn given_one_remote_participant_unmuted_when_sole_unmuted_asked_then_that_participant() {
    // given
    let h = Harness::new();
    set_participants(&h, &[("p1", "Alice", false, true), ("p2", "Bob", false, false), ("p3", "Carol", false, true)]);
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole.as_deref(), Some("Bob"));
}

/// tests/test_live_speaker_attribution.py::test_sole_unmuted_fallback_not_used_when_multiple_unmuted
#[tokio::test]
async fn given_two_remote_participants_unmuted_when_sole_unmuted_asked_then_none() {
    // given
    let h = Harness::new();
    set_participants(&h, &[("p1", "Alice", false, false), ("p2", "Bob", false, false), ("p3", "Carol", false, true)]);
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole, None);
}

/// tests/test_live_speaker_attribution.py::test_participants_has_remote_participants_counts_only_others[participants0-False]
#[tokio::test]
async fn given_no_participants_when_remote_participants_asked_then_false() {
    // given
    let h = Harness::new();
    // when
    let remote = h.bridge.has_remote_participants();
    // then
    assert!(!remote);
}

/// tests/test_live_speaker_attribution.py::test_participants_has_remote_participants_counts_only_others[participants1-False]
#[tokio::test]
async fn given_only_the_user_in_call_when_remote_participants_asked_then_false() {
    // given
    let h = Harness::new();
    set_participants(&h, &[("me", "Me", true, false)]);
    // when
    let remote = h.bridge.has_remote_participants();
    // then
    assert!(!remote);
}

/// tests/test_live_speaker_attribution.py::test_participants_has_remote_participants_counts_only_others[participants2-True]
#[tokio::test]
async fn given_a_muted_remote_participant_when_remote_participants_asked_then_true() {
    // given
    let h = Harness::new();
    set_participants(&h, &[("a", "Alice", false, true)]);
    // when
    let remote = h.bridge.has_remote_participants();
    // then
    assert!(remote);
}

// ---- tests/test_acceptance_ktalk_unknown.py ----

/// tests/test_acceptance_ktalk_unknown.py::test_speaker_switch_with_mute_sole_unmuted_fallback
#[tokio::test]
async fn given_speaker_muted_and_another_unmuted_without_history_when_chain_queried_then_sole_unmuted_names_speaker() {
    // given
    let h = Harness::new();
    set_participants(&h, &[("p1", "Alice", false, true), ("p2", "Bob", false, false), ("p3", "Carol", false, true)]);
    let now = h.bridge.now();
    // when
    let dominant = h.bridge.get_dominant_speaker_for_period(now - 3.0, now);
    let current = h.bridge.get_current_speaker();
    let alice_muted = h.bridge.is_speaker_muted("Alice");
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(dominant, None, "no events in the history");
    assert_eq!(current, None);
    assert!(alice_muted, "the last speaker's memory is blocked by her mute");
    assert_eq!(sole.as_deref(), Some("Bob"));
    assert_eq!(dominant.or(current).or(sole).as_deref(), Some("Bob"));
}

/// tests/test_acceptance_ktalk_unknown.py::test_speaker_switch_three_participants_one_unmuted
#[tokio::test]
async fn given_five_remote_participants_one_unmuted_when_sole_unmuted_asked_then_that_participant() {
    // given
    let h = Harness::new();
    set_participants(
        &h,
        &[
            ("p1", "Alice", false, true),
            ("p2", "Bob", false, true),
            ("p3", "Carol", false, false),
            ("p4", "Dave", false, true),
            ("p5", "Eve", false, true),
        ],
    );
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole.as_deref(), Some("Carol"));
}

/// tests/test_acceptance_ktalk_unknown.py::test_multi_speaker_two_unmuted_preserves_unknown
#[tokio::test]
async fn given_two_of_three_remote_unmuted_when_sole_unmuted_asked_then_none() {
    // given
    let h = Harness::new();
    set_participants(&h, &[("p1", "Alice", false, false), ("p2", "Bob", false, false), ("p3", "Carol", false, true)]);
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole, None);
}

/// tests/test_acceptance_ktalk_unknown.py::test_multi_speaker_all_unmuted_preserves_unknown
#[tokio::test]
async fn given_all_remote_unmuted_when_sole_unmuted_asked_then_none() {
    // given
    let h = Harness::new();
    set_participants(&h, &[("p1", "Alice", false, false), ("p2", "Bob", false, false), ("p3", "Carol", false, false)]);
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole, None);
}

/// tests/test_acceptance_ktalk_unknown.py::test_local_participant_excluded_from_sole_unmuted
#[tokio::test]
async fn given_only_local_participant_unmuted_when_sole_unmuted_asked_then_none() {
    // given
    let h = Harness::new();
    set_participants(&h, &[("p_local", "Me", true, false), ("p1", "Alice", false, true), ("p2", "Bob", false, true)]);
    // when
    let sole = h.bridge.get_sole_unmuted_remote_participant();
    // then
    assert_eq!(sole, None);
}

/// tests/test_acceptance_ktalk_unknown.py::test_correction_flag_in_send_transcript
#[tokio::test]
async fn given_connected_client_when_correction_transcript_sent_then_frame_carries_correction_true() {
    // given
    let h = Harness::new();
    h.attach();
    let (client, _ext) = h.add(Role::Extension);
    // when
    h.bridge.send_transcript(&TranscriptLine {
        speaker: "Alice".into(),
        text: "hello".into(),
        timestamp: "12:00:00".into(),
        correction: true,
        ..Default::default()
    });
    settle().await;
    // then
    let sent = client.frames();
    assert_eq!(sent.len(), 1);
    assert_eq!(frame_type(&sent[0]), Some("transcript"));
    assert_eq!(str_of(&sent[0], "speaker"), "Alice");
    assert_eq!(field(&sent[0], "correction"), &Value::Bool(true));
}
