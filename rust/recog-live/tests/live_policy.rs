//! The capture policy and the other duties of the main loop, ported from
//! tests/test_ktalk_only.py (with the real bridge and storage, as there), the live parts
//! of test_audio_retention.py and test_golden.py, the capture-loss and digital-silence
//! cases of test_live_*.py, the user profile, the teardown, and the capture choices of
//! test_system_audio_cli.py.

mod live_support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use live_support::{Rig, RigOptions, T0, field, rig, speech, text};
use recog_bridge::Bridge;
use recog_bridge::hooks::{FixedPolicy, LiveEvents};
use recog_bridge::testing::{FakeConnection, default_policy};
use recog_core::install_config::InstallConfig;
use recog_core::pyjson::Value;
use recog_live::live::LiveOptions;
use recog_live::live::build::{CaptureSettings, MicChoice, build_captures, create_echo_stage};
use recog_live::live::session::{self, LiveHooks, SessionConfig, stats_map};

fn session_config() -> SessionConfig {
    let mut storage = recog_core::meeting_storage::StorageOptions::new(std::path::Path::new("/nonexistent"));
    storage.log = Arc::new(|_, _| {});
    SessionConfig {
        bridge: recog_bridge::BridgeConfig { port: 0, debug_file: None, ..recog_bridge::BridgeConfig::default() },
        policy: Arc::new(FixedPolicy(default_policy())),
        increments: None,
        storage,
        golden_record: false,
    }
}

/// A `ktalk_only` transcriber with the real bridge and storage, and a tab to drive it.
struct KtalkOnly {
    r: Rig,
    bridge: Arc<Bridge>,
    runtime: tokio::runtime::Runtime,
    tab: FakeConnection,
}

impl KtalkOnly {
    fn new() -> Self {
        let r = rig(RigOptions {
            options: LiveOptions { ktalk_only: true, save_audio: true, ..LiveOptions::default() },
            ..RigOptions::default()
        });
        r.storage();
        let bridge = session::setup_bridge(&r.live, &mut session_config()).unwrap().unwrap();
        r.live.start_audio_captures().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        bridge.attach_runtime(runtime.handle().clone());
        let (tab, connection) = FakeConnection::new("('127.0.0.1', 50000)", None);
        let b = bridge.clone();
        runtime.spawn(async move { b.handle_client(connection).await });
        Self { r, bridge, runtime, tab }
    }

    fn send(&self, frame: &str) {
        self.tab.send_text(frame);
        self.runtime.block_on(self.tab.idle());
    }

    fn join(&self, platform: &str) {
        self.send(&format!(
            r#"{{"type": "meeting_join", "platform": "{platform}", "url": "https://example.ktalk.ru/work", "title": "Work"}}"#
        ));
    }

    fn idle(&self) -> bool {
        let mic = self.r.mic.as_ref().unwrap();
        let system = self.r.system.as_ref().unwrap();
        !self.r.live.is_recording_active()
            && !mic.running.load(Ordering::SeqCst)
            && !system.running.load(Ordering::SeqCst)
    }
}

#[test]
fn ktalk_only_at_startup_opens_no_device_and_transcribes_nothing() {
    // given
    let k = KtalkOnly::new();
    // when
    k.r.mic.as_ref().unwrap().push(speech(3.0), T0);
    k.r.live.process_chunk(true).unwrap();
    // then
    assert!(k.idle());
    assert_eq!(k.r.mic.as_ref().unwrap().starts.load(Ordering::SeqCst), 0);
    assert_eq!(k.r.asr.chunk_calls.load(Ordering::SeqCst), 0);
    assert!(k.r.live.wav_paths().0.is_none());
}

#[test]
fn ktalk_only_a_join_records_and_a_leave_discards_the_buffers_and_closes_the_files() {
    // given
    let k = KtalkOnly::new();
    k.join("ktalk");
    assert!(!k.idle());
    let meeting = k.r.storage().current_meeting_path().unwrap();
    assert_eq!(k.r.live.wav_paths().0.unwrap(), meeting.join("audio_mic.wav"));
    k.r.asr.says("Рабочая встреча");
    k.r.mic.as_ref().unwrap().push(speech(3.0), T0);
    k.r.live.process_chunk(true).unwrap();
    assert!(std::fs::metadata(meeting.join("audio_mic.wav")).unwrap().len() > 44);
    let mut state = k.r.live.source_state(false).unwrap();
    state.pending_chunk = Some((speech(3.0), T0, k.r.live.generation()));
    k.r.live.set_source_state(false, state);
    // when
    k.send(r#"{"type": "meeting_leave"}"#);
    // then
    assert!(k.idle());
    assert!(k.r.live.source_state(false).unwrap().pending_chunk.is_none());
    assert_eq!(k.r.live.wav_paths(), (None, None));
    let transcript = std::fs::read_to_string(meeting.join("transcript.jsonl")).unwrap();
    assert!(transcript.contains("Рабочая встреча"));
    let meta = recog_core::pyjson::loads(&std::fs::read_to_string(meeting.join("meta.json")).unwrap()).unwrap();
    assert_eq!(field(&meta, "recording_active"), &Value::Bool(false));
    assert!(text(&meta, "audio_mic_path").ends_with("audio_mic.wav"));
    // and a rejoin starts fresh
    k.join("ktalk");
    assert!(!k.idle());
    assert!(k.r.mic.as_ref().unwrap().get_chunk_count() == 0);
}

trait Count {
    fn get_chunk_count(&self) -> usize;
}

impl Count for live_support::FakeCapture {
    fn get_chunk_count(&self) -> usize {
        self.chunks.lock().unwrap().len()
    }
}

#[test]
fn ktalk_only_another_platform_joining_keeps_the_capture_off() {
    for platform in ["jitsi", "vas3k", "matrix", "gmeet", "zoom"] {
        // given
        let k = KtalkOnly::new();
        // when
        k.join(platform);
        // then
        assert!(k.idle(), "{platform}");
        let rejected = k.tab.sink.of_type("meeting_join_rejected");
        assert_eq!(field(&rejected[0], "reason"), &Value::from("ktalk_only"), "{platform}");
    }
}

#[test]
fn ktalk_only_the_owner_tab_disconnecting_stops_the_capture() {
    // given
    let k = KtalkOnly::new();
    k.join("ktalk");
    // when
    k.tab.close();
    k.runtime.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(50)).await });
    // then
    assert!(k.idle());
    let _ = &k.bridge;
}

#[test]
fn captures_not_ready_switching_the_capture_on_fails() {
    // given
    let r = rig(RigOptions {
        options: LiveOptions { ktalk_only: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    // when
    let result = r.live.set_conference_capture(true);
    // then
    assert_eq!(result, Err("Audio capture is not ready".into()));
}

#[test]
fn a_device_that_will_not_start_switching_the_capture_on_rolls_back_and_says_why() {
    // given
    let r = rig(RigOptions {
        options: LiveOptions { ktalk_only: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    r.storage();
    r.live.start_audio_captures().unwrap();
    r.system.as_ref().unwrap().fail_start.store(true, Ordering::SeqCst);
    // when
    let result = r.live.set_conference_capture(true);
    // then
    assert_eq!(result, Err("device busy".into()));
    assert!(!r.live.is_recording_active());
    assert!(!r.mic.as_ref().unwrap().running.load(Ordering::SeqCst));
    assert!(r.stdout().ends_with("Waiting for a KTalk meeting (audio capture off)\n"));
}

#[test]
fn a_lost_system_capture_checking_writes_one_marker_per_meeting_and_one_on_recovery() {
    // given
    let r =
        rig(RigOptions { options: LiveOptions { use_ktalk: true, ..LiveOptions::default() }, ..RigOptions::default() });
    let storage = r.storage();
    let system = r.system.as_ref().unwrap();
    system.dead.store(true, Ordering::SeqCst);
    // when
    r.live.check_system_capture_alive().unwrap();
    r.live.check_system_capture_alive().unwrap();
    // then
    let reasons = |rows: Vec<Value>| rows.iter().map(|r| text(r, "uncertain_reason")).collect::<Vec<_>>();
    assert_eq!(reasons(storage.get_current_transcript().unwrap()), ["system_audio_capture_lost"]);
    // and a new meeting gets its own
    storage.on_ktalk_leave("user_left").unwrap();
    r.live.check_system_capture_alive().unwrap();
    assert_eq!(reasons(storage.get_current_transcript().unwrap()), ["system_audio_capture_lost"]);
    // and the capture back gives the recovery marker
    system.dead.store(false, Ordering::SeqCst);
    r.live.check_system_capture_alive().unwrap();
    r.live.check_system_capture_alive().unwrap();
    let rows = storage.get_current_transcript().unwrap();
    assert_eq!(reasons(rows.clone()), ["system_audio_capture_lost", "system_audio_capture_recovered"]);
    assert_eq!(text(&rows[1], "text"), "[Системный звук восстановлен]");
}

#[test]
fn a_minute_of_zeros_in_a_conference_checking_requests_a_restart_once_per_interval() {
    // given
    let r = rig(RigOptions::default());
    let conference = r.conference();
    let system = r.system.as_ref().unwrap();
    *system.silence.lock().unwrap() = Some(61.0);
    // when: no conference
    r.live.restart_silent_system_capture();
    // then
    assert!(system.restarts.lock().unwrap().is_empty());
    // when: a conference, but within the 5 s interval of the last check
    conference.active.store(true, Ordering::SeqCst);
    r.live.restart_silent_system_capture();
    assert!(system.restarts.lock().unwrap().is_empty());
    // when: the next check
    r.clock.advance(5.0);
    r.live.restart_silent_system_capture();
    assert_eq!(*system.restarts.lock().unwrap(), ["Only digital silence for 61s during a conference"]);
    // when: real silence of less than a minute
    *system.silence.lock().unwrap() = Some(30.0);
    r.clock.advance(5.0);
    r.live.restart_silent_system_capture();
    assert_eq!(system.restarts.lock().unwrap().len(), 1);
}

#[test]
fn ktalk_only_idle_reopening_device_captures_leaves_them_alone() {
    let r = rig(RigOptions {
        options: LiveOptions { ktalk_only: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    r.live.reopen_stopped_device_captures();
    assert_eq!(r.mic.as_ref().unwrap().reopens.load(Ordering::SeqCst), 0);
    let r = rig(RigOptions::default());
    r.live.reopen_stopped_device_captures();
    assert_eq!(r.mic.as_ref().unwrap().reopens.load(Ordering::SeqCst), 1);
}

#[test]
fn a_conference_on_golden_recording_keeps_audio_and_answers_only_then() {
    // given
    let r = rig(RigOptions { system: false, ..RigOptions::default() });
    r.storage();
    let conference = r.conference();
    let dir = r.dir.path().join("golden-session");
    let recorder = Arc::new(recog_live::golden::GoldenRecorder::new(&dir).unwrap());
    r.live.set_golden(Some(recorder.clone()));
    r.asr.says("вне встречи");
    r.asr.says("во встрече");
    let mic = r.mic.as_ref().unwrap();
    mic.push(speech(3.0), T0);
    r.live.process_chunk(true).unwrap();
    // when
    conference.active.store(true, Ordering::SeqCst);
    mic.push(speech(3.0), T0 + 10.0);
    r.live.process_chunk(true).unwrap();
    recorder.close();
    // then
    let events = recog_live::golden::read_session(&dir).unwrap();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e.get("kind").and_then(Value::as_str)).collect();
    assert_eq!(kinds, ["capture", "asr"]);
    assert_eq!(events[1].get("output"), Some(&Value::from("во встрече")));
    assert_eq!(events[0].get("start_ts"), Some(&Value::Float(T0 + 10.0)));
}

#[test]
fn the_setting_on_starting_golden_recording_removes_old_sessions_and_names_the_new_by_time() {
    // given
    let r = rig(RigOptions::default());
    let old = r.data_dir().join("golden/2020-01-01_000000");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("events.jsonl"), "").unwrap();
    let file = std::fs::File::options().write(true).open(old.join("events.jsonl")).unwrap();
    file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000)).unwrap();
    // when
    let recorder = session::start_golden_recording(&r.live, true).unwrap().unwrap();
    // then: T0 is 2026-09-21 14:13:20 UTC
    assert!(!old.exists());
    assert_eq!(recorder.directory(), r.data_dir().join("golden/2026-09-21_141320"));
    assert_eq!(
        r.stderr.lines(),
        [
            "[golden] Removed 2020-01-01_000000, older than 14 days".to_owned(),
            format!("[golden] Recording the session into {}", recorder.directory().display()),
        ]
    );
    assert!(session::start_golden_recording(&r.live, false).unwrap().is_none());
}

#[test]
fn a_meeting_change_rotating_closes_the_old_files_into_their_meta_and_opens_new_ones() {
    // given
    let r = rig(RigOptions {
        options: LiveOptions { save_audio: true, use_ktalk: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    let storage = r.storage();
    let old = storage.current_meeting_path().unwrap();
    r.live.set_system_wav_anchor(Some((1.0, 2)));
    storage.on_ktalk_leave("user_left").unwrap();
    let new = storage.current_meeting_path().unwrap();
    // when
    r.live.on_meeting_change("x", &new, false).unwrap();
    // then
    assert_eq!(r.live.wav_paths(), (Some(new.join("audio_mic.wav")), Some(new.join("audio_system.wav"))));
    assert_eq!(r.live.system_wav_anchor(), None);
    assert!(r.stdout().contains(&format!("System audio saved: {}", old.join("audio_system.wav").display())));
    assert!(r.stdout().contains(&format!("Recording mic audio: {}", new.join("audio_mic.wav").display())));
}

#[test]
fn ktalk_only_a_non_ktalk_meeting_rotating_opens_no_files() {
    // given
    let r = rig(RigOptions {
        options: LiveOptions { save_audio: true, ktalk_only: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    let storage = r.storage();
    // when
    r.live.on_meeting_change("x", &storage.current_meeting_path().unwrap(), false).unwrap();
    // then
    assert_eq!(r.live.wav_paths(), (None, None));
}

#[test]
fn a_saved_profile_and_an_override_reading_the_identity_picks_the_override_then_the_profile() {
    // given
    let r = rig(RigOptions::default());
    recog_core::user_profile::UserProfileStore::new(&r.data_dir()).write("Вера", &[]).unwrap();
    // when
    let identity = r.live.read_user_identity().unwrap();
    // then
    assert_eq!(field(&identity, "local_user_name"), &Value::from("Вера"));
    let with_override = rig(RigOptions {
        options: LiveOptions { mic_speaker_name: Some("Гость".into()), ..LiveOptions::default() },
        ..RigOptions::default()
    });
    assert_eq!(with_override.live.mic_speaker_name(), "Гость");
    // and an applied identity relabels the microphone
    r.live.apply_user_identity(&identity);
    assert_eq!(r.live.mic_speaker_name(), "Вера");
}

#[test]
fn a_bridge_profile_refresh_applying_relabels_the_next_mic_row() {
    // given
    let r = rig(RigOptions {
        system: false,
        options: LiveOptions { use_ktalk: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    r.storage();
    let bridge = session::setup_bridge(&r.live, &mut session_config()).unwrap().unwrap();
    recog_core::user_profile::UserProfileStore::new(&r.data_dir()).write("Оля", &[]).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    bridge.attach_runtime(runtime.handle().clone());
    let (tab, connection) = FakeConnection::new("('127.0.0.1', 50001)", None);
    let b = bridge.clone();
    runtime.spawn(async move { b.handle_client(connection).await });
    // when
    tab.send_text(r#"{"type": "subscriber_hello"}"#);
    tab.send_text(r#"{"type": "user_profile_refresh", "request_id": "r1"}"#);
    runtime.block_on(tab.idle());
    // then
    assert_eq!(r.live.mic_speaker_name(), "Оля");
    assert!(!tab.sink.of_type("user_profile_applied").is_empty());
}

#[test]
fn the_end_of_a_run_tearing_down_ends_the_meeting_and_writes_the_transcript_file() {
    // given
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("transcript.md");
    let r = rig(RigOptions {
        system: false,
        options: LiveOptions { output_file: Some(path.clone()), ..LiveOptions::default() },
        ..RigOptions::default()
    });
    let storage = r.storage();
    r.live.handle_mic_chunk("первая", "12:00:00", T0, 0).unwrap();
    r.live.handle_mic_chunk("вторая", "12:00:05", T0 + 5.0, 0).unwrap();
    let meeting = storage.current_meeting_id().unwrap();
    // when
    session::teardown(&r.live, None);
    // then
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "\n**Me** *[12:00:00]*: первая\n\n\n**Me** *[12:00:05]*: вторая\n"
    );
    assert!(r.stdout().contains(&format!("Meeting saved: {meeting}\n")));
    assert!(r.stdout().contains(&format!("\n{}\nОстановлено.\n", "-".repeat(60))));
    assert!(storage.current_meeting_id().is_none());
    assert_eq!(r.mic.as_ref().unwrap().stops.load(Ordering::SeqCst), 1);
}

#[test]
fn a_pcm_frame_from_the_bridge_reaching_the_hooks_lands_in_the_ingest() {
    // given
    let r =
        rig(RigOptions { options: LiveOptions { use_ktalk: true, ..LiveOptions::default() }, ..RigOptions::default() });
    let hooks = LiveHooks(Arc::downgrade(&r.live));
    assert!(!hooks.wants_pcm());
    let ingest = Arc::new(recog_live::per_speaker::PerSpeakerIngest::new(
        Arc::new(|_| Ok(())),
        recog_live::per_speaker::IngestOptions::default(),
    ));
    r.live.set_ingest(Some(ingest.clone()));
    // when
    hooks
        .pcm_chunk(recog_bridge::hooks::PcmChunk {
            pid: "p".into(),
            speaker_name: "P".into(),
            is_local: false,
            seq: 3,
            chunk_start_ts: T0,
            chunk_end_ts: T0 + 1.0,
            sample_rate: 16_000,
            pcm: vec![1, 0, 2, 0],
        })
        .unwrap();
    // then
    assert!(hooks.wants_pcm());
    assert_eq!(ingest.buffered().get("p"), Some(&1));
    let stats = stats_map(&ingest.get_stats());
    assert_eq!(
        stats.keys().cloned().collect::<Vec<_>>(),
        [
            "pcm_ever_seen",
            "degraded",
            "meeting_active_since",
            "last_chunk_ts",
            "now",
            "pending_buffer_sizes",
            "per_pid"
        ]
    );
}

#[test]
fn a_speaker_update_reaching_the_hooks_sets_the_current_ktalk_speaker() {
    // given: no bridge object, only the speaker callback (Python's `_ktalk_current_speaker`)
    let r = rig(RigOptions {
        mic: false,
        options: LiveOptions { use_ktalk: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    r.storage();
    let hooks = LiveHooks(Arc::downgrade(&r.live));
    // when
    hooks.speaker_changed(Some("Дима"), true);
    r.live.handle_ktalk_chunk("привет", "12:00:00", T0 - 3.0, T0, T0, 0, false).unwrap();
    // then: without a bridge nothing is stored, as in Python; the line names the speaker
    assert!(r.live.transcript_lines()[0].contains("**Дима**"));
    hooks.speaker_changed(Some("Дима"), false);
    r.live.handle_ktalk_chunk("ещё", "12:00:30", T0 + 27.0, T0 + 30.0, T0 + 30.0, 0, false).unwrap();
    assert!(r.live.transcript_lines()[1].contains("**Unknown**"));
}

fn settings<'a>(root: &'a std::path::Path, config: &'a InstallConfig, platform: &'a str) -> CaptureSettings<'a> {
    CaptureSettings {
        repo_root: root,
        config,
        python: "/usr/bin/python3".into(),
        platform,
        log: recog_live::log::Logger::capture(recog_live::clock::SystemClock::shared()).0,
    }
}

#[test]
fn each_choice_building_captures_gives_the_sources_python_builds() {
    let root = tempfile::tempdir().unwrap();
    let config = InstallConfig::default();
    let linux = settings(root.path(), &config, "linux");
    // pulse with a named microphone: both
    let (mic, system) = build_captures("pulse", &MicChoice::Named("usb".into()), &linux).unwrap();
    assert_eq!(mic.unwrap().argv().last().unwrap(), "--device=usb");
    assert_eq!(system.unwrap().argv().last().unwrap(), "--device=@DEFAULT_MONITOR@");
    // pulse with the default input: Python builds no microphone in this mode
    let (mic, system) = build_captures("pulse", &MicChoice::Default, &linux).unwrap();
    assert!(mic.is_none() && system.is_some());
    // a device as system audio, the microphone beside it
    let (mic, system) = build_captures("device:alsa_input.loop", &MicChoice::Named("usb".into()), &linux).unwrap();
    assert!(mic.is_some());
    assert_eq!(system.unwrap().argv().last().unwrap(), "--device=alsa_input.loop");
    // no system audio: the default microphone alone
    let (mic, system) = build_captures("none", &MicChoice::Default, &linux).unwrap();
    assert_eq!(mic.unwrap().argv().last().unwrap(), "--device=@DEFAULT_SOURCE@");
    assert!(system.is_none());
    // nothing at all
    let (mic, system) = build_captures("none", &MicChoice::Off, &linux).unwrap();
    assert!(mic.is_none() && system.is_none());
    // a Mac: the microphone worker by name
    let mac = settings(root.path(), &config, "darwin");
    let (mic, _) = build_captures("none", &MicChoice::Named("Микрофон MacBook Pro".into()), &mac).unwrap();
    let argv = mic.unwrap().argv();
    assert!(argv[2].ends_with("recog/mic_worker.py"), "{argv:?}");
    assert_eq!(argv[argv.len() - 2..], ["--device".to_owned(), "Микрофон MacBook Pro".to_owned()]);
}

#[test]
fn echo_cancel_on_or_off_creating_the_stage_needs_screencapture() {
    let mut config = InstallConfig::default();
    assert!(create_echo_stage("screencapture", &config).is_none());
    config.live.echo_cancel = true;
    assert!(create_echo_stage("pulse", &config).is_none());
}
