//! The mixed path of the main loop (`_process_chunk` and its handlers), ported from
//! tests/test_live_decomposed.py, test_live_buffering.py and
//! test_live_speaker_attribution.py: the ASR gate and retry slot, capture times,
//! dedup, the microphone handler, the silence split, the attribution chain of system
//! audio and the retroactive correction.

mod live_support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use live_support::{FakeCapture, Rig, RigOptions, T0, field, rig, silence, speech, text};
use recog_core::pyjson::Value;
use recog_live::asr::AsrError;
use recog_live::live::LiveOptions;

fn mic_rig() -> Rig {
    rig(RigOptions { system: false, ..RigOptions::default() })
}

fn mic(r: &Rig) -> &Arc<FakeCapture> {
    r.mic.as_ref().unwrap()
}

fn system(r: &Rig) -> &Arc<FakeCapture> {
    r.system.as_ref().unwrap()
}

#[test]
fn asr_unavailable_processing_a_chunk_leaves_the_buffer_unread() {
    // given
    let r = mic_rig();
    r.asr.available.store(false, Ordering::SeqCst);
    mic(&r).push(speech(3.0), T0);
    // when
    r.live.process_chunk(true).unwrap();
    // then
    assert_eq!(mic(&r).reads.load(Ordering::SeqCst), 0);
}

#[test]
fn asr_lost_between_gate_and_call_processing_keeps_the_chunk_in_the_retry_slot() {
    // given
    let r = mic_rig();
    r.asr.fails(AsrError::TemporarilyUnavailable);
    mic(&r).push(speech(3.0), 1000.0);
    // when
    r.live.process_chunk(true).unwrap();
    // then
    let (chunk, ts, generation) = r.live.source_state(true).unwrap().pending_chunk.unwrap();
    assert_eq!(chunk.len(), 48_000);
    assert!((ts - 1000.0).abs() < 1e-9);
    assert_eq!(generation, r.live.generation());
}

#[test]
fn a_held_chunk_asr_back_transcribes_it_without_rereading_or_rewriting_the_wav() {
    // given
    let r = rig(RigOptions {
        system: false,
        options: LiveOptions { save_audio: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    let storage = r.storage();
    r.asr.fails(AsrError::TemporarilyUnavailable);
    mic(&r).push(speech(3.0), T0);
    r.live.process_chunk(true).unwrap();
    let written = r.live.wav_offsets_ms().0.unwrap();
    // when
    r.asr.says("привет мир");
    r.live.process_chunk(true).unwrap();
    // then
    assert_eq!(mic(&r).reads.load(Ordering::SeqCst), 1);
    assert_eq!(r.live.wav_offsets_ms().0.unwrap(), written);
    assert!(r.live.source_state(true).unwrap().pending_chunk.is_none());
    let rows = storage.get_current_transcript().unwrap();
    assert_eq!(text(&rows[0], "text"), "привет мир");
    let captured_at = recog_core::time::fromisoformat(&text(&rows[0], "ts")).unwrap();
    assert_eq!(captured_at.utc_micros(), Some(((T0 + 3.0) * 1e6) as i64));
}

#[test]
fn a_held_chunk_of_an_old_meeting_processing_drops_it_and_reads_the_buffer() {
    // given
    let r = mic_rig();
    let mut state = r.live.source_state(true).unwrap();
    state.pending_chunk = Some((speech(3.0), 1000.0, r.live.generation()));
    r.live.set_source_state(true, state);
    r.live.invalidate_pending_chunks();
    let mut state = r.live.source_state(true).unwrap();
    state.pending_chunk = Some((speech(3.0), 1000.0, r.live.generation() - 1));
    r.live.set_source_state(true, state);
    // when
    r.live.process_chunk(true).unwrap();
    // then
    assert!(r.live.source_state(true).unwrap().pending_chunk.is_none());
    assert_eq!(mic(&r).reads.load(Ordering::SeqCst), 1);
    assert_eq!(r.asr.chunk_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_backlog_chunk_an_hour_old_processing_stamps_the_segment_with_its_capture_time() {
    // given
    let r = mic_rig();
    let storage = r.storage();
    r.asr.says("привет мир");
    mic(&r).push(speech(3.0), T0 - 3600.0);
    // when
    r.live.process_chunk(true).unwrap();
    // then
    let rows = storage.get_current_transcript().unwrap();
    let ts = recog_core::time::fromisoformat(&text(&rows[0], "ts")).unwrap();
    assert_eq!(ts.utc_micros(), Some(((T0 - 3600.0 + 3.0) * 1e6) as i64));
}

#[test]
fn a_short_chunk_processing_skips_it() {
    // given
    let r = mic_rig();
    mic(&r).push(speech(0.05), T0);
    // when
    r.live.process_chunk(true).unwrap();
    // then
    assert_eq!(r.asr.chunk_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn adjacent_chunks_with_the_same_words_processing_deduplicates_the_second() {
    // given
    let r = mic_rig();
    let storage = r.storage();
    r.asr.says("привет мир как дела");
    r.asr.says("как дела");
    mic(&r).push(speech(3.0), T0);
    mic(&r).push(speech(3.0), T0 + 2.5);
    // when
    r.live.process_chunk(true).unwrap();
    r.live.process_chunk(true).unwrap();
    // then
    assert_eq!(storage.get_current_transcript().unwrap().len(), 1);
}

#[test]
fn the_same_phrase_after_the_dedup_window_processing_keeps_it() {
    // given
    let r = mic_rig();
    let storage = r.storage();
    r.asr.says("да");
    r.asr.says("да");
    mic(&r).push(speech(3.0), T0);
    mic(&r).push(speech(3.0), T0 + 3.0 + 2.5);
    // when
    r.live.process_chunk(true).unwrap();
    r.live.process_chunk(true).unwrap();
    // then
    let rows = storage.get_current_transcript().unwrap();
    assert_eq!(rows.iter().map(|r| text(r, "text")).collect::<Vec<_>>(), ["да", "да"]);
}

#[test]
fn a_mic_chunk_handling_names_it_after_the_profile_and_marks_it_local() {
    // given
    let r = rig(RigOptions { system: false, legacy_name: "Тестер".into(), ..RigOptions::default() });
    let storage = r.storage();
    let conference = r.conference();
    // when
    r.live.handle_mic_chunk("hello", "12:00:00", T0, 0).unwrap();
    // then
    let rows = storage.get_current_transcript().unwrap();
    assert_eq!(text(&rows[0], "speaker"), "Тестер");
    assert_eq!(text(&rows[0], "speaker_id"), "local");
    assert_eq!(field(&rows[0], "is_local"), &Value::Bool(true));
    assert!(r.live.transcript_lines()[0].contains("**Тестер**"));
    let sent = conference.sent();
    assert!(sent[0].is_local);
    assert_eq!(sent[0].segment_id, 1);
}

#[test]
fn the_same_speaker_a_second_later_handling_a_mic_chunk_continues_the_segment() {
    // given
    let r = mic_rig();
    let storage = r.storage();
    let mut state = r.live.source_state(true).unwrap();
    state.last_speaker = Some("Me".into());
    state.last_speech_time = T0 - 1.0;
    state.segment_start_time = T0 - 5.0;
    r.live.set_source_state(true, state);
    // when
    r.live.handle_mic_chunk("hello", "12:00:00", T0, 0).unwrap();
    // then
    assert_eq!(field(&storage.get_current_transcript().unwrap()[0], "cont"), &Value::Bool(true));
}

#[test]
fn a_pause_longer_than_the_turn_gap_handling_a_mic_chunk_starts_a_new_segment() {
    // given
    let r = mic_rig();
    let storage = r.storage();
    let mut state = r.live.source_state(true).unwrap();
    state.last_speaker = Some("Me".into());
    state.last_speech_time = T0 - 4.0;
    state.segment_start_time = T0 - 4.0;
    r.live.set_source_state(true, state);
    // when
    r.live.handle_mic_chunk("hello", "12:00:00", T0, 0).unwrap();
    // then
    assert_eq!(field(&storage.get_current_transcript().unwrap()[0], "cont"), &Value::Bool(false));
}

#[test]
fn the_local_mic_muted_in_the_call_processing_skips_asr() {
    // given
    let r = mic_rig();
    let conference = r.conference();
    conference.local_muted.store(true, Ordering::SeqCst);
    mic(&r).push(speech(3.0), T0);
    // when
    r.live.process_chunk(true).unwrap();
    // then
    assert_eq!(r.asr.chunk_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_silent_chunk_processing_neither_transcribes_nor_writes_it() {
    // given
    let r = rig(RigOptions {
        system: false,
        options: LiveOptions { save_audio: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    r.storage();
    mic(&r).push(silence(3.0), T0);
    // when
    r.live.process_chunk(true).unwrap();
    // then
    assert_eq!(r.asr.chunk_calls.load(Ordering::SeqCst), 0);
    assert_eq!(r.live.wav_offsets_ms().0, Some(0));
}

#[test]
fn ten_minutes_without_text_checking_the_silence_split_opens_a_new_meeting_and_rotates_the_wav() {
    // given
    // (an orphan meeting: a run with conference signals; a plain local recording is
    // never split)
    let r = rig(RigOptions {
        system: false,
        options: LiveOptions { save_audio: true, use_ktalk: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    let storage = r.storage();
    storage
        .append_segment(&recog_core::meeting_storage::NewSegment {
            speaker: "Me".into(),
            speaker_id: "local".into(),
            text: "раньше".into(),
            ..Default::default()
        })
        .unwrap();
    let old = storage.current_meeting_path().unwrap();
    let mut state = r.live.source_state(true).unwrap();
    state.last_text_time = T0 - 700.0;
    state.last_speaker = Some("Me".into());
    r.live.set_source_state(true, state);
    let chunk = speech(3.0);
    // when
    r.live.check_silence_split(true, T0, Some(&chunk)).unwrap();
    // then
    let new = storage.current_meeting_path().unwrap();
    assert_ne!(new, old);
    let (mic_path, _) = r.live.wav_paths();
    assert_eq!(mic_path.unwrap(), new.join("audio_mic.wav"));
    // The triggering chunk is written again into the new file, without the 0.5 s the
    // capture repeats (continuation: the buffers kept flowing).
    assert_eq!(r.live.wav_offsets_ms().0, Some(2500));
    let state = r.live.source_state(true).unwrap();
    assert_eq!(state.last_speaker, None);
    assert!((state.last_text_time - T0).abs() < 1e-9);
    let old_meta = recog_core::pyjson::loads(&std::fs::read_to_string(old.join("meta.json")).unwrap()).unwrap();
    assert_eq!(text(&old_meta, "end_reason"), "silence_timeout");
}

#[test]
fn a_short_silence_checking_the_silence_split_keeps_the_meeting_and_moves_the_last_text_time() {
    // given
    let r = mic_rig();
    let storage = r.storage();
    let before = storage.current_meeting_path();
    let mut state = r.live.source_state(true).unwrap();
    state.last_text_time = T0 - 10.0;
    r.live.set_source_state(true, state);
    // when
    r.live.check_silence_split(true, T0, None).unwrap();
    // then
    assert_eq!(storage.current_meeting_path(), before);
    assert!((r.live.source_state(true).unwrap().last_text_time - T0).abs() < 1e-9);
}

#[test]
fn no_storage_checking_the_silence_split_changes_nothing() {
    // given
    let r = mic_rig();
    let mut state = r.live.source_state(true).unwrap();
    state.last_text_time = T0 - 700.0;
    r.live.set_source_state(true, state);
    // when
    r.live.check_silence_split(true, T0, None).unwrap();
    // then
    assert!((r.live.source_state(true).unwrap().last_text_time - (T0 - 700.0)).abs() < 1e-9);
}

fn system_rig() -> (Rig, Arc<live_support::FakeConference>) {
    let r = rig(RigOptions {
        mic: false,
        options: LiveOptions { use_ktalk: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    r.storage();
    let conference = r.conference();
    (r, conference)
}

fn system_row(r: &Rig, text_: &str) -> Value {
    r.live.handle_ktalk_chunk(text_, "12:00:00", T0 - 3.0, T0, T0, r.live.generation(), false).unwrap();
    r.rows().last().unwrap().clone()
}

#[test]
fn a_dominant_speaker_handling_system_audio_attributes_to_them_and_not_local() {
    // given
    let (r, conference) = system_rig();
    *conference.dominant.lock().unwrap() = Some("Alice".into());
    // when
    let row = system_row(&r, "добрый день");
    // then
    assert_eq!(text(&row, "speaker"), "Alice");
    assert_eq!(field(&row, "is_local"), &Value::Bool(false));
    assert_eq!(field(&row, "uncertain"), &Value::Bool(false));
    assert_ne!(text(&row, "uncertain_reason"), "local");
    assert!(!conference.sent()[0].is_local);
}

#[test]
fn no_dominant_speaker_handling_system_audio_takes_the_current_one() {
    // given
    let (r, conference) = system_rig();
    *conference.current.lock().unwrap() = Some("Bob".into());
    // when / then
    assert_eq!(text(&system_row(&r, "да"), "speaker"), "Bob");
}

#[test]
fn no_signal_within_the_ttl_handling_system_audio_reuses_the_last_speaker_unless_muted() {
    // given
    let (r, conference) = system_rig();
    let mut state = r.live.source_state(false).unwrap();
    state.last_speaker = Some("Alice".into());
    state.last_speech_time = T0 - 5.0;
    r.live.set_source_state(false, state.clone());
    // when / then
    assert_eq!(text(&system_row(&r, "раз"), "speaker"), "Alice");
    conference.muted.lock().unwrap().insert("Alice".into());
    r.live.set_source_state(false, state);
    assert_eq!(text(&system_row(&r, "два"), "speaker"), "Unknown");
}

#[test]
fn only_one_remote_mic_open_handling_system_audio_takes_them_as_uncertain_and_keeps_no_ttl() {
    // given
    let (r, conference) = system_rig();
    *conference.sole_unmuted.lock().unwrap() = Some("Carol".into());
    // when
    let row = system_row(&r, "вопрос");
    // then
    assert_eq!(text(&row, "speaker"), "Carol");
    assert_eq!(field(&row, "uncertain"), &Value::Bool(true));
    assert_eq!(text(&row, "uncertain_reason"), "sole_unmuted_fallback");
    assert_eq!(r.live.source_state(false).unwrap().last_speaker, None);
}

#[test]
fn nobody_remote_in_the_call_handling_system_audio_names_the_system_audio() {
    // given
    let (r, conference) = system_rig();
    conference.has_remote.store(false, Ordering::SeqCst);
    // when
    let row = system_row(&r, "подкаст");
    // then
    assert_eq!(text(&row, "speaker"), "Системный звук");
    assert_eq!(field(&row, "uncertain"), &Value::Bool(false));
    assert_eq!(r.live.source_state(false).unwrap().last_speaker, None);
}

#[test]
fn no_attribution_at_all_handling_system_audio_writes_unknown_with_no_speaker() {
    // given
    let (r, _conference) = system_rig();
    // when
    let row = system_row(&r, "кто это");
    // then
    assert_eq!(text(&row, "speaker"), "Unknown");
    assert_eq!(text(&row, "uncertain_reason"), "no_speaker");
}

#[test]
fn every_mic_muted_handling_system_audio_marks_mics_off() {
    // given
    let (r, conference) = system_rig();
    *conference.dominant.lock().unwrap() = Some("Alice".into());
    conference.all_muted.store(true, Ordering::SeqCst);
    // when / then
    assert_eq!(text(&system_row(&r, "тихо"), "uncertain_reason"), "mics_off");
}

#[test]
fn the_mixed_fallback_handling_system_audio_tags_the_persisted_reason_only() {
    // given
    let (r, conference) = system_rig();
    *conference.dominant.lock().unwrap() = Some("Alice".into());
    // when
    r.live.handle_ktalk_chunk("раз", "12:00:00", T0 - 3.0, T0, T0, 0, true).unwrap();
    *conference.dominant.lock().unwrap() = None;
    r.live.handle_ktalk_chunk("два", "12:00:03", T0, T0 + 3.0, T0 + 30.0, 0, true).unwrap();
    // then
    let rows = r.rows();
    assert_eq!(text(&rows[0], "uncertain_reason"), "fallback_mixed");
    assert_eq!(text(&rows[1], "uncertain_reason"), "fallback_mixed/no_speaker");
    assert_eq!(conference.sent()[1].uncertain_reason, "no_speaker");
}

#[test]
fn unknown_rows_then_a_speaker_handling_system_audio_corrects_the_run_backwards() {
    // given
    let (r, conference) = system_rig();
    r.live.handle_ktalk_chunk("один", "12:00:00", T0 - 6.0, T0 - 3.0, T0 - 3.0, 0, false).unwrap();
    r.live.handle_ktalk_chunk("два", "12:00:03", T0 - 3.0, T0, T0, 0, false).unwrap();
    *conference.dominant.lock().unwrap() = Some("Alice".into());
    // when
    r.live.handle_ktalk_chunk("три", "12:00:06", T0, T0 + 3.0, T0 + 3.0, 0, false).unwrap();
    // then
    let rows = r.rows();
    assert!(rows.iter().all(|row| text(row, "speaker") == "Alice"), "{rows:?}");
    assert!(rows.iter().all(|row| field(row, "uncertain") == &Value::Bool(false)));
    let corrections: Vec<_> = conference.sent().into_iter().filter(|l| l.correction).collect();
    assert_eq!(corrections.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["два", "один"]);
    assert!(r.live.transcript_lines().iter().all(|l| !l.contains("**Unknown**")));
}

#[test]
fn an_unknown_run_older_than_a_minute_handling_system_audio_leaves_it() {
    // given
    let (r, conference) = system_rig();
    r.live.handle_ktalk_chunk("давно", "11:58:00", T0 - 120.0, T0 - 117.0, T0 - 117.0, 0, false).unwrap();
    r.live.handle_ktalk_chunk("недавно", "11:59:57", T0 - 6.0, T0 - 3.0, T0 - 3.0, 0, false).unwrap();
    *conference.dominant.lock().unwrap() = Some("Alice".into());
    // when
    r.live.handle_ktalk_chunk("сейчас", "12:00:00", T0 - 3.0, T0, T0, 0, false).unwrap();
    // then
    let speakers: Vec<String> = r.rows().iter().map(|row| text(row, "speaker")).collect();
    assert_eq!(speakers, ["Unknown", "Alice", "Alice"]);
}

#[test]
fn no_conference_processing_system_audio_writes_an_unattributed_speaker() {
    // given
    let r = rig(RigOptions { mic: false, ..RigOptions::default() });
    let storage = r.storage();
    r.asr.says("что-то играет");
    system(&r).push(speech(3.0), T0);
    // when
    r.live.process_chunk(false).unwrap();
    // then
    let row = &storage.get_current_transcript().unwrap()[0];
    assert_eq!(text(row, "speaker"), "speaker");
    assert_eq!(text(row, "uncertain_reason"), "no_conference_identity");
    assert_eq!(field(row, "uncertain"), &Value::Bool(true));
}

#[test]
fn a_ktalk_run_without_pcm_processing_system_audio_asks_for_the_dominant_speaker() {
    // given
    let (r, conference) = system_rig();
    r.asr.says("слово");
    system(&r).push(speech(3.0), T0);
    // when
    r.live.process_chunk(false).unwrap();
    // then
    assert_eq!(conference.dominant_queries.load(Ordering::SeqCst), 1);
}

#[test]
fn asr_failing_transcribing_directly_logs_once_and_gives_nothing() {
    // given
    let r = mic_rig();
    r.asr.fails(AsrError::Failed("boom".into()));
    // when
    let text_ = r.live.transcribe_direct(&speech(3.0)).unwrap();
    // then
    assert_eq!(text_, None);
    assert_eq!(r.stderr.lines(), ["[LiveTranscriber] _transcribe_direct failed: RuntimeError: boom"]);
}

#[test]
fn storage_that_cannot_start_setting_it_up_aborts_with_the_fatal_line() {
    // given
    let r = mic_rig();
    std::fs::create_dir_all(r.data_dir()).unwrap();
    std::fs::write(r.data_dir().join("meetings"), "a file").unwrap();
    let mut options = recog_core::meeting_storage::StorageOptions::new(r.dir.path());
    options.log = Arc::new(|_, _| {});
    // when
    let result = recog_live::live::session::setup_meeting_storage(&r.live, options);
    // then
    assert!(result.is_err());
    assert!(r.stderr.lines()[0].starts_with("[FATAL ERROR] MeetingStorage init failed: "), "{:?}", r.stderr.lines());
}

#[test]
fn nothing_to_capture_building_the_transcriber_fails() {
    let dir = tempfile::tempdir().unwrap();
    let clock = recog_live::clock::ManualClock::new(T0);
    let env = recog_live::live::LiveEnv {
        data_dir: dir.path().to_path_buf(),
        dedup_max_gap_s: 2.0,
        legacy_mic_name: "Me".into(),
        clock: clock.shared(),
        zone: Arc::new(live_support::Utc),
        log: recog_live::log::Logger::capture(clock.shared()).0,
        out: Arc::new(|_: &str| {}),
    };
    let err = recog_live::live::Live::new(LiveOptions::default(), env, live_support::FakeAsr::new(), None, None)
        .err()
        .unwrap();
    assert!(err.starts_with("нечего захватывать"), "{err}");
}

#[test]
fn each_backend_the_system_audio_label_names_it() {
    for (spec, label) in [
        ("screencapture", "ScreenCaptureKit"),
        ("pulse", "PulseAudio/PipeWire (parec)"),
        ("device:3", "3"),
        ("none", "none"),
    ] {
        let r = rig(RigOptions {
            options: LiveOptions { system_audio: spec.into(), ..LiveOptions::default() },
            ..RigOptions::default()
        });
        assert_eq!(r.live.system_audio_label(), label);
    }
}

#[test]
fn chunk_end_time_processing_a_mic_chunk_prints_the_line_and_stamps_hh_mm_ss() {
    // given
    let r = mic_rig();
    r.storage();
    r.asr.says("строка");
    mic(&r).push(speech(3.0), 1_790_000_000.0);
    // when
    r.live.process_chunk(true).unwrap();
    // then: 1790000003 is 2026-09-21 14:13:23 UTC.
    assert!(r.stdout().ends_with("\n**Me** *[14:13:23]*: строка\n"), "{}", r.stdout());
}
