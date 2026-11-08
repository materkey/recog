//! Synthetic signals of the echo tests, the shapes `tests/test_echo_cancel.py` builds:
//! speech-band noise in bursts, and what a microphone hears of it from the speakers.
#![allow(dead_code)]

use recog_live::echo::numpy::{Complex64, convolve_full, irfft, rfft, std};
use recog_live::echo::{EchoStage, SR};

pub const ECHO_DELAY_S: f64 = 0.05;
pub const START_S: f64 = 1000.0;
pub const SRU: usize = SR as usize;

/// A normal generator (Box-Muller over xorshift64*), deterministic per seed.
pub struct Normal(u64);

impl Normal {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn uniform(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    pub fn sample(&mut self, sigma: f64) -> f64 {
        let (u, v) = (self.uniform(), self.uniform());
        sigma * (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }

    pub fn vec(&mut self, sigma: f64, n: usize) -> Vec<f64> {
        (0..n).map(|_| self.sample(sigma)).collect()
    }
}

/// Noise shaped to the speech band, in bursts with short pauses.
pub fn speech_like(seconds: f64, seed: u64, level: f64) -> Vec<f32> {
    let n = (seconds * f64::from(SR)) as usize;
    let noise = Normal::new(seed).vec(1.0, n);
    let mut spectrum = rfft(&noise, n);
    for (k, bin) in spectrum.iter_mut().enumerate() {
        let f = k as f64 * f64::from(SR) / n as f64;
        if !(200.0..=3500.0).contains(&f) {
            *bin = Complex64::new(0.0, 0.0);
        }
    }
    let x = irfft(&spectrum, n);
    let s = std(&x);
    x.iter()
        .enumerate()
        .map(|(i, v)| {
            let gate = (2.0 * std::f64::consts::PI * 0.7 * i as f64 / f64::from(SR)).sin() > -0.7;
            (v / s * if gate { level } else { 0.0 }) as f32
        })
        .collect()
}

/// What the microphone hears: delayed, filtered, with room noise.
pub fn echo_of(reference: &[f32]) -> Vec<f32> {
    let mut room = vec![0.0; (0.06 * f64::from(SR)) as usize];
    for (lag_ms, gain) in [(0, 0.4), (3, -0.25), (11, 0.12), (37, 0.05)] {
        room[lag_ms * SRU / 1000] = gain;
    }
    let delay = (ECHO_DELAY_S * f64::from(SR)) as usize;
    let mut delayed = vec![0.0; delay];
    delayed.extend(reference.iter().map(|&v| f64::from(v)));
    let echo = convolve_full(&delayed, &room);
    let noise = Normal::new(99).vec(0.0007, reference.len());
    (0..reference.len()).map(|i| (echo[i] + noise[i]) as f32).collect()
}

/// Where a microphone block starts and what it holds, or None to skip it.
pub type MicBlockAt<'a> = dyn Fn(usize, &[f32]) -> Option<(usize, Vec<f32>)> + 'a;

/// One call into the stage: a reference block, a microphone block, or the flush.
#[derive(Clone)]
pub enum Event {
    Reference { samples: Vec<f32>, t: f64, echo_possible: bool },
    Mic { samples: Vec<f32>, t: f64 },
    Flush,
}

/// The events of feeding both streams in real-time order, as `_run` does: the
/// reference in blocks of `ref_block` up to the microphone block's end, then the
/// microphone block. `mic_block_at` may cut or skip a microphone block (holes) or
/// replace its samples, and `echo_possible_at` gives the flag per reference block.
pub fn events(
    mic: &[f32],
    reference: Option<&[f32]>,
    block: usize,
    ref_block: usize,
    echo_possible_at: &dyn Fn(usize) -> bool,
    mic_block_at: &MicBlockAt<'_>,
) -> Vec<Event> {
    let mut out = Vec::new();
    let mut ref_at = 0;
    let mut i = 0;
    while i + block <= mic.len() {
        if let Some(reference) = reference {
            while ref_at + ref_block <= reference.len() && ref_at <= i + block {
                out.push(Event::Reference {
                    samples: reference[ref_at..ref_at + ref_block].to_vec(),
                    t: START_S + ref_at as f64 / f64::from(SR),
                    echo_possible: echo_possible_at(i),
                });
                ref_at += ref_block;
            }
        }
        if let Some((start, samples)) = mic_block_at(i, &mic[i..i + block]) {
            out.push(Event::Mic { samples, t: START_S + start as f64 / f64::from(SR) });
        }
        i += block;
    }
    out
}

/// The plain `_run` events: every block whole, one flag throughout.
pub fn plain_events(mic: &[f32], reference: Option<&[f32]>, echo_possible: bool) -> Vec<Event> {
    events(mic, reference, 160, 341, &|_| echo_possible, &|i, b| Some((i, b.to_vec())))
}

/// Feeds `events` to `stage`; the output of each microphone call (and the flush), and
/// whether the stage was active after it.
pub fn feed(stage: &EchoStage, events: &[Event]) -> Vec<(Vec<f32>, bool)> {
    let mut calls = Vec::new();
    for event in events {
        match event {
            Event::Reference { samples, t, echo_possible } => stage.add_reference(samples, *t, *echo_possible),
            Event::Mic { samples, t } => calls.push((stage.add_mic(samples, *t), stage.is_active())),
            Event::Flush => calls.push((stage.flush_raw(), stage.is_active())),
        }
    }
    calls
}

/// `_run`: what the stage gave back, joined, and whether it ended active.
pub fn run(mic: &[f32], reference: Option<&[f32]>, echo_possible: bool) -> (Vec<f32>, bool) {
    let stage = EchoStage::new();
    let calls = feed(&stage, &plain_events(mic, reference, echo_possible));
    (calls.into_iter().flat_map(|(out, _)| out).collect(), stage.is_active())
}

pub fn rms(x: &[f32]) -> f64 {
    let squares: Vec<f64> = x.iter().map(|&v| f64::from(v) * f64::from(v)).collect();
    recog_live::echo::numpy::mean(&squares).sqrt()
}

/// How many samples `out` lags behind `mic`.
pub fn delay_of(out: &[f32], mic: &[f32]) -> usize {
    let n = 2 * SRU;
    let dots: Vec<f64> = (0..4000)
        .map(|lag| out[lag..lag + n].iter().zip(&mic[..n]).map(|(o, m)| f64::from(*o) * f64::from(*m)).sum())
        .collect();
    // The first of equal maxima, as Python's max().
    let mut best = 0;
    for (lag, &d) in dots.iter().enumerate() {
        if d > dots[best] {
            best = lag;
        }
    }
    best
}
