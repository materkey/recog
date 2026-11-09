//! The capture buffer shared by the microphone and the system audio, in place of
//! `recog/audio_capture.py`'s `BufferedAudioCapture`.
//!
//! Producers append mono float32 blocks with the wall-clock time of their first sample;
//! the consumer takes chunks of a fixed duration with 0.5 s of overlap kept for the
//! next one. While the buffer holds no more than [`CAPTURE_RESYNC_MAX_S`] its start
//! time is re-anchored to the wall clock on every block (sample counting alone drifts
//! forever, since the overlap never lets the buffer empty); a backlog (ASR down) keeps
//! sample-derived spoken-time stamps. The arithmetic is Python's, operation for
//! operation, so the times are the same `f64`s.
//!
//! Known difference: Python downmixes 2-D (frames, channels) blocks from sounddevice;
//! here blocks are mono already (the in-process sounddevice capture is not ported).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use recog_core::install_config::InstallConfig;

use crate::clock::SharedClock;
use crate::config::CAPTURE_RESYNC_MAX_S;

/// `int(seconds * sample_rate)`: truncation toward zero, as Python's `int()`.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn samples_for(seconds: f64, sample_rate: u32) -> i64 {
    (seconds * f64::from(sample_rate)) as i64
}

/// The pending-buffer cap of the installation, `[asr] pending_buffer_cap_s`.
#[must_use]
pub fn max_buffer_s(config: &InstallConfig) -> f64 {
    config.asr.pending_buffer_cap_s
}

#[derive(Debug, Default)]
struct State {
    blocks: VecDeque<Vec<f32>>,
    start_ts: Option<f64>,
    buffered: i64,
    source_epoch: u64,
    chunk_epoch: u64,
}

/// A buffer with capture times, a cap and overlapping chunks.
pub struct BufferedAudioCapture {
    sample_rate: u32,
    name: String,
    running: AtomicBool,
    clock: SharedClock,
    overlap_samples: i64,
    max_buffered_samples: i64,
    resync_max_samples: i64,
    state: Mutex<State>,
}

impl std::fmt::Debug for BufferedAudioCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferedAudioCapture").field("name", &self.name).finish_non_exhaustive()
    }
}

#[allow(clippy::cast_precision_loss)]
fn ratio(samples: i64, sample_rate: u32) -> f64 {
    // Python's int / int: both exact in f64 at these magnitudes, one rounding.
    samples as f64 / f64::from(sample_rate)
}

impl BufferedAudioCapture {
    /// A stopped buffer holding at most `max_buffer_s` seconds.
    #[must_use]
    pub fn new(sample_rate: u32, name: &str, max_buffer_s: f64, clock: SharedClock) -> Self {
        Self {
            sample_rate,
            name: name.to_owned(),
            running: AtomicBool::new(false),
            clock,
            overlap_samples: samples_for(0.5, sample_rate),
            max_buffered_samples: samples_for(max_buffer_s, sample_rate),
            resync_max_samples: samples_for(CAPTURE_RESYNC_MAX_S, sample_rate),
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the capture runs; a stopped one gives no chunks.
    #[must_use]
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::SeqCst);
    }

    /// `_append_locked`: adds `data` captured at `captured_at`; returns the number of
    /// samples dropped from the oldest end by the cap.
    pub fn append(&self, data: &[f32], captured_at: f64) -> i64 {
        let mut s = self.lock();
        if s.blocks.is_empty() {
            s.start_ts = Some(captured_at);
        }
        s.blocks.push_back(data.to_vec());
        #[allow(clippy::cast_possible_wrap)]
        let len = data.len() as i64;
        s.buffered += len;
        let mut dropped = 0;
        while s.buffered > self.max_buffered_samples {
            let Some(oldest) = s.blocks.pop_front() else { break };
            #[allow(clippy::cast_possible_wrap)]
            let n = oldest.len() as i64;
            s.buffered -= n;
            if let Some(ts) = s.start_ts.as_mut() {
                *ts += ratio(n, self.sample_rate);
            }
            dropped += n;
        }
        if s.buffered <= self.resync_max_samples {
            s.start_ts = Some(captured_at + ratio(len - s.buffered, self.sample_rate));
        }
        dropped
    }

    /// Discards all buffered audio (a meeting boundary).
    pub fn clear_buffer(&self) {
        let mut s = self.lock();
        s.blocks.clear();
        s.start_ts = None;
        s.buffered = 0;
    }

    /// The number of the stretch of continuous stream; grows at every source boundary.
    #[must_use]
    pub fn source_epoch(&self) -> u64 {
        self.lock().source_epoch
    }

    /// The `source_epoch` of the stretch the last `get_chunk` was cut from.
    #[must_use]
    pub fn chunk_epoch(&self) -> u64 {
        self.lock().chunk_epoch
    }

    /// A boundary of sources: the buffer is dropped and `source_epoch` grows.
    pub fn mark_source_boundary(&self) {
        let mut s = self.lock();
        s.blocks.clear();
        s.start_ts = None;
        s.buffered = 0;
        s.source_epoch += 1;
    }

    /// Samples held now.
    #[must_use]
    pub fn buffered_samples(&self) -> i64 {
        self.lock().buffered
    }

    /// Wall-clock time of the first unread sample, None while empty.
    #[must_use]
    pub fn buffer_start_ts(&self) -> Option<f64> {
        self.lock().start_ts
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().blocks.is_empty()
    }

    /// `duration` seconds of audio and the wall-clock time of its first sample, or None
    /// while stopped or short; 0.5 s stay for the next chunk.
    pub fn get_chunk(&self, duration: f64) -> Option<(Vec<f32>, f64)> {
        if !self.running() {
            return None;
        }
        let needed = usize::try_from(samples_for(duration, self.sample_rate)).unwrap_or(0);
        let mut s = self.lock();
        if s.blocks.is_empty() {
            return None;
        }
        let total: usize = s.blocks.iter().map(Vec::len).sum();
        if total < needed {
            return None;
        }
        let mut audio = Vec::with_capacity(total);
        for block in &s.blocks {
            audio.extend_from_slice(block);
        }
        s.chunk_epoch = s.source_epoch;
        let start = s.start_ts.unwrap_or_else(|| self.clock.wall() - duration);
        let overlap = usize::try_from(self.overlap_samples).unwrap_or(0);
        let overlap_start = needed.saturating_sub(overlap);
        let rest = audio[overlap_start..].to_vec();
        audio.truncate(needed);
        s.blocks.clear();
        #[allow(clippy::cast_possible_wrap)]
        {
            s.buffered = rest.len() as i64;
            s.start_ts = Some(start + ratio(overlap_start as i64, self.sample_rate));
        }
        s.blocks.push_back(rest);
        Some((audio, start))
    }
}
