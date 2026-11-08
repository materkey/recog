//! The cases of tests/test_per_speaker.py against the Rust ingest.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use recog_live::asr::AsrError;
use recog_live::clock::ManualClock;
use recog_live::config::{
    PCM_BATCH_INTERVAL_MS, PCM_CHUNK_DURATION_S, PCM_CHUNK_OVERLAP_MS, PCM_CHUNK_PAYLOAD_S, PCM_REFINE_MONOLOG_CAP_S,
    PCM_REFINE_WINDOW_S, PCM_SILENCE_WATCHDOG_S, SPEAKER_TURN_GAP,
};
use recog_live::log::{Captured, Logger};
use recog_live::per_speaker::{BatchedChunk, IngestOptions, PcmChunk, PerSpeakerIngest, TranscribeCallback};

const SR: u32 = 16_000;

fn chunk(pid: &str, seq: i64) -> PcmChunk {
    PcmChunk {
        pid: pid.into(),
        speaker_name: "Alice".into(),
        is_local: false,
        seq,
        chunk_start_ts: 1000.0,
        chunk_end_ts: 1003.3,
        sample_rate: SR,
        pcm: vec![0, 0, 1, 0],
    }
}

fn timed(pid: &str, seq: i64, start: f64, end: f64) -> PcmChunk {
    PcmChunk { chunk_start_ts: start, chunk_end_ts: end, ..chunk(pid, seq) }
}

fn second(pid: &str, seq: i64) -> PcmChunk {
    PcmChunk { pcm: vec![0; 2 * SR as usize], ..chunk(pid, seq) }
}

type Received = Arc<Mutex<Vec<Vec<BatchedChunk>>>>;

fn collector() -> (TranscribeCallback, Received) {
    let received: Received = Arc::default();
    let sink = received.clone();
    (
        Arc::new(move |batch: &[BatchedChunk]| {
            sink.lock().unwrap().push(batch.to_vec());
            Ok(())
        }),
        received,
    )
}

fn quiet() -> (Logger, Captured) {
    Logger::capture(ManualClock::new(0.0).shared())
}

fn options(max_batch_size: usize, max_chunks_per_pid: usize) -> IngestOptions {
    IngestOptions { max_batch_size, max_chunks_per_pid, logger: quiet().0, ..IngestOptions::default() }
}

fn with_clock(clock: &ManualClock) -> IngestOptions {
    IngestOptions { clock: clock.shared(), ..options(1000, 1000) }
}

/// `_Clock(start=100.0)`: wall 100 at monotonic 0.
fn clock100() -> ManualClock {
    ManualClock::new(100.0)
}

fn now(clock: &ManualClock) -> Option<f64> {
    use recog_live::clock::Clock as _;
    Some(clock.wall())
}

fn wait_for(received: &Received, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while received.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    !received.lock().unwrap().is_empty()
}

fn advance_silence_past_turn_gap(ingest: &PerSpeakerIngest, clock: &ManualClock) {
    let turn_gap_ms = SPEAKER_TURN_GAP * 1000.0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let ticks = (turn_gap_ms / PCM_BATCH_INTERVAL_MS as f64).floor() as usize + 2;
    for _ in 0..ticks {
        ingest.tick(now(clock));
    }
}

#[test]
fn five_chunks_over_a_capacity_of_three_enqueued_then_the_three_latest_seqs_remain() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, options(100, 3));
    for s in 1..=5 {
        #[allow(clippy::cast_precision_loss)]
        ingest.enqueue(timed("p1", s, s as f64, s as f64 + 0.1));
    }
    ingest.tick(Some(0.0));
    let batches = received.lock().unwrap();
    assert_eq!(batches.len(), 1);
    #[allow(clippy::cast_possible_truncation)]
    let seqs: Vec<i64> = batches[0].iter().map(|b| (b.chunk_start_ms / 1000.0) as i64).collect();
    assert_eq!(seqs, vec![3, 4, 5]);
    assert!(batches[0].iter().all(|b| b.pid == "p1"));
}

#[test]
fn max_batch_size_chunks_enqueued_when_ticking_then_one_full_batch_is_dispatched() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, options(4, 100));
    for s in 1..=4 {
        ingest.enqueue(chunk("p1", s));
    }
    ingest.tick(Some(0.0));
    let batches = received.lock().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 4);
}

#[test]
fn worker_running_with_a_long_interval_when_a_full_batch_arrives_then_the_wake_dispatches_it() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, IngestOptions { batch_interval_ms: 2000, ..options(3, 100) });
    ingest.start();
    for s in 1..=3 {
        ingest.enqueue(chunk("p1", s));
    }
    assert!(wait_for(&received, Duration::from_secs(1)), "batch should have fired via the wake event");
    assert_eq!(received.lock().unwrap()[0].len(), 3);
    ingest.stop(Duration::from_secs(2));
}

#[test]
fn worker_running_with_one_chunk_when_the_interval_passes_then_it_is_dispatched() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, IngestOptions { batch_interval_ms: 50, ..options(100, 100) });
    ingest.start();
    ingest.enqueue(chunk("p1", 1));
    assert!(wait_for(&received, Duration::from_secs(1)), "batch should have fired on timeout");
    assert_eq!(received.lock().unwrap()[0].len(), 1);
    ingest.stop(Duration::from_secs(2));
}

#[test]
fn chunks_out_of_seq_order_when_drained_then_they_come_in_seq_order() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, options(100, 100));
    ingest.enqueue(timed("p1", 3, 3.0, 3.1));
    ingest.enqueue(timed("p1", 1, 1.0, 1.1));
    ingest.enqueue(timed("p1", 2, 2.0, 2.1));
    ingest.tick(Some(0.0));
    let starts: Vec<f64> = received.lock().unwrap()[0].iter().map(|b| b.chunk_start_ms).collect();
    assert_eq!(starts, vec![1000.0, 2000.0, 3000.0]);
}

#[test]
fn chunk_of_an_old_generation_when_drained_then_nothing_is_dispatched() {
    let (cb, received) = collector();
    let generation = Arc::new(AtomicI64::new(0));
    let g = generation.clone();
    let ingest = PerSpeakerIngest::new(
        cb,
        IngestOptions { generation_getter: Some(Arc::new(move || g.load(Ordering::SeqCst))), ..options(100, 100) },
    );
    ingest.enqueue(chunk("p1", 1));
    generation.store(1, Ordering::SeqCst);
    ingest.tick(Some(0.0));
    assert!(received.lock().unwrap().is_empty());
}

#[test]
fn generation_bumped_while_enqueue_waited_for_the_lock_when_it_resumes_then_nothing_is_recorded() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let ingest = PerSpeakerIngest::new(
        Arc::new(|_| Ok(())),
        IngestOptions {
            generation_getter: Some(Arc::new(move || i64::from(c.fetch_add(1, Ordering::SeqCst) != 0))),
            ..options(100, 100)
        },
    );
    ingest.enqueue(second("p1", 1));
    assert!(ingest.buffered().is_empty());
    assert!(ingest.get_stats().per_pid.is_empty());
    assert!(ingest.refine_pids().is_empty());
    assert!(!ingest.pcm_ever_seen());
}

#[test]
fn chunks_of_two_pids_when_drain_is_called_then_buffers_are_empty_and_tick_dispatches_nothing() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, options(100, 100));
    for (pid, seq) in [("p1", 1), ("p1", 2), ("p2", 1), ("p2", 2), ("p2", 3)] {
        ingest.enqueue(chunk(pid, seq));
    }
    ingest.drain();
    assert!(ingest.buffered().is_empty());
    let stats = ingest.get_stats();
    assert_eq!(stats.last_chunk_ts, None);
    assert!(!stats.degraded);
    ingest.tick(Some(0.0));
    assert!(received.lock().unwrap().is_empty());
}

fn active() -> Option<recog_live::per_speaker::FlagGetter> {
    Some(Arc::new(|| true))
}

#[test]
fn meeting_active_and_silence_past_the_threshold_when_ticking_then_degraded() {
    let clock = clock100();
    let (cb, _received) = collector();
    let ingest = PerSpeakerIngest::new(
        cb,
        IngestOptions { meeting_active_getter: active(), clock: clock.shared(), ..options(100, 100) },
    );
    ingest.enqueue(chunk("p1", 1));
    clock.advance(PCM_SILENCE_WATCHDOG_S + 1.0);
    ingest.tick(now(&clock));
    assert!(ingest.degraded());
}

#[test]
fn meeting_inactive_and_silence_past_the_threshold_when_ticking_then_not_degraded() {
    let clock = clock100();
    let (cb, _received) = collector();
    let ingest = PerSpeakerIngest::new(
        cb,
        IngestOptions { meeting_active_getter: Some(Arc::new(|| false)), clock: clock.shared(), ..options(100, 100) },
    );
    ingest.enqueue(chunk("p1", 1));
    clock.advance(PCM_SILENCE_WATCHDOG_S + 5.0);
    ingest.tick(now(&clock));
    assert!(!ingest.degraded());
}

#[test]
fn degraded_ingest_when_a_new_chunk_arrives_then_it_recovers_and_says_so() {
    let clock = clock100();
    let (cb, _received) = collector();
    let (logger, captured) = quiet();
    let ingest = PerSpeakerIngest::new(
        cb,
        IngestOptions { meeting_active_getter: active(), clock: clock.shared(), logger, ..options(100, 100) },
    );
    ingest.enqueue(chunk("p1", 1));
    clock.advance(PCM_SILENCE_WATCHDOG_S + 1.0);
    ingest.tick(now(&clock));
    assert!(ingest.degraded());
    ingest.enqueue(chunk("p1", 2));
    assert!(!ingest.degraded());
    assert_eq!(
        captured.lines(),
        vec![
            "[PerSpeakerIngest] pcm silence watchdog fired (31.0s since last chunk); falling back to mixed-path",
            "[PerSpeakerIngest] pcm recovered; exiting degraded mode",
        ]
    );
}

#[test]
fn two_threads_enqueuing_at_once_when_both_finish_then_every_chunk_is_buffered() {
    let (cb, _received) = collector();
    let ingest = Arc::new(PerSpeakerIngest::new(cb, options(1000, 1000)));
    let threads: Vec<_> = ["p1", "p2"]
        .into_iter()
        .map(|pid| {
            let ingest = ingest.clone();
            std::thread::spawn(move || {
                for i in 0..50 {
                    ingest.enqueue(chunk(pid, 1 + i));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let buffered = ingest.buffered();
    assert_eq!(buffered.values().sum::<usize>(), 100);
    assert_eq!(buffered["p1"], 50);
    assert_eq!(buffered["p2"], 50);
}

#[test]
fn int16le_bytes_when_drained_then_the_batch_carries_the_samples() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, options(100, 100));
    let pcm: Vec<u8> = [1i16, -1, 32767, -32768].iter().flat_map(|v| v.to_le_bytes()).collect();
    ingest.enqueue(PcmChunk { pcm, ..chunk("p1", 1) });
    ingest.tick(Some(0.0));
    let batches = received.lock().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 1);
    assert_eq!(batches[0][0].pcm, vec![1, -1, 32767, -32768]);
}

#[test]
fn nine_chunks_of_three_pids_with_batches_of_four_when_ticking_thrice_then_four_four_one() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, options(4, 100));
    for pid in ["pa", "pb", "pc"] {
        for s in 1..=3 {
            ingest.enqueue(chunk(pid, s));
        }
    }
    for _ in 0..3 {
        ingest.tick(Some(0.0));
    }
    let sizes: Vec<usize> = received.lock().unwrap().iter().map(Vec::len).collect();
    assert_eq!(sizes, vec![4, 4, 1]);
}

#[test]
fn meeting_active_and_no_pcm_ever_when_the_threshold_passes_then_degraded() {
    let clock = clock100();
    let (logger, captured) = quiet();
    let ingest = PerSpeakerIngest::new(
        Arc::new(|_| Ok(())),
        IngestOptions { meeting_active_getter: active(), clock: clock.shared(), logger, ..options(100, 100) },
    );
    ingest.tick(now(&clock));
    assert!(!ingest.degraded());
    clock.advance(PCM_SILENCE_WATCHDOG_S + 1.0);
    ingest.tick(now(&clock));
    assert!(ingest.degraded());
    assert!(captured.text().contains("31.0s since meeting start with no pcm ever"));
}

#[test]
fn generation_bumped_after_a_drain_when_reading_last_drain_generation_then_the_drain_time_value_stays() {
    let (cb, received) = collector();
    let generation = Arc::new(AtomicI64::new(5));
    let g = generation.clone();
    let ingest = PerSpeakerIngest::new(
        cb,
        IngestOptions { generation_getter: Some(Arc::new(move || g.load(Ordering::SeqCst))), ..options(100, 100) },
    );
    ingest.enqueue(chunk("p1", 1));
    ingest.tick(Some(0.0));
    assert_eq!(received.lock().unwrap().len(), 1);
    assert_eq!(ingest.last_drain_generation(), Some(5));
    generation.store(6, Ordering::SeqCst);
    assert_eq!(ingest.last_drain_generation(), Some(5));
    ingest.drain();
    assert_eq!(ingest.last_drain_generation(), Some(5));
}

#[test]
fn enqueue_drain_enqueue_when_reading_pcm_ever_seen_then_false_true_false_true() {
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), options(100, 100));
    assert!(!ingest.pcm_ever_seen());
    ingest.enqueue(chunk("p1", 1));
    assert!(ingest.pcm_ever_seen());
    ingest.drain();
    assert!(!ingest.pcm_ever_seen());
    ingest.enqueue(chunk("p1", 2));
    assert!(ingest.pcm_ever_seen());
}

#[test]
fn started_worker_when_stopped_then_the_thread_is_gone() {
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), IngestOptions { batch_interval_ms: 20, ..options(8, 24) });
    ingest.start();
    assert!(ingest.is_running());
    ingest.stop(Duration::from_secs(1));
    assert!(!ingest.is_running());
}

#[test]
fn thirty_one_second_chunks_when_enqueued_then_the_ring_holds_the_capacity_and_the_clock_counts_all() {
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), options(1000, 1000));
    for s in 1..=30 {
        ingest.enqueue(second("p1", s));
    }
    let cap_seconds = PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S + 3.0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cap_samples = (cap_seconds * f64::from(SR)) as usize;
    let total: usize = ingest.refine_state("p1").unwrap().ring.iter().map(|(n, _)| n).sum();
    assert!(total <= cap_samples);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let floor = ((cap_seconds - 1.0) * f64::from(SR)) as usize;
    assert!(total >= floor);
    assert!((ingest.get_pid_audio_ms("p1") - 30_000.0).abs() < 1e-9);
}

#[test]
fn two_pids_with_different_audio_when_reading_their_clocks_then_each_counts_its_own() {
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), options(1000, 1000));
    assert_eq!(ingest.get_pid_audio_ms("p1"), 0.0);
    for s in 1..=5 {
        ingest.enqueue(second("p1", s));
    }
    for s in 1..=3 {
        ingest.enqueue(second("p2", s));
    }
    assert!((ingest.get_pid_audio_ms("p1") - 5000.0).abs() < 1e-9);
    assert!((ingest.get_pid_audio_ms("p2") - 3000.0).abs() < 1e-9);
    assert_eq!(ingest.get_pid_audio_ms("never_seen"), 0.0);
}

#[test]
fn production_payloads_when_enqueued_then_the_overlap_is_stripped_from_the_second_on() {
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), options(1000, 1000));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let payload = (PCM_CHUNK_PAYLOAD_S * f64::from(SR)) as usize;
    #[allow(clippy::cast_possible_truncation)]
    let overlap = (PCM_CHUNK_OVERLAP_MS * u64::from(SR) / 1000) as usize;
    let stride = payload - overlap;
    for seq in 1..=3 {
        ingest.enqueue(PcmChunk { pcm: vec![0; 2 * payload], ..chunk("p1", seq) });
    }
    let expected_ms = (PCM_CHUNK_PAYLOAD_S + 2.0 * PCM_CHUNK_DURATION_S) * 1000.0;
    assert!((ingest.get_pid_audio_ms("p1") - expected_ms).abs() < 1e-6);
    let ring = ingest.refine_state("p1").unwrap().ring;
    assert_eq!(ring.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![payload, stride, stride]);
    assert!((ring[0].1 - PCM_CHUNK_PAYLOAD_S * 1000.0).abs() < 1e-6);
    assert!((ring[1].1 - (PCM_CHUNK_PAYLOAD_S + PCM_CHUNK_DURATION_S) * 1000.0).abs() < 1e-6);
    assert!((ring[2].1 - expected_ms).abs() < 1e-6);
}

#[test]
fn idle_ticks_after_a_chunk_when_another_arrives_then_silence_counts_up_and_snaps_back() {
    let clock = clock100();
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), with_clock(&clock));
    let silence = |i: &PerSpeakerIngest| i.refine_state("p1").unwrap().silence_ms.unwrap();
    ingest.enqueue(second("p1", 1));
    assert_eq!(silence(&ingest), 0.0);
    ingest.tick(now(&clock));
    assert_eq!(silence(&ingest), 0.0);
    #[allow(clippy::cast_precision_loss)]
    let step = PCM_BATCH_INTERVAL_MS as f64;
    ingest.tick(now(&clock));
    assert_eq!(silence(&ingest), step);
    ingest.tick(now(&clock));
    assert_eq!(silence(&ingest), 2.0 * step);
    ingest.enqueue(second("p1", 2));
    assert_eq!(silence(&ingest), 0.0);
}

#[test]
fn silence_past_the_turn_gap_when_ticking_then_continuous_speech_resets() {
    let clock = clock100();
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), with_clock(&clock));
    ingest.enqueue(second("p1", 1));
    ingest.enqueue(second("p1", 2));
    assert_eq!(ingest.refine_state("p1").unwrap().continuous_speech_ms, Some(2000.0));
    advance_silence_past_turn_gap(&ingest, &clock);
    let state = ingest.refine_state("p1").unwrap();
    assert!(state.silence_ms.unwrap() >= SPEAKER_TURN_GAP * 1000.0);
    assert_eq!(state.continuous_speech_ms, Some(0.0));
}

#[test]
fn populated_refine_state_when_drained_then_every_counter_is_gone() {
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), options(1000, 1000));
    ingest.enqueue(second("p1", 1));
    assert!(ingest.get_pid_audio_ms("p1") > 0.0);
    ingest.drain();
    assert!(ingest.refine_pids().is_empty());
    assert_eq!(ingest.refine_state("p1"), None);
    assert_eq!(ingest.get_pid_audio_ms("p1"), 0.0);
}

#[test]
fn five_seconds_then_a_turn_gap_when_asking_for_a_candidate_then_the_whole_window_comes_once() {
    let clock = clock100();
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), with_clock(&clock));
    for s in 1..=5 {
        ingest.enqueue(second("p1", s));
    }
    advance_silence_past_turn_gap(&ingest, &clock);
    let task = ingest.get_refine_candidate("p1").unwrap();
    assert_eq!(task.pid, "p1");
    assert_eq!(task.pcm_samples.len(), 5 * SR as usize);
    assert_eq!(task.offset_base_ms, 0.0);
    assert_eq!(task.submitted_at, now(&clock).unwrap());
    assert_eq!(ingest.refine_state("p1").unwrap().watermark_ms, Some(5000.0));
    assert!(ingest.get_refine_candidate("p1").is_none());
}

#[test]
fn fifteen_seconds_of_continuous_speech_when_asking_for_a_candidate_then_the_monolog_trigger_fires() {
    let clock = clock100();
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), with_clock(&clock));
    #[allow(clippy::cast_possible_truncation)]
    let seconds = PCM_REFINE_MONOLOG_CAP_S as i64;
    for s in 1..=seconds {
        ingest.enqueue(second("p1", s));
    }
    ingest.tick(now(&clock));
    let state = ingest.refine_state("p1").unwrap();
    assert!(state.continuous_speech_ms.unwrap() >= PCM_REFINE_MONOLOG_CAP_S * 1000.0);
    assert_eq!(state.silence_ms, Some(0.0));
    let task = ingest.get_refine_candidate("p1").unwrap();
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let expected = seconds as usize * SR as usize;
    assert_eq!(task.pcm_samples.len(), expected);
    assert_eq!(task.offset_base_ms, 0.0);
    let state = ingest.refine_state("p1").unwrap();
    assert_eq!(state.continuous_speech_ms, Some(0.0));
    #[allow(clippy::cast_precision_loss)]
    let watermark = seconds as f64 * 1000.0;
    assert_eq!(state.watermark_ms, Some(watermark));
}

#[test]
fn unknown_pid_or_everything_refined_when_asking_for_a_candidate_then_none() {
    let clock = clock100();
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), with_clock(&clock));
    assert!(ingest.get_refine_candidate("never_seen").is_none());
    ingest.enqueue(second("p1", 1));
    advance_silence_past_turn_gap(&ingest, &clock);
    assert!(ingest.get_refine_candidate("p1").is_some());
    for _ in 0..3 {
        ingest.tick(now(&clock));
    }
    assert!(ingest.get_refine_candidate("p1").is_none());
}

#[test]
fn a_candidate_taken_when_asking_again_without_new_audio_then_none() {
    let clock = clock100();
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), with_clock(&clock));
    for s in 1..=3 {
        ingest.enqueue(second("p1", s));
    }
    advance_silence_past_turn_gap(&ingest, &clock);
    assert!(ingest.get_refine_candidate("p1").is_some());
    assert!(ingest.get_refine_candidate("p1").is_none());
}

#[test]
fn thirty_seconds_of_speech_when_the_monolog_trigger_fires_then_the_window_is_capped_at_twenty() {
    let clock = clock100();
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), with_clock(&clock));
    for s in 1..=30 {
        ingest.enqueue(second("p1", s));
    }
    ingest.tick(now(&clock));
    let task = ingest.get_refine_candidate("p1").unwrap();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let window = (PCM_REFINE_WINDOW_S * f64::from(SR)) as usize;
    assert_eq!(task.pcm_samples.len(), window);
    assert_eq!(task.offset_base_ms, 30_000.0 - PCM_REFINE_WINDOW_S * 1000.0);
    assert_eq!(ingest.refine_state("p1").unwrap().watermark_ms, Some(30_000.0));
}

#[test]
fn asr_unavailable_when_ticking_then_nothing_is_drained_until_it_returns() {
    let (cb, received) = collector();
    let available = Arc::new(AtomicBool::new(false));
    let a = available.clone();
    let ingest = PerSpeakerIngest::new(
        cb,
        IngestOptions {
            meeting_active_getter: active(),
            asr_available_getter: Some(Arc::new(move || a.load(Ordering::SeqCst))),
            logger: quiet().0,
            ..IngestOptions::default()
        },
    );
    ingest.enqueue(chunk("p1", 0));
    ingest.tick(None);
    assert!(received.lock().unwrap().is_empty());
    available.store(true, Ordering::SeqCst);
    ingest.tick(None);
    let batches = received.lock().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 1);
    assert_eq!(batches[0][0].pid, "p1");
}

#[test]
fn callback_finding_asr_unavailable_when_ticking_again_then_the_same_batch_is_dispatched() {
    let calls: Received = Arc::default();
    let fail_first = Arc::new(AtomicBool::new(true));
    let sink = calls.clone();
    let (logger, captured) = quiet();
    let ingest = PerSpeakerIngest::new(
        Arc::new(move |batch: &[BatchedChunk]| {
            sink.lock().unwrap().push(batch.to_vec());
            if fail_first.swap(false, Ordering::SeqCst) { Err(AsrError::TemporarilyUnavailable) } else { Ok(()) }
        }),
        IngestOptions { meeting_active_getter: active(), logger, ..IngestOptions::default() },
    );
    ingest.enqueue(chunk("p1", 0));
    ingest.tick(None);
    ingest.tick(None);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    assert_eq!(captured.lines(), vec!["[PerSpeakerIngest] ASR unavailable — holding batch for retry"]);
}

#[test]
fn no_asr_available_getter_when_ticking_then_drain_works_as_before() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(
        cb,
        IngestOptions { meeting_active_getter: active(), logger: quiet().0, ..IngestOptions::default() },
    );
    ingest.enqueue(chunk("p1", 0));
    ingest.tick(None);
    assert_eq!(received.lock().unwrap().len(), 1);
}

#[test]
fn early_participant_with_high_seq_and_late_one_from_zero_when_draining_then_the_oldest_audio_goes_first() {
    let (cb, received) = collector();
    let ingest = PerSpeakerIngest::new(cb, options(2, 100));
    ingest.enqueue(timed("early", 700, 10.0, 13.0));
    ingest.enqueue(timed("early", 701, 13.0, 16.0));
    ingest.enqueue(timed("late", 0, 14.0, 17.0));
    ingest.enqueue(timed("late", 1, 17.0, 20.0));
    ingest.tick(Some(0.0));
    let first: Vec<(String, f64)> =
        received.lock().unwrap()[0].iter().map(|b| (b.pid.clone(), b.chunk_start_ms)).collect();
    assert_eq!(first, vec![("early".to_owned(), 10_000.0), ("early".to_owned(), 13_000.0)]);
}

#[test]
fn queue_holding_two_when_a_third_chunk_arrives_then_the_drop_is_reported() {
    let (logger, captured) = quiet();
    let ingest = PerSpeakerIngest::new(Arc::new(|_| Ok(())), IngestOptions { logger, ..options(100, 2) });
    for s in 0..3 {
        ingest.enqueue(chunk("p1", s));
    }
    assert_eq!(captured.lines(), vec!["[PerSpeakerIngest] ASR is behind: dropped 1 oldest pcm chunk(s) of Alice"]);
}

#[test]
fn callback_failing_otherwise_when_ticking_then_the_error_is_logged_and_the_batch_is_not_held() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let (logger, captured) = quiet();
    let ingest = PerSpeakerIngest::new(
        Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            Err(AsrError::Failed("boom".into()))
        }),
        IngestOptions { logger, ..options(100, 100) },
    );
    ingest.enqueue(chunk("p1", 0));
    assert_eq!(ingest.tick(Some(0.0)).len(), 1);
    assert!(ingest.tick(Some(0.0)).is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(captured.lines(), vec!["[PerSpeakerIngest] transcribe callback raised: boom"]);
}
