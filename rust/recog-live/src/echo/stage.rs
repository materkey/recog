//! `EchoStage`: the microphone goes in and comes out the same length, later, without
//! the echo.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::detector::EchoPathDetector;
use super::filters::{Fir, HighPass, Speex};
use super::inverse::shared_inverse_fir;
use super::numpy::std;
use super::placer::{ReferenceRing, TimelinePlacer};
use super::suppressor::Suppressor;
use super::{BLOCK, EchoSink, FRAME, HIGH_PASS_HZ, HOP, QUIET_HOLD_S, QUIET_RMS, REFERENCE_LEAD, REFERENCE_WAIT, SR};

/// What the system capture's reader thread writes and the microphone's reads.
struct ReferenceSide {
    ring: ReferenceRing,
    placer: TimelinePlacer,
    echo_possible: bool,
}

/// The microphone's side: everything `add_mic` keeps between calls.
struct MicSide {
    placer: TimelinePlacer,
    pending: Vec<f64>,
    position: Option<i64>,
    highpass: HighPass,
    inverse: Fir,
    speex: Speex,
    suppressor: Suppressor,
    detector: EchoPathDetector,
    raw_delay: Vec<f64>,
    running: bool,
    cold: bool,
    quiet_frames: usize,
    raw_share: f64,
}

/// The echo stage, shared by the two captures.
///
/// `add_reference` comes from the system capture's reader thread, `add_mic` from the
/// microphone's. The microphone is raw whenever any of these fails: the output can make
/// an echo (built-in speakers, not muted), the echo path is present, the reference has
/// not been quiet, the reference exists.
pub struct EchoStage {
    reference: Mutex<ReferenceSide>,
    mic: Mutex<MicSide>,
    active: AtomicBool,
}

impl EchoStage {
    /// A stage with the process's measured inverse filter.
    #[must_use]
    pub fn new() -> Self {
        Self::with_inverse(shared_inverse_fir().as_ref().clone())
    }

    /// A stage with the given inverse low-cut taps (tests hand it Python's).
    #[must_use]
    pub fn with_inverse(taps: Vec<f64>) -> Self {
        Self {
            reference: Mutex::new(ReferenceSide {
                ring: ReferenceRing::new(),
                placer: TimelinePlacer::new(SR),
                echo_possible: false,
            }),
            mic: Mutex::new(MicSide {
                placer: TimelinePlacer::new(SR),
                pending: Vec::new(),
                position: None,
                highpass: HighPass::new(HIGH_PASS_HZ),
                inverse: Fir::new(taps),
                speex: Speex::new(),
                suppressor: Suppressor::new(),
                detector: EchoPathDetector::new(SR),
                raw_delay: vec![0.0; HOP],
                running: false,
                cold: true,
                quiet_frames: 0,
                raw_share: 1.0,
            }),
            active: AtomicBool::new(false),
        }
    }

    /// The stage as the captures hold it.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Whether the last block was cancelled.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// `add_reference`: the system audio with its presentation time.
    pub fn add_reference(&self, samples: &[f32], time_s: f64, echo_possible: bool) {
        let samples: Vec<f64> = samples.iter().map(|&v| f64::from(v)).collect();
        let mut side = self.reference.lock().unwrap_or_else(PoisonError::into_inner);
        let (placed, jumped) = side.placer.place(&samples, time_s);
        let end = side.placer.next_position.unwrap_or_default();
        if jumped {
            side.ring = ReferenceRing::new();
        }
        #[allow(clippy::cast_possible_wrap)]
        side.ring.write(end - placed.len() as i64, &placed);
        side.echo_possible = echo_possible;
    }

    /// `add_mic`: whatever is ready of the microphone, in the order it came.
    pub fn add_mic(&self, samples: &[f32], time_s: f64) -> Vec<f32> {
        let samples: Vec<f64> = samples.iter().map(|&v| f64::from(v)).collect();
        let mut guard = self.mic.lock().unwrap_or_else(PoisonError::into_inner);
        let mic = &mut *guard;
        let (placed, jumped) = mic.placer.place(&samples, time_s);
        let mut out: Vec<f64> = Vec::new();
        if jumped || mic.position.is_none() {
            let pending = std::mem::take(&mut mic.pending);
            out.extend(raw_from_delay_line(&mut mic.raw_delay, &pending));
            #[allow(clippy::cast_possible_wrap)]
            let start = mic.placer.next_position.unwrap_or_default() - placed.len() as i64;
            mic.position = Some(start);
        }
        mic.pending.extend_from_slice(&placed);
        while mic.pending.len() >= BLOCK {
            let reference_end = self.reference.lock().unwrap_or_else(PoisonError::into_inner).ring.end;
            let position = mic.position.unwrap_or_default();
            #[allow(clippy::cast_possible_wrap)]
            let ready = reference_end.is_some_and(|end| end >= position + REFERENCE_LEAD + BLOCK as i64);
            if !ready && mic.pending.len() < BLOCK + REFERENCE_WAIT {
                break;
            }
            let block: Vec<f64> = mic.pending.drain(..BLOCK).collect();
            let processed = self.block(mic, &block);
            out.extend(processed);
            #[allow(clippy::cast_possible_wrap)]
            {
                mic.position = Some(position + BLOCK as i64);
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        out.iter().map(|&v| v as f32).collect()
    }

    /// `flush_raw`: what is still held, raw, as the stage is left.
    pub fn flush_raw(&self) -> Vec<f32> {
        let mut guard = self.mic.lock().unwrap_or_else(PoisonError::into_inner);
        let mic = &mut *guard;
        let pending = std::mem::take(&mut mic.pending);
        let mut out = raw_from_delay_line(&mut mic.raw_delay, &pending);
        out.extend_from_slice(&mic.raw_delay);
        #[allow(clippy::cast_possible_truncation)]
        out.iter().map(|&v| v as f32).collect()
    }

    fn block(&self, mic: &mut MicSide, block: &[f64]) -> Vec<f64> {
        let position = mic.position.unwrap_or_default();
        let (lead, has_reference, present, echo_possible) = {
            let side = self.reference.lock().unwrap_or_else(PoisonError::into_inner);
            let (lead, has_reference) = side.ring.read(position + REFERENCE_LEAD, BLOCK);
            let (present, _) = side.ring.read(position, BLOCK);
            (lead, has_reference, present, side.echo_possible)
        };
        let raw = raw_from_delay_line(&mut mic.raw_delay, block);
        mic.detector.feed(block, &present);
        let wanted = has_reference && echo_possible && mic.detector.present;
        self.active.store(wanted, Ordering::Relaxed);

        if !wanted && !mic.running {
            mic.raw_share = 1.0;
            return raw;
        }

        let cancelled = cancel(mic, block, &lead);
        let cold = mic.cold;
        mic.cold = false;
        mic.running = wanted;
        if !wanted {
            // One more block, to fade to raw from audio that exists; then Speex pauses
            // keeping its filter, which beats a running or a reset one on the way back.
            mic.cold = true;
        }
        mix(mic, &raw, &cancelled, &lead, wanted, cold)
    }
}

impl Default for EchoStage {
    fn default() -> Self {
        Self::new()
    }
}

/// `block` through the delay that keeps raw audio level with the suppressor's.
fn raw_from_delay_line(raw_delay: &mut Vec<f64>, block: &[f64]) -> Vec<f64> {
    let mut joined = std::mem::take(raw_delay);
    joined.extend_from_slice(block);
    *raw_delay = joined.split_off(block.len());
    joined
}

fn cancel(mic: &mut MicSide, block: &[f64], reference: &[f64]) -> Vec<f64> {
    let filtered = mic.inverse.block(&mic.highpass.block(block));
    let output = mic.speex.block(&filtered, reference);
    let mut out = Vec::with_capacity(BLOCK);
    for i in (0..BLOCK).step_by(HOP) {
        out.extend(mic.suppressor.hop(&output[i..i + HOP], &reference[i..i + HOP]));
    }
    out
}

/// A crossfade of one frame to raw or back, as each frame asks.
fn mix(mic: &mut MicSide, raw: &[f64], cancelled: &[f64], reference: &[f64], wanted: bool, cold: bool) -> Vec<f64> {
    let mut share = vec![0.0; BLOCK];
    let mut current = mic.raw_share;
    #[allow(clippy::cast_precision_loss)]
    let step = 1.0 / FRAME as f64;
    for i in (0..BLOCK).step_by(FRAME) {
        let quiet = std(&reference[i..i + FRAME]) < QUIET_RMS;
        mic.quiet_frames = if quiet { mic.quiet_frames + 1 } else { 0 };
        #[allow(clippy::cast_precision_loss)]
        let held = (mic.quiet_frames * FRAME) as f64 / f64::from(SR) >= QUIET_HOLD_S;
        let target = !wanted || cold || held;
        let signed = if target { step } else { -step };
        for (k, s) in share[i..i + FRAME].iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let ramp = current + (k + 1) as f64 * signed;
            *s = ramp.clamp(0.0, 1.0);
        }
        current = share[i + FRAME - 1];
    }
    mic.raw_share = current;
    share.iter().zip(raw).zip(cancelled).map(|((s, r), c)| s * r + (1.0 - s) * c).collect()
}

impl EchoSink for EchoStage {
    fn add_mic(&self, samples: &[f32], time_s: f64) -> Result<Vec<f32>, String> {
        Ok(EchoStage::add_mic(self, samples, time_s))
    }

    fn flush_raw(&self) -> Vec<f32> {
        EchoStage::flush_raw(self)
    }

    fn active(&self) -> bool {
        self.is_active()
    }

    fn add_reference(&self, samples: &[f32], time_s: f64, echo_possible: bool) -> Result<(), String> {
        EchoStage::add_reference(self, samples, time_s, echo_possible);
        Ok(())
    }
}
