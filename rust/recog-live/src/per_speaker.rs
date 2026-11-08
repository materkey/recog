//! Per-speaker PCM ingest, in place of `recog/per_speaker.py`.
//!
//! Collects the PCM chunks the extension sends through the bridge, buffers them per
//! participant (pid) sorted by `seq`, and hands batches of up to `max_batch_size` to
//! the transcribe callback (`ASR.transcribe_batch` in live). Attribution travels with
//! the chunk: each batched chunk carries its pid, speaker name and `is_local`.
//!
//! - Meeting generation: a chunk carries the generation read when it was enqueued; a
//!   chunk of an old generation is dropped at enqueue (the switch raced it) and at
//!   drain. [`PerSpeakerIngest::last_drain_generation`] is the generation the latest
//!   drain saw, for the callback to reject a batch whose meeting switched since.
//! - Watchdog: without PCM for `silence_watchdog_s` while a meeting is active (since
//!   its first tick when no chunk ever came) the ingest is degraded; live falls back
//!   to the mixed path. A new chunk leaves degraded mode.
//! - ASR gate: while ASR cannot serve, nothing is drained (draining is destructive);
//!   a batch whose callback found ASR unavailable is held and dispatched first.
//! - Refine: every chunk's samples (the 600 ms overlap of production payloads
//!   stripped) go into a per-pid ring and advance the pid's audio clock; the gap and
//!   monolog triggers build [`RefineTask`]s from the drained part of the ring.
//!
//! The arithmetic follows Python's step by step, so the same input gives the same
//! floats: the differential test compares them exactly.
//!
//! Differences from Python:
//! - An odd trailing byte of `pcm` is ignored; `np.frombuffer` raises on it, which
//!   leaves Python's ingest half-updated.
//! - `stop(timeout)` leaves the thread detached when it does not finish in time, as
//!   `Thread.join(timeout)` does.
//! - Callback errors are `AsrError` values (`TemporarilyUnavailable` holds the
//!   batch); their message is logged as Python logs the exception's text.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::asr::AsrError;
use crate::clock::{SharedClock, SystemClock};
use crate::config::{
    PCM_BATCH_INTERVAL_MS, PCM_CHUNK_DURATION_S, PCM_CHUNK_OVERLAP_MS, PCM_CHUNK_PAYLOAD_S, PCM_MAX_BATCH_SIZE,
    PCM_REFINE_MONOLOG_CAP_S, PCM_REFINE_WINDOW_S, PCM_SILENCE_WATCHDOG_S, SPEAKER_TURN_GAP,
};
use crate::log::Logger;
use crate::refine::{Event, RefineTask};

/// The default sample rate when a chunk says 0, Python's `sample_rate or 16000`.
const DEFAULT_SAMPLE_RATE: u32 = 16_000;

/// A chunk of Int16 PCM from one participant, the bridge's `PcmChunk`.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmChunk {
    pub pid: String,
    pub speaker_name: String,
    pub is_local: bool,
    pub seq: i64,
    /// Unix seconds of the window's start, clock offset applied.
    pub chunk_start_ts: f64,
    pub chunk_end_ts: f64,
    pub sample_rate: u32,
    /// Int16LE bytes.
    pub pcm: Vec<u8>,
}

/// One chunk of a batch, Python's `BatchedChunk` tuple.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchedChunk {
    pub pid: String,
    pub speaker_name: String,
    pub is_local: bool,
    pub pcm: Vec<i16>,
    pub chunk_start_ms: f64,
    pub chunk_end_ms: f64,
    /// The pid's audio clock before and after this chunk (overlap stripped), for
    /// non-overlapping live-segment bounds.
    pub pid_audio_ms_start: f64,
    pub pid_audio_ms_end: f64,
}

/// Per-pid counters of [`PerSpeakerIngest::get_stats`].
#[derive(Debug, Clone, PartialEq)]
pub struct PidStats {
    pub speaker_name: String,
    pub is_local: bool,
    pub chunks_seen: u64,
    pub last_seq: Option<i64>,
    pub gaps_count: i64,
    pub out_of_order_count: u64,
    pub first_seen_ts: f64,
    pub last_seen_ts: f64,
    pub age_s: f64,
    pub since_last_chunk_s: f64,
    pub chunks_per_sec: f64,
}

/// [`PerSpeakerIngest::get_stats`].
#[derive(Debug, Clone, PartialEq)]
pub struct IngestStats {
    pub pcm_ever_seen: bool,
    pub degraded: bool,
    pub meeting_active_since: Option<f64>,
    pub last_chunk_ts: Option<f64>,
    pub now: f64,
    pub pending_buffer_sizes: BTreeMap<String, usize>,
    pub per_pid: BTreeMap<String, PidStats>,
}

/// A pid's refine bookkeeping, for diagnostics and the tests.
#[derive(Debug, Clone, PartialEq)]
pub struct PidRefineState {
    /// Sample counts of the ring's entries, oldest first, with their audio-clock ends.
    pub ring: Vec<(usize, f64)>,
    pub audio_ms: f64,
    pub drained_ms: Option<f64>,
    pub watermark_ms: Option<f64>,
    pub silence_ms: Option<f64>,
    pub continuous_speech_ms: Option<f64>,
    pub sample_rate: Option<u32>,
    pub had_chunk_since_last_tick: bool,
}

/// The transcribe callback; `Err(TemporarilyUnavailable)` holds the batch for retry.
pub type TranscribeCallback = Arc<dyn Fn(&[BatchedChunk]) -> Result<(), AsrError> + Send + Sync>;
/// The refine submit callback (`RefineWorker::submit` in live).
pub type RefineSubmit = Arc<dyn Fn(RefineTask) -> Result<(), String> + Send + Sync>;
/// A getter of the current meeting generation.
pub type GenerationGetter = Arc<dyn Fn() -> i64 + Send + Sync>;
/// A getter of a flag (meeting active, ASR available).
pub type FlagGetter = Arc<dyn Fn() -> bool + Send + Sync>;

/// Construction options, Python's keyword arguments with their defaults.
#[derive(Clone)]
pub struct IngestOptions {
    pub generation_getter: Option<GenerationGetter>,
    pub meeting_active_getter: Option<FlagGetter>,
    pub batch_interval_ms: u64,
    pub max_batch_size: usize,
    pub silence_watchdog_s: f64,
    pub max_chunks_per_pid: usize,
    /// `time_fn` is this clock's wall time.
    pub clock: SharedClock,
    pub refine_submit_callback: Option<RefineSubmit>,
    pub asr_available_getter: Option<FlagGetter>,
    pub logger: Logger,
}

impl Default for IngestOptions {
    fn default() -> Self {
        Self {
            generation_getter: None,
            meeting_active_getter: None,
            batch_interval_ms: PCM_BATCH_INTERVAL_MS,
            max_batch_size: PCM_MAX_BATCH_SIZE,
            silence_watchdog_s: PCM_SILENCE_WATCHDOG_S,
            max_chunks_per_pid: 3 * PCM_MAX_BATCH_SIZE,
            clock: SystemClock::shared(),
            refine_submit_callback: None,
            asr_available_getter: None,
            logger: Logger::stderr(),
        }
    }
}

struct BufferedChunk {
    chunk: PcmChunk,
    meeting_generation: i64,
    pid_audio_ms_start: f64,
    pid_audio_ms_end: f64,
}

struct RawStats {
    speaker_name: String,
    is_local: bool,
    chunks_seen: u64,
    first_seen_ts: f64,
    last_seen_ts: f64,
    last_seq: Option<i64>,
    gaps_count: i64,
    out_of_order_count: u64,
}

#[derive(Default)]
struct State {
    pending_retry_batch: Option<Vec<BatchedChunk>>,
    buffers: BTreeMap<String, Vec<BufferedChunk>>,
    stats: BTreeMap<String, RawStats>,
    last_chunk_ts: Option<f64>,
    degraded: bool,
    pcm_ever_seen: bool,
    meeting_active_since: Option<f64>,
    last_drain_generation: Option<i64>,
    refine_submit_callback: Option<RefineSubmit>,
    pcm_rings: BTreeMap<String, VecDeque<(Vec<i16>, f64)>>,
    pid_audio_ms: BTreeMap<String, f64>,
    /// The keys of `pid_audio_ms` in insertion order, which a Python dict keeps: the
    /// refine candidates of a tick are taken in this order.
    pid_audio_order: Vec<String>,
    pid_drained_ms: BTreeMap<String, f64>,
    refine_watermark_ms: BTreeMap<String, f64>,
    silence_ms: BTreeMap<String, f64>,
    continuous_speech_ms: BTreeMap<String, f64>,
    pids_had_chunk_since_last_tick: BTreeSet<String>,
    pid_sample_rate: BTreeMap<String, u32>,
}

struct Inner {
    transcribe_callback: TranscribeCallback,
    generation: GenerationGetter,
    meeting_active: FlagGetter,
    asr_available: FlagGetter,
    batch_interval_s: f64,
    max_batch_size: usize,
    silence_watchdog_s: f64,
    max_chunks_per_pid: usize,
    clock: SharedClock,
    logger: Logger,
    ring_capacity_ms: f64,
    state: Mutex<State>,
    stop_event: Event,
    wake_event: Event,
}

/// Per-pid buffers and batched dispatch to ASR; see the module documentation.
///
/// Thread-safe: `enqueue` comes from the bridge, `tick` from the worker thread,
/// `drain`/`stop` from anywhere. The getters run under the ingest's lock and the
/// callbacks outside it, as in Python.
pub struct PerSpeakerIngest {
    inner: Arc<Inner>,
    thread: Mutex<Option<(JoinHandle<()>, Arc<Event>)>>,
}

fn decode_pcm_int16(pcm: &[u8]) -> Vec<i16> {
    pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

/// Python's `round(x, 2)`: the exact value rounded half to even at two decimals.
fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x)
}

/// Python's `max(0.0, x)`: `x` only when it is greater.
fn max_zero(x: f64) -> f64 {
    if x > 0.0 { x } else { 0.0 }
}

#[allow(clippy::cast_precision_loss)]
fn as_f64(n: usize) -> f64 {
    n as f64
}

impl PerSpeakerIngest {
    #[must_use]
    pub fn new(transcribe_callback: TranscribeCallback, options: IngestOptions) -> Self {
        let state = State { refine_submit_callback: options.refine_submit_callback, ..State::default() };
        let inner = Inner {
            transcribe_callback,
            generation: options.generation_getter.unwrap_or_else(|| Arc::new(|| 0)),
            meeting_active: options.meeting_active_getter.unwrap_or_else(|| Arc::new(|| false)),
            asr_available: options.asr_available_getter.unwrap_or_else(|| Arc::new(|| true)),
            #[allow(clippy::cast_precision_loss)]
            batch_interval_s: options.batch_interval_ms as f64 / 1000.0,
            max_batch_size: options.max_batch_size,
            silence_watchdog_s: options.silence_watchdog_s,
            max_chunks_per_pid: options.max_chunks_per_pid,
            clock: options.clock,
            logger: options.logger,
            ring_capacity_ms: (PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S + 3.0) * 1000.0,
            state: Mutex::new(state),
            stop_event: Event::default(),
            wake_event: Event::default(),
        };
        Self { inner: Arc::new(inner), thread: Mutex::new(None) }
    }

    /// Whether the watchdog fired and no chunk came since.
    #[must_use]
    pub fn degraded(&self) -> bool {
        self.inner.lock().degraded
    }

    /// Whether a chunk was enqueued since the start or the last `drain`.
    #[must_use]
    pub fn pcm_ever_seen(&self) -> bool {
        self.inner.lock().pcm_ever_seen
    }

    /// The generation the most recent drain saw (kept across `drain`).
    #[must_use]
    pub fn last_drain_generation(&self) -> Option<i64> {
        self.inner.lock().last_drain_generation
    }

    /// Per-pid counters and the top-level flags, for live diagnostics.
    #[must_use]
    pub fn get_stats(&self) -> IngestStats {
        let now = self.inner.clock.wall();
        let state = self.inner.lock();
        let per_pid = state
            .stats
            .iter()
            .map(|(pid, s)| {
                let duration = f64::max(now - s.first_seen_ts, 0.001);
                #[allow(clippy::cast_precision_loss)]
                let rate = s.chunks_seen as f64 / duration;
                let stats = PidStats {
                    speaker_name: s.speaker_name.clone(),
                    is_local: s.is_local,
                    chunks_seen: s.chunks_seen,
                    last_seq: s.last_seq,
                    gaps_count: s.gaps_count,
                    out_of_order_count: s.out_of_order_count,
                    first_seen_ts: s.first_seen_ts,
                    last_seen_ts: s.last_seen_ts,
                    age_s: round2(now - s.first_seen_ts),
                    since_last_chunk_s: round2(now - s.last_seen_ts),
                    chunks_per_sec: round2(rate),
                };
                (pid.clone(), stats)
            })
            .collect();
        IngestStats {
            pcm_ever_seen: state.pcm_ever_seen,
            degraded: state.degraded,
            meeting_active_since: state.meeting_active_since,
            last_chunk_ts: state.last_chunk_ts,
            now,
            pending_buffer_sizes: state.buffers.iter().map(|(pid, b)| (pid.clone(), b.len())).collect(),
            per_pid,
        }
    }

    /// Enqueues a chunk; safe from the bridge's thread.
    pub fn enqueue(&self, chunk: PcmChunk) {
        let inner = &self.inner;
        let now = inner.clock.wall();
        let generation = (inner.generation)();
        let mut wake = false;
        let mut dropped_overflow = 0;
        let label = if chunk.speaker_name.is_empty() { chunk.pid.clone() } else { chunk.speaker_name.clone() };
        {
            let mut state = inner.lock();
            let state = &mut *state;
            // The generation was read before the lock: a switch in between makes this
            // chunk the old meeting's, and it must not seed the new one's rings.
            if generation != (inner.generation)() {
                return;
            }
            let samples = decode_pcm_int16(&chunk.pcm);
            let pid = chunk.pid.clone();
            let buf = state.buffers.entry(pid.clone()).or_default();
            let idx = buf.partition_point(|b| b.chunk.seq < chunk.seq);
            let stats = state.stats.entry(pid.clone()).or_insert_with(|| RawStats {
                speaker_name: chunk.speaker_name.clone(),
                is_local: chunk.is_local,
                chunks_seen: 0,
                first_seen_ts: now,
                last_seen_ts: now,
                last_seq: None,
                gaps_count: 0,
                out_of_order_count: 0,
            });
            if !chunk.speaker_name.is_empty() {
                stats.speaker_name.clone_from(&chunk.speaker_name);
            }
            stats.chunks_seen += 1;
            stats.last_seen_ts = now;
            let prev_seq = stats.last_seq;
            if let Some(prev) = prev_seq {
                if chunk.seq > prev + 1 {
                    stats.gaps_count += chunk.seq - prev - 1;
                } else if chunk.seq <= prev {
                    stats.out_of_order_count += 1;
                }
            }
            if prev_seq.is_none_or(|prev| chunk.seq > prev) {
                stats.last_seq = Some(chunk.seq);
            }

            if state.degraded {
                inner.logger.line("[PerSpeakerIngest] pcm recovered; exiting degraded mode");
                state.degraded = false;
            }
            state.last_chunk_ts = Some(now);
            state.pcm_ever_seen = true;

            let sample_rate = if chunk.sample_rate == 0 { DEFAULT_SAMPLE_RATE } else { chunk.sample_rate };
            let sr = f64::from(sample_rate);
            let mut samples = samples;
            // Strip the worklet's 600 ms overlap from every chunk after the first, when
            // the payload has the production shape (3.6 s); other sizes are left alone.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let expected_payload_samples = (PCM_CHUNK_PAYLOAD_S * sr) as usize;
            let first_chunk_for_pid = !state.pid_audio_ms.contains_key(&pid);
            if !first_chunk_for_pid && samples.len() == expected_payload_samples {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
                let overlap_samples = ((PCM_CHUNK_OVERLAP_MS * u64::from(sample_rate)) as f64 / 1000.0) as usize;
                if 0 < overlap_samples && overlap_samples < samples.len() {
                    samples.drain(..overlap_samples);
                }
            }
            let chunk_ms = (as_f64(samples.len()) / sr) * 1000.0;
            let mut bounds = (0.0, 0.0);
            if !samples.is_empty() && chunk_ms > 0.0 {
                let prev_pid_audio_ms = state.pid_audio_ms.get(&pid).copied().unwrap_or(0.0);
                let end = prev_pid_audio_ms + chunk_ms;
                if state.pid_audio_ms.insert(pid.clone(), end).is_none() {
                    state.pid_audio_order.push(pid.clone());
                }
                state.pcm_rings.entry(pid.clone()).or_default().push_back((samples, end));
                state.refine_watermark_ms.entry(pid.clone()).or_insert(0.0);
                state.silence_ms.insert(pid.clone(), 0.0);
                let continuous = state.continuous_speech_ms.get(&pid).copied().unwrap_or(0.0) + chunk_ms;
                state.continuous_speech_ms.insert(pid.clone(), continuous);
                state.pid_sample_rate.insert(pid.clone(), sample_rate);
                enforce_ring_capacity(state, inner.ring_capacity_ms, &pid, sample_rate);
                state.pids_had_chunk_since_last_tick.insert(pid.clone());
                bounds = (prev_pid_audio_ms, end);
            }

            let buffered = BufferedChunk {
                chunk,
                meeting_generation: generation,
                pid_audio_ms_start: bounds.0,
                pid_audio_ms_end: bounds.1,
            };
            let buf = state.buffers.entry(pid).or_default();
            buf.insert(idx, buffered);
            while buf.len() > inner.max_chunks_per_pid {
                buf.remove(0);
                dropped_overflow += 1;
            }

            let total: usize = state.buffers.values().map(Vec::len).sum();
            if total >= inner.max_batch_size {
                wake = true;
            }
        }
        if dropped_overflow > 0 {
            inner.logger.rate_limited(
                "per-speaker-overflow",
                &format!("[PerSpeakerIngest] ASR is behind: dropped {dropped_overflow} oldest pcm chunk(s) of {label}"),
            );
        }
        if wake {
            inner.wake_event.set();
        }
    }

    /// Clears every per-pid buffer and counter on a meeting boundary (after the
    /// generation is bumped). `last_drain_generation` stays, for an in-flight callback.
    pub fn drain(&self) {
        let mut state = self.inner.lock();
        let refine_submit_callback = state.refine_submit_callback.take();
        let last_drain_generation = state.last_drain_generation;
        *state = State { refine_submit_callback, last_drain_generation, ..State::default() };
    }

    /// One step: watchdog, at most one batch to the callback, refine candidates to the
    /// submit callback. Returns the dispatched batch. `now` defaults to the clock.
    pub fn tick(&self, now: Option<f64>) -> Vec<BatchedChunk> {
        let inner = &self.inner;
        let now = now.unwrap_or_else(|| inner.clock.wall());
        let mut batch = Vec::new();
        let mut batch_generation = None;
        let mut active_pids = Vec::new();
        {
            let mut state = inner.lock();
            inner.check_watchdog(&mut state, now);
            if (inner.asr_available)() {
                batch = match state.pending_retry_batch.take() {
                    Some(held) => held,
                    None => inner.drain_batch(&mut state),
                };
                batch_generation = state.last_drain_generation;
            }
            // While ASR is unavailable the gate stays shut: draining is destructive.
            update_per_pid_silence(&mut state);
            if state.refine_submit_callback.is_some() {
                active_pids = state.pid_audio_order.clone();
            }
        }
        if !batch.is_empty() {
            match (inner.transcribe_callback)(&batch) {
                Ok(()) => {}
                Err(AsrError::TemporarilyUnavailable) => {
                    {
                        let mut state = inner.lock();
                        if batch_generation != Some((inner.generation)()) {
                            return Vec::new();
                        }
                        state.pending_retry_batch = Some(batch.clone());
                    }
                    inner.logger.line("[PerSpeakerIngest] ASR unavailable — holding batch for retry");
                }
                Err(e) => inner.logger.line(&format!("[PerSpeakerIngest] transcribe callback raised: {e}")),
            }
        }
        for pid in &active_pids {
            let Some(callback) = inner.lock().refine_submit_callback.clone() else { break };
            let Some(task) = self.get_refine_candidate(pid) else { continue };
            if let Err(e) = callback(task) {
                inner.logger.line(&format!("[PerSpeakerIngest] refine submit raised: {e}"));
            }
        }
        batch
    }

    /// Attaches or detaches the per-tick refine submit callback.
    pub fn set_refine_submit_callback(&self, callback: Option<RefineSubmit>) {
        self.inner.lock().refine_submit_callback = callback;
    }

    /// Spawns the worker thread: a tick every batch interval, or at once when a full
    /// batch is waiting.
    pub fn start(&self) {
        let mut thread = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, finished)) = thread.as_ref()
            && !finished.is_set()
        {
            return;
        }
        self.inner.stop_event.clear();
        self.inner.wake_event.clear();
        let worker = Self { inner: self.inner.clone(), thread: Mutex::new(None) };
        let finished = Arc::new(Event::default());
        let done = finished.clone();
        let handle = std::thread::Builder::new()
            .name("PerSpeakerIngest".into())
            .spawn(move || {
                worker.run();
                done.set();
            })
            .unwrap_or_else(|e| panic!("cannot start the PerSpeakerIngest thread: {e}"));
        *thread = Some((handle, finished));
    }

    /// Signals the worker thread to exit and waits up to `timeout` for it.
    pub fn stop(&self, timeout: Duration) {
        self.inner.stop_event.set();
        self.inner.wake_event.set();
        let taken = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some((handle, finished)) = taken
            && finished.wait(timeout)
        {
            let _ = handle.join();
        }
    }

    /// Whether the worker thread runs.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.thread.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_some_and(|(_, f)| !f.is_set())
    }

    /// The pid's audio clock (ms): the overlap-stripped audio enqueued this meeting.
    #[must_use]
    pub fn get_pid_audio_ms(&self, pid: &str) -> f64 {
        self.inner.lock().pid_audio_ms.get(pid).copied().unwrap_or(0.0)
    }

    /// The chunks buffered per pid (empty lists of pids already drained included).
    #[must_use]
    pub fn buffered(&self) -> BTreeMap<String, usize> {
        self.inner.lock().buffers.iter().map(|(pid, b)| (pid.clone(), b.len())).collect()
    }

    /// The pids with refine bookkeeping.
    #[must_use]
    pub fn refine_pids(&self) -> Vec<String> {
        let state = self.inner.lock();
        let mut pids: BTreeSet<String> = state.pid_audio_ms.keys().cloned().collect();
        pids.extend(state.pcm_rings.keys().cloned());
        pids.extend(state.silence_ms.keys().cloned());
        pids.extend(state.pids_had_chunk_since_last_tick.iter().cloned());
        pids.into_iter().collect()
    }

    /// The pid's refine bookkeeping, None when the ingest knows nothing of it.
    #[must_use]
    pub fn refine_state(&self, pid: &str) -> Option<PidRefineState> {
        let state = self.inner.lock();
        let known = state.pid_audio_ms.contains_key(pid)
            || state.pcm_rings.contains_key(pid)
            || state.silence_ms.contains_key(pid)
            || state.pids_had_chunk_since_last_tick.contains(pid);
        known.then(|| PidRefineState {
            ring: state
                .pcm_rings
                .get(pid)
                .map(|r| r.iter().map(|(s, end)| (s.len(), *end)).collect())
                .unwrap_or_default(),
            audio_ms: state.pid_audio_ms.get(pid).copied().unwrap_or(0.0),
            drained_ms: state.pid_drained_ms.get(pid).copied(),
            watermark_ms: state.refine_watermark_ms.get(pid).copied(),
            silence_ms: state.silence_ms.get(pid).copied(),
            continuous_speech_ms: state.continuous_speech_ms.get(pid).copied(),
            sample_rate: state.pid_sample_rate.get(pid).copied(),
            had_chunk_since_last_tick: state.pids_had_chunk_since_last_tick.contains(pid),
        })
    }

    /// A refine task for `pid` when the gap or the monolog trigger fired, else None.
    ///
    /// The window spans the drained audio after the watermark, capped at
    /// `PCM_REFINE_WINDOW_S`; the watermark moves to the drained end atomically, and a
    /// monolog trigger also resets the continuous-speech counter.
    pub fn get_refine_candidate(&self, pid: &str) -> Option<RefineTask> {
        let inner = &self.inner;
        let mut state = inner.lock();
        let drained_ms = state.pid_drained_ms.get(pid).copied().unwrap_or(0.0);
        let watermark = state.refine_watermark_ms.get(pid).copied().unwrap_or(0.0);
        let unrefined_ms = drained_ms - watermark;
        if unrefined_ms <= 0.0 {
            return None;
        }
        let silence_ms = state.silence_ms.get(pid).copied().unwrap_or(0.0);
        let continuous_ms = state.continuous_speech_ms.get(pid).copied().unwrap_or(0.0);
        let gap_triggered = silence_ms >= SPEAKER_TURN_GAP * 1000.0;
        let monolog_triggered = continuous_ms >= PCM_REFINE_MONOLOG_CAP_S * 1000.0;
        if !(gap_triggered || monolog_triggered) {
            return None;
        }
        let ring = state.pcm_rings.get(pid).filter(|r| !r.is_empty())?;
        let sample_rate = match state.pid_sample_rate.get(pid).copied().unwrap_or(DEFAULT_SAMPLE_RATE) {
            0 => DEFAULT_SAMPLE_RATE,
            sr => sr,
        };
        let sr = f64::from(sample_rate);
        let window_ms = PCM_REFINE_WINDOW_S * 1000.0;
        let target_ms = unrefined_ms.min(window_ms);
        #[allow(clippy::cast_possible_truncation)]
        let target_samples = (target_ms * sr / 1000.0).round_ties_even() as i64;
        if target_samples <= 0 {
            return None;
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let target_samples = target_samples as usize;
        let mut pulled: Vec<&[i16]> = Vec::new();
        let mut total = 0;
        for (samples, entry_end_ms) in ring.iter().rev() {
            // Audio not yet drained to live has no live segment to refine.
            if *entry_end_ms > drained_ms {
                continue;
            }
            // Older entries were covered by a previous refine.
            if *entry_end_ms <= watermark {
                break;
            }
            pulled.push(samples);
            total += samples.len();
            if total >= target_samples {
                break;
            }
        }
        if pulled.is_empty() {
            return None;
        }
        let mut pcm: Vec<i16> = pulled.iter().rev().flat_map(|s| s.iter().copied()).collect();
        if pcm.len() > target_samples {
            pcm.drain(..pcm.len() - target_samples);
        }
        let actual_samples_ms = (as_f64(pcm.len()) / sr) * 1000.0;
        let offset_base_ms = max_zero(drained_ms - actual_samples_ms);
        state.refine_watermark_ms.insert(pid.to_owned(), drained_ms);
        if monolog_triggered {
            state.continuous_speech_ms.insert(pid.to_owned(), 0.0);
        }
        Some(RefineTask {
            pid: pid.to_owned(),
            pcm_samples: pcm,
            offset_base_ms,
            submitted_at: inner.clock.wall(),
            meeting_generation: (inner.generation)(),
        })
    }

    fn run(&self) {
        let interval = Duration::from_secs_f64(self.inner.batch_interval_s);
        while !self.inner.stop_event.is_set() {
            self.inner.wake_event.wait(interval);
            self.inner.wake_event.clear();
            if self.inner.stop_event.is_set() {
                break;
            }
            let _ = self.tick(None);
        }
    }
}

impl Drop for PerSpeakerIngest {
    fn drop(&mut self) {
        if self.thread.get_mut().unwrap_or_else(PoisonError::into_inner).is_some() {
            self.stop(Duration::from_secs(2));
        }
    }
}

/// Drops the oldest ring entries of `pid` while it holds more than the capacity,
/// whole chunks at a time, keeping at least one.
fn enforce_ring_capacity(state: &mut State, capacity_ms: f64, pid: &str, sample_rate: u32) {
    let Some(ring) = state.pcm_rings.get_mut(pid) else { return };
    if ring.is_empty() || sample_rate == 0 {
        return;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cap_samples = (capacity_ms * f64::from(sample_rate) / 1000.0) as usize;
    if cap_samples == 0 {
        return;
    }
    let mut total: usize = ring.iter().map(|(s, _)| s.len()).sum();
    while total > cap_samples && ring.len() > 1 {
        if let Some((oldest, _)) = ring.pop_front() {
            total -= oldest.len();
        }
    }
}

/// Advances the silence of pids without a chunk since the last tick; silence past the
/// turn gap resets continuous speech.
fn update_per_pid_silence(state: &mut State) {
    let touched = std::mem::take(&mut state.pids_had_chunk_since_last_tick);
    let turn_gap_ms = SPEAKER_TURN_GAP * 1000.0;
    #[allow(clippy::cast_precision_loss)]
    let interval = PCM_BATCH_INTERVAL_MS as f64;
    let pids: Vec<String> = state.pid_audio_ms.keys().cloned().collect();
    for pid in pids {
        if touched.contains(&pid) {
            continue;
        }
        let new_silence = state.silence_ms.get(&pid).copied().unwrap_or(0.0) + interval;
        state.silence_ms.insert(pid.clone(), new_silence);
        if new_silence >= turn_gap_ms {
            state.continuous_speech_ms.insert(pid, 0.0);
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn check_watchdog(&self, state: &mut State, now: f64) {
        if state.degraded {
            return;
        }
        if !(self.meeting_active)() {
            // Outside a meeting: the next meeting gets a fresh window.
            state.meeting_active_since = None;
            return;
        }
        let since = *state.meeting_active_since.get_or_insert(now);
        let reference = state.last_chunk_ts.unwrap_or(since);
        let elapsed = now - reference;
        if elapsed > self.silence_watchdog_s {
            let suffix = if state.last_chunk_ts.is_some() {
                format!("{elapsed:.1}s since last chunk")
            } else {
                format!("{elapsed:.1}s since meeting start with no pcm ever")
            };
            self.logger
                .line(&format!("[PerSpeakerIngest] pcm silence watchdog fired ({suffix}); falling back to mixed-path"));
            state.degraded = true;
        }
    }

    fn drain_batch(&self, state: &mut State) -> Vec<BatchedChunk> {
        let current_gen = (self.generation)();
        state.last_drain_generation = Some(current_gen);
        let mut batch = Vec::new();
        let mut dropped_stale = 0;
        while batch.len() < self.max_batch_size {
            // The pid whose head chunk was captured first, ties by pid; not the lowest
            // seq, which counts from each pid's own join.
            let mut best: Option<(&String, f64)> = None;
            for (pid, buf) in &state.buffers {
                let Some(head) = buf.first() else { continue };
                if best.is_none_or(|(_, start)| head.chunk.chunk_start_ts < start) {
                    best = Some((pid, head.chunk.chunk_start_ts));
                }
            }
            let Some((best_pid, _)) = best else { break };
            let best_pid = best_pid.clone();
            let Some(buf) = state.buffers.get_mut(&best_pid) else { break };
            let buffered = buf.remove(0);
            if buffered.meeting_generation != current_gen {
                dropped_stale += 1;
                continue;
            }
            let pcm = decode_pcm_int16(&buffered.chunk.pcm);
            let drained = state.pid_drained_ms.get(&best_pid).copied().unwrap_or(0.0);
            if buffered.pid_audio_ms_end > drained {
                state.pid_drained_ms.insert(best_pid, buffered.pid_audio_ms_end);
            }
            let chunk = buffered.chunk;
            batch.push(BatchedChunk {
                pid: chunk.pid,
                speaker_name: chunk.speaker_name,
                is_local: chunk.is_local,
                pcm,
                chunk_start_ms: chunk.chunk_start_ts * 1000.0,
                chunk_end_ms: chunk.chunk_end_ts * 1000.0,
                pid_audio_ms_start: buffered.pid_audio_ms_start,
                pid_audio_ms_end: buffered.pid_audio_ms_end,
            });
        }
        if dropped_stale > 0 {
            self.logger.line(&format!(
                "[PerSpeakerIngest] dropped {dropped_stale} stale-generation chunks (current_gen={current_gen})"
            ));
        }
        batch
    }
}
