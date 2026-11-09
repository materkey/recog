//! The per-speaker path of the main loop (`_on_pcm_batch`, the segment registry, the
//! PCM-primary gate of system audio, the watchdog markers, refine submission and
//! `_apply_refine`), ported from tests/test_live_speaker_attribution.py,
//! test_live_segment_registry.py, test_refine_alignment.py,
//! test_refine_integration.py and test_per_speaker_refine_e2e.py.

mod live_support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use live_support::{FakeConference, Rig, RigOptions, T0, field, rig, silence, speech, text};
use recog_core::pyjson::Value;
use recog_live::asr::{AsrError, Word};
use recog_live::live::LiveOptions;
use recog_live::per_speaker::{BatchedChunk, IngestOptions, PcmChunk, PerSpeakerIngest};
use recog_live::refine::{RefineOptions, RefineResult, RefineTask, RefineWorker, new_model_lock};

fn ktalk_rig(save_audio: bool) -> (Rig, Arc<FakeConference>) {
    let r = rig(RigOptions {
        options: LiveOptions { use_ktalk: true, save_audio, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    r.storage();
    let conference = r.conference();
    (r, conference)
}

fn ingest(r: &Rig) -> Arc<PerSpeakerIngest> {
    let ingest = Arc::new(PerSpeakerIngest::new(
        Arc::new(|_| Ok(())),
        IngestOptions {
            clock: r.clock.shared(),
            meeting_active_getter: Some(Arc::new(|| true)),
            logger: recog_live::log::Logger::capture(r.clock.shared()).0,
            ..IngestOptions::default()
        },
    ));
    r.live.set_ingest(Some(ingest.clone()));
    ingest
}

fn refine_worker(r: &Rig) -> Arc<RefineWorker> {
    let worker = Arc::new(RefineWorker::new(
        new_model_lock(),
        Arc::new(|_, _| Err(AsrError::Failed("unused".into()))),
        Arc::new(|_| Ok(())),
        RefineOptions { transcribe_manages_lock: true, ..RefineOptions::default() },
    ));
    r.live.set_refine_worker(Some(worker.clone()));
    worker
}

fn loud(n: usize) -> Vec<i16> {
    (0..n).map(|i| if i % 2 == 0 { 3000 } else { -3000 }).collect()
}

fn chunk(pid: &str, name: &str, start_ms: f64, end_ms: f64, pid_ms: (f64, f64)) -> BatchedChunk {
    BatchedChunk {
        pid: pid.into(),
        speaker_name: name.into(),
        is_local: false,
        pcm: loud(16_000),
        chunk_start_ms: start_ms,
        chunk_end_ms: end_ms,
        pid_audio_ms_start: pid_ms.0,
        pid_audio_ms_end: pid_ms.1,
    }
}

fn now_ms(r: &Rig) -> f64 {
    r.clock.wall_ms()
}

trait WallMs {
    fn wall_ms(&self) -> f64;
}

impl WallMs for recog_live::clock::ManualClock {
    fn wall_ms(&self) -> f64 {
        use recog_live::clock::Clock as _;
        self.wall() * 1000.0
    }
}

#[test]
fn a_batch_transcribing_attributes_by_the_chunk_and_never_asks_the_bridge() {
    // given
    let (r, conference) = ktalk_rig(false);
    ingest(&r);
    r.asr.batch(&["привет", "пока"]);
    let now = now_ms(&r);
    let batch = [
        chunk("pid-alice", "Alice", now - 3000.0, now, (0.0, 3000.0)),
        chunk("pid-bob", "Bob", now - 3000.0, now, (0.0, 3000.0)),
    ];
    // when
    r.live.on_pcm_batch(&batch).unwrap();
    // then
    let rows = r.rows();
    assert_eq!(rows.iter().map(|r| text(r, "speaker")).collect::<Vec<_>>(), ["Alice", "Bob"]);
    assert_eq!(rows.iter().map(|r| text(r, "speaker_id")).collect::<Vec<_>>(), ["pid-alice", "pid-bob"]);
    assert_eq!(conference.dominant_queries.load(Ordering::SeqCst), 0);
    let sent = conference.sent();
    assert_eq!(sent[0].source_id, "pid-alice");
    assert_eq!(sent[1].segment_id, 2);
}

#[test]
fn a_name_with_newlines_or_none_transcribing_sanitizes_it_or_names_the_pid() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    r.asr.batch(&["раз", "два"]);
    let now = now_ms(&r);
    let batch = [
        chunk("pid-1", "Evil\nName\r", now - 3000.0, now, (0.0, 3000.0)),
        chunk("abcdefghij", "", now - 3000.0, now, (0.0, 3000.0)),
    ];
    // when
    r.live.on_pcm_batch(&batch).unwrap();
    // then
    let speakers: Vec<String> = r.rows().iter().map(|r| text(r, "speaker")).collect();
    assert_eq!(speakers, ["Evil Name", "Участник abcdef"]);
}

#[test]
fn silent_and_speaking_participants_transcribing_sends_only_the_speech_to_asr() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    let now = now_ms(&r);
    let mut quiet = chunk("pid-q", "Quiet", now - 3000.0, now, (0.0, 3000.0));
    quiet.pcm = vec![0; 16_000];
    let batch = [quiet, chunk("pid-s", "Speaker", now - 3000.0, now, (0.0, 3000.0))];
    // when
    r.live.on_pcm_batch(&batch).unwrap();
    // then
    assert_eq!(*r.asr.batch_calls.lock().unwrap(), [1]);
    assert_eq!(text(&r.rows()[0], "speaker"), "Speaker");
}

#[test]
fn a_batch_of_silence_transcribing_does_not_call_asr() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    let now = now_ms(&r);
    let mut quiet = chunk("pid-q", "Quiet", now - 3000.0, now, (0.0, 3000.0));
    quiet.pcm = vec![10; 16_000];
    // when
    r.live.on_pcm_batch(&[quiet]).unwrap();
    // then
    assert!(r.asr.batch_calls.lock().unwrap().is_empty());
}

#[test]
fn fewer_texts_than_clips_transcribing_drops_the_batch_loudly() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    r.asr.batch(&["только один"]);
    let now = now_ms(&r);
    let batch = [chunk("a", "A", now, now, (0.0, 1.0)), chunk("b", "B", now, now, (0.0, 1.0))];
    // when
    r.live.on_pcm_batch(&batch).unwrap();
    // then
    assert!(r.rows().is_empty());
    assert_eq!(
        r.stderr.lines(),
        ["[PerSpeaker] transcribe_batch returned 1 text(s) for 2 input(s) — dropping batch to avoid misattribution"]
    );
}

#[test]
fn asr_unavailable_transcribing_a_batch_hands_it_back_for_the_retry() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    r.asr.batch_answers.lock().unwrap().push_back(Err(AsrError::TemporarilyUnavailable));
    let now = now_ms(&r);
    // when
    let result = r.live.on_pcm_batch(&[chunk("a", "A", now, now, (0.0, 1.0))]);
    // then
    assert_eq!(result, Err(AsrError::TemporarilyUnavailable));
}

#[test]
fn a_meeting_switch_after_the_drain_transcribing_drops_the_batch() {
    // given
    let (r, _conference) = ktalk_rig(false);
    let ingest = ingest(&r);
    ingest.enqueue(PcmChunk {
        pid: "a".into(),
        speaker_name: "A".into(),
        is_local: false,
        seq: 0,
        chunk_start_ts: T0,
        chunk_end_ts: T0 + 1.0,
        sample_rate: 16_000,
        pcm: loud(16_000).iter().flat_map(|v| v.to_le_bytes()).collect(),
    });
    ingest.tick(Some(T0 + 1.0));
    assert_eq!(ingest.last_drain_generation(), Some(0));
    r.live.invalidate_pending_chunks();
    let now = now_ms(&r);
    // when
    r.live.on_pcm_batch(&[chunk("a", "A", now, now, (0.0, 1.0))]).unwrap();
    // then
    assert!(r.asr.batch_calls.lock().unwrap().is_empty());
    assert!(r.rows().is_empty());
}

#[test]
fn a_late_batch_transcribing_stamps_its_rows_with_the_speech_time() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    let spoken_end = (T0 - 120.0) * 1000.0;
    // when
    r.live.on_pcm_batch(&[chunk("a", "A", spoken_end - 3000.0, spoken_end, (0.0, 3000.0))]).unwrap();
    // then
    let ts = recog_core::time::fromisoformat(&text(&r.rows()[0], "ts")).unwrap();
    assert_eq!(ts.utc_micros(), Some(((T0 - 120.0) * 1e6) as i64));
}

#[test]
fn a_fresh_anchor_transcribing_extrapolates_the_offset_from_it_exactly() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    let now = now_ms(&r);
    r.live.set_system_wav_anchor(Some((now - 10_000.0, 50_000)));
    r.asr.batch(&["a", "b"]);
    let batch = [chunk("a", "A", now - 9000.0, now, (0.0, 1.0)), chunk("b", "B", now - 6000.0, now, (0.0, 1.0))];
    // when
    r.live.on_pcm_batch(&batch).unwrap();
    // then
    let offsets: Vec<Value> = r.rows().iter().map(|r| field(r, "audio_offset_ms").clone()).collect();
    assert_eq!(offsets, [Value::Int(51_000), Value::Int(54_000)]);
}

#[test]
fn a_stale_or_missing_anchor_transcribing_falls_back_to_the_writer_minus_the_age() {
    // given
    let (r, _conference) = ktalk_rig(true);
    ingest(&r);
    r.system.as_ref().unwrap().push(speech(3.0), T0 - 3.0);
    r.live.process_chunk(false).unwrap();
    let now = now_ms(&r);
    r.live.set_system_wav_anchor(Some((now - 40_000.0, 0)));
    // when
    r.live.on_pcm_batch(&[chunk("a", "A", now - 1000.0, now, (0.0, 1.0))]).unwrap();
    r.live.set_system_wav_anchor(None);
    r.live.on_pcm_batch(&[chunk("b", "B", now - 10_000.0, now, (0.0, 1.0))]).unwrap();
    // then: the system WAV holds 3000 ms.
    let offsets: Vec<Value> = r.rows().iter().map(|r| field(r, "audio_offset_ms").clone()).collect();
    assert_eq!(offsets, [Value::Int(2000), Value::Int(0)]);
}

#[test]
fn an_anchor_after_the_chunk_transcribing_clamps_the_offset_to_zero() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    let now = now_ms(&r);
    r.live.set_system_wav_anchor(Some((now, 100)));
    // when
    r.live.on_pcm_batch(&[chunk("a", "A", now - 5000.0, now, (0.0, 1.0))]).unwrap();
    // then
    assert_eq!(field(&r.rows()[0], "audio_offset_ms"), &Value::Int(0));
}

#[test]
fn chunks_of_one_pid_registering_live_segments_keeps_their_own_bounds_in_order() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    r.asr.batch(&["раз", "два"]);
    let now = now_ms(&r);
    let batch = [chunk("a", "A", now, now, (0.0, 3000.0)), chunk("a", "A", now, now, (3000.0, 6000.0))];
    // when
    r.live.on_pcm_batch(&batch).unwrap();
    // then
    assert_eq!(r.live.pcm_live_segments("a"), [(1, 0.0, 3000.0), (2, 3000.0, 6000.0)]);
    assert_eq!(r.live.pcm_segment_text(2).as_deref(), Some("два"));
}

#[test]
fn an_entry_past_the_window_registering_prunes_it_with_its_text() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    r.live.register_pcm_live_segment("a", 1, 0.0, 1000.0);
    r.live.register_pcm_live_segment("a", 2, 1000.0, 2000.0);
    r.live.register_pcm_live_segment("b", 3, 0.0, 1000.0);
    // when: 1000 < 27000 - 25000 = 2000 prunes the first entry only.
    r.live.register_pcm_live_segment("a", 4, 26_000.0, 27_000.0);
    // then
    assert_eq!(r.live.pcm_live_segments("a"), [(2, 1000.0, 2000.0), (4, 26_000.0, 27_000.0)]);
    assert_eq!(r.live.pcm_live_segments("b"), [(3, 0.0, 1000.0)]);
}

#[test]
fn no_ingest_registering_a_live_segment_does_nothing() {
    let (r, _conference) = ktalk_rig(false);
    r.live.register_pcm_live_segment("a", 1, 0.0, 1000.0);
    assert!(r.live.pcm_live_segments("a").is_empty());
}

fn system_chunk(r: &Rig, samples: Vec<f32>) {
    r.system.as_ref().unwrap().push(samples, T0);
    r.live.process_chunk(false).unwrap();
}

fn feed_pcm(ingest: &PerSpeakerIngest) {
    ingest.enqueue(PcmChunk {
        pid: "a".into(),
        speaker_name: "A".into(),
        is_local: false,
        seq: 0,
        chunk_start_ts: T0,
        chunk_end_ts: T0 + 1.0,
        sample_rate: 16_000,
        pcm: loud(1600).iter().flat_map(|v| v.to_le_bytes()).collect(),
    });
}

#[test]
fn healthy_pcm_processing_system_audio_skips_its_asr_but_writes_even_silence() {
    // given
    let (r, _conference) = ktalk_rig(true);
    let ingest = ingest(&r);
    feed_pcm(&ingest);
    // when
    system_chunk(&r, speech(3.0));
    system_chunk(&r, silence(3.0));
    // then
    assert_eq!(r.asr.chunk_calls.load(Ordering::SeqCst), 0);
    assert_eq!(r.live.wav_offsets_ms().1, Some(5500));
    assert!(r.live.system_wav_anchor().is_some());
}

#[test]
fn no_pcm_yet_processing_system_audio_runs_the_mixed_path() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    r.asr.says("до первого чанка");
    // when
    system_chunk(&r, speech(3.0));
    // then
    assert_eq!(r.asr.chunk_calls.load(Ordering::SeqCst), 1);
    assert_eq!(text(&r.rows()[0], "uncertain_reason"), "no_speaker");
}

#[test]
fn the_watchdog_tripped_processing_system_audio_marks_once_pauses_refine_and_tags_rows() {
    // given
    let (r, conference) = ktalk_rig(false);
    let ingest = ingest(&r);
    let worker = refine_worker(&r);
    *conference.dominant.lock().unwrap() = Some("Alice".into());
    feed_pcm(&ingest);
    ingest.tick(Some(T0 + 100.0));
    assert!(ingest.degraded());
    r.asr.says("раз");
    r.asr.says("два");
    // when
    system_chunk(&r, speech(3.0));
    r.system.as_ref().unwrap().push(speech(3.0), T0 + 10.0);
    r.live.process_chunk(false).unwrap();
    // then
    let rows = r.rows();
    let markers: Vec<String> =
        rows.iter().filter(|r| text(r, "speaker_id") == "__system__").map(|r| text(r, "text")).collect();
    assert_eq!(markers, ["[PCM silence >30s — switched to mixed fallback]"]);
    assert!(worker.is_paused());
    let reasons: Vec<String> =
        rows.iter().filter(|r| text(r, "speaker") == "Alice").map(|r| text(r, "uncertain_reason")).collect();
    assert_eq!(reasons, ["fallback_mixed", "fallback_mixed"]);
    // and when PCM comes back
    feed_pcm(&ingest);
    system_chunk(&r, speech(3.0));
    let markers: Vec<String> =
        r.rows().iter().filter(|r| text(r, "speaker_id") == "__system__").map(|r| text(r, "text")).collect();
    assert_eq!(markers.last().unwrap(), "[PCM recovered — back to per-speaker]");
    assert!(!worker.is_paused());
    assert!(r.stdout().contains("\n*[PCM recovered — back to per-speaker]* *["));
}

#[test]
fn a_meeting_switch_since_the_flip_emitting_the_marker_neither_writes_nor_pauses() {
    // given
    let (r, _conference) = ktalk_rig(false);
    let worker = refine_worker(&r);
    let stale = r.live.generation();
    r.live.invalidate_pending_chunks();
    // when
    r.live.emit_pcm_transition_marker(true, stale).unwrap();
    // then
    assert!(r.rows().is_empty());
    assert!(!worker.is_paused());
    assert!(r.live.transcript_lines()[0].contains("[PCM silence >30s — switched to mixed fallback]"));
}

#[test]
fn the_current_generation_emitting_the_marker_writes_it_and_pauses_then_resumes() {
    // given
    let (r, _conference) = ktalk_rig(false);
    let worker = refine_worker(&r);
    // when
    r.live.emit_pcm_transition_marker(true, r.live.generation()).unwrap();
    // then
    assert!(worker.is_paused());
    let row = &r.rows()[0];
    assert_eq!(text(row, "speaker"), "Система");
    assert_eq!(text(row, "uncertain_reason"), "pcm_watchdog_transition");
    r.live.emit_pcm_transition_marker(false, r.live.generation()).unwrap();
    assert!(!worker.is_paused());
}

#[test]
fn a_meeting_boundary_invalidating_drains_the_ingest_clears_buffers_and_resumes_refine() {
    // given
    let (r, _conference) = ktalk_rig(false);
    let ingest = ingest(&r);
    let worker = refine_worker(&r);
    worker.pause();
    feed_pcm(&ingest);
    r.live.set_last_pcm_degraded(true);
    // when
    r.live.invalidate_pending_chunks();
    // then
    assert!(ingest.buffered().is_empty());
    assert!(!ingest.pcm_ever_seen());
    assert!(!worker.is_paused());
    assert!(!r.live.last_pcm_degraded());
    assert_eq!(r.mic.as_ref().unwrap().clears.load(Ordering::SeqCst), 1);
    assert_eq!(r.system.as_ref().unwrap().clears.load(Ordering::SeqCst), 1);
}

#[test]
fn ktalk_off_or_on_building_the_transcriber_sets_the_pcm_transport() {
    let off = rig(RigOptions::default());
    assert!(!off.live.use_pcm_transport());
    let on =
        rig(RigOptions { options: LiveOptions { use_ktalk: true, ..LiveOptions::default() }, ..RigOptions::default() });
    assert!(on.live.use_pcm_transport());
    let only = rig(RigOptions {
        options: LiveOptions { ktalk_only: true, ..LiveOptions::default() },
        ..RigOptions::default()
    });
    assert!(only.live.use_ktalk() && only.live.use_pcm_transport());
}

#[test]
fn live_asr_running_transcribing_directly_holds_refine_back_until_it_returns() {
    // given
    let (r, _conference) = ktalk_rig(false);
    let worker = refine_worker(&r);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (w, s) = (worker.clone(), seen.clone());
    *r.asr.on_call.lock().unwrap() = Some(Box::new(move || s.lock().unwrap().push(w.live_pending())));
    // when
    r.live.transcribe_direct(&speech(3.0)).unwrap();
    // then
    assert_eq!(*seen.lock().unwrap(), [true]);
    assert!(!worker.live_pending());
}

fn task(r: &Rig, pid: &str, base: f64) -> RefineTask {
    RefineTask {
        pid: pid.into(),
        pcm_samples: loud(16_000),
        offset_base_ms: base,
        submitted_at: T0,
        meeting_generation: r.live.generation(),
    }
}

#[test]
fn a_window_without_live_segments_submitting_skips_it_and_one_with_reaches_the_worker() {
    // given
    let (r, _conference) = ktalk_rig(false);
    ingest(&r);
    let worker = refine_worker(&r);
    r.live.register_pcm_live_segment("a", 1, 0.0, 3000.0);
    // when
    r.live.submit_refine(task(&r, "b", 0.0));
    r.live.submit_refine(task(&r, "a", 50_000.0));
    r.live.submit_refine(task(&r, "a", 0.0));
    // then
    let queued = worker.queued_tasks();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].pid, "a");
    assert!((queued[0].offset_base_ms - 0.0).abs() < 1e-9);
}

fn word(text: &str, start: i64, end: i64) -> Word {
    Word { text: text.into(), start, end }
}

fn refine_rig() -> (Rig, Arc<FakeConference>) {
    let (r, conference) = ktalk_rig(false);
    ingest(&r);
    refine_worker(&r);
    r.asr.batch(&["раз два", "три четыре"]);
    let now = now_ms(&r);
    let batch = [chunk("a", "A", now, now, (0.0, 3000.0)), chunk("a", "A", now, now, (3000.0, 6000.0))];
    r.live.on_pcm_batch(&batch).unwrap();
    (r, conference)
}

fn result(r: &Rig, base: f64, words: Vec<Word>) -> RefineResult {
    RefineResult { task: Arc::new(task(r, "a", base)), text: String::new(), words }
}

#[test]
fn refined_words_applying_align_by_their_start_even_across_a_boundary() {
    // given
    let (r, conference) = refine_rig();
    // a word spanning 2900-3100 starts in the first segment
    let words = vec![word("Раз", 100, 500), word("Два", 2900, 3100), word("Три", 3200, 3600), word(" ", 4000, 4100)];
    // when
    r.live.apply_refine(&result(&r, 0.0, words)).unwrap();
    // then
    let rows = r.rows();
    assert_eq!(text(&rows[0], "text"), "Раз Два");
    assert_eq!(field(&rows[0], "refined"), &Value::Bool(true));
    assert_eq!(text(&rows[1], "text"), "Три");
    let refines = conference.refines.lock().unwrap().clone();
    let meeting = r.storage().current_meeting_id();
    assert_eq!(refines[0].1, meeting);
    assert_eq!(refines[0].0.len(), 2);
    assert_eq!(r.live.pcm_segment_text(1).as_deref(), Some("Раз Два"));
}

#[test]
fn the_same_text_applying_a_refine_rewrites_nothing() {
    // given
    let (r, conference) = refine_rig();
    // when
    r.live.apply_refine(&result(&r, 0.0, vec![word("раз", 0, 100), word("два", 200, 300)])).unwrap();
    // then
    assert_eq!(field(&r.rows()[0], "refined"), &Value::Bool(false));
    assert!(conference.refines.lock().unwrap().is_empty());
}

#[test]
fn a_window_offset_applying_rebases_the_words_into_the_pid_frame() {
    // given
    let (r, _conference) = refine_rig();
    // when: the window starts at 2500 ms of the pid's audio
    r.live.apply_refine(&result(&r, 2500.0, vec![word("Вперёд", 1000, 1200)])).unwrap();
    // then
    let rows = r.rows();
    assert_eq!(field(&rows[0], "refined"), &Value::Bool(false));
    assert_eq!(text(&rows[1], "text"), "Вперёд");
}

#[test]
fn an_old_meeting_or_a_paused_worker_applying_a_refine_drops_it() {
    // given
    let (r, _conference) = refine_rig();
    let old = result(&r, 0.0, vec![word("Старое", 0, 100)]);
    r.live.refine_worker().unwrap().pause();
    // when
    r.live.apply_refine(&old).unwrap();
    // then
    assert_eq!(field(&r.rows()[0], "refined"), &Value::Bool(false));
    r.live.refine_worker().unwrap().resume();
    r.live.invalidate_pending_chunks();
    r.live.apply_refine(&old).unwrap();
    assert!(r.live.pcm_segment_text(1).is_none());
}

#[test]
fn a_refine_worker_end_to_end_running_rewrites_the_row_through_the_ingest() {
    // given: the transcriber wired as the session wires it, a fake model
    let r =
        rig(RigOptions { options: LiveOptions { use_ktalk: true, ..LiveOptions::default() }, ..RigOptions::default() });
    r.storage();
    r.conference();
    *r.asr.refine_answer.lock().unwrap() = recog_live::asr::Transcription {
        text: "Привет Мир".into(),
        words: vec![word("Привет", 100, 400), word("Мир", 500, 800)],
    };
    r.asr.batch(&["привет мир"]);
    recog_live::live::session::setup_per_speaker_ingest(&r.live);
    let ingest = r.live.ingest().unwrap();
    // when: one chunk, then silence until the gap trigger
    use recog_live::clock::Clock as _;
    let wall = r.clock.wall();
    ingest.enqueue(PcmChunk {
        pid: "a".into(),
        speaker_name: "Alice".into(),
        is_local: false,
        seq: 0,
        chunk_start_ts: wall - 1.0,
        chunk_end_ts: wall,
        sample_rate: 16_000,
        pcm: loud(16_000).iter().flat_map(|v| v.to_le_bytes()).collect(),
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline
        && !r.rows().first().is_some_and(|row| field(row, "refined") == &Value::Bool(true))
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    // then
    let rows = r.rows();
    assert_eq!(text(&rows[0], "text"), "Привет Мир", "{rows:?}");
    assert_eq!(r.asr.refine_calls.load(Ordering::SeqCst), 1);
    r.live.stop_audio();
}
