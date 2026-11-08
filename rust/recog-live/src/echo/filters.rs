//! The chain's filters: the high-pass, the inverse FIR and Speex itself.

use speexdsp_aec::{Config, DEFAULT_SAMPLING_RATE, EchoCanceller};

use super::numpy::convolve_full;
use super::{FRAME, SR, TAIL};

/// `_to16`: `np.clip(x * 32767, -32768, 32767).astype(np.int16)`, truncating toward
/// zero as numpy's cast does.
#[must_use]
pub fn to16(x: f64) -> i16 {
    #[allow(clippy::cast_possible_truncation)]
    let v = (x * 32767.0).clamp(-32768.0, 32767.0) as i16;
    v
}

/// `_Speex`: the canceller with pyaec's 8 kHz settings whatever the rate, a block of
/// whole frames at a time, as the Python module is called.
pub struct Speex {
    aec: EchoCanceller,
}

impl Speex {
    /// # Panics
    /// Never: the settings are fixed and valid.
    #[must_use]
    pub fn new() -> Self {
        let config = Config { frame_size: FRAME, filter_length: TAIL, sampling_rate: DEFAULT_SAMPLING_RATE };
        Self { aec: EchoCanceller::new(config).expect("the fixed Speex settings are valid") }
    }

    /// The microphone block with the echo of `reference` taken out, in [-1, 1).
    ///
    /// # Panics
    /// When the two blocks differ in length or are not whole frames.
    pub fn block(&mut self, mic: &[f64], reference: &[f64]) -> Vec<f64> {
        assert!(mic.len() == reference.len() && mic.len().is_multiple_of(FRAME), "whole frames of both streams");
        let m: Vec<i16> = mic.iter().map(|&v| to16(v)).collect();
        let r: Vec<i16> = reference.iter().map(|&v| to16(v)).collect();
        let mut out = vec![0i16; m.len()];
        for ((mf, rf), of) in
            m.as_chunks::<FRAME>().0.iter().zip(r.as_chunks::<FRAME>().0).zip(out.as_chunks_mut::<FRAME>().0)
        {
            self.aec.cancel(mf, rf, of).expect("frames are checked above");
        }
        out.iter().map(|&v| f64::from(v) / 32768.0).collect()
    }
}

impl Default for Speex {
    fn default() -> Self {
        Self::new()
    }
}

/// `_HighPass`: second-order Butterworth, its arithmetic in Python's order.
#[derive(Debug, Clone)]
pub struct HighPass {
    b0: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl HighPass {
    #[must_use]
    pub fn new(cutoff: f64) -> Self {
        let k = (std::f64::consts::PI * cutoff / f64::from(SR)).tan();
        let sqrt2 = 2.0_f64.sqrt();
        let norm = 1.0 / (1.0 + sqrt2 * k + k * k);
        Self { b0: norm, a1: 2.0 * (k * k - 1.0) * norm, a2: (1.0 - sqrt2 * k + k * k) * norm, z1: 0.0, z2: 0.0 }
    }

    pub fn block(&mut self, x: &[f64]) -> Vec<f64> {
        let (b0, a1, a2) = (self.b0, self.a1, self.a2);
        let (mut z1, mut z2) = (self.z1, self.z2);
        let out = x
            .iter()
            .map(|&sample| {
                let y = b0 * sample + z1;
                z1 = -2.0 * b0 * sample - a1 * y + z2;
                z2 = b0 * sample - a2 * y;
                y
            })
            .collect();
        self.z1 = z1;
        self.z2 = z2;
        out
    }
}

/// `_Fir`: block convolution carrying the tail into the next block.
#[derive(Debug, Clone)]
pub struct Fir {
    taps: Vec<f64>,
    tail: Vec<f64>,
}

impl Fir {
    #[must_use]
    pub fn new(taps: Vec<f64>) -> Self {
        let tail = vec![0.0; taps.len().saturating_sub(1)];
        Self { taps, tail }
    }

    pub fn block(&mut self, x: &[f64]) -> Vec<f64> {
        let mut full = convolve_full(x, &self.taps);
        for (f, t) in full.iter_mut().zip(&self.tail) {
            *f += *t;
        }
        self.tail = full[x.len()..].to_vec();
        full.truncate(x.len());
        full
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_to16_truncates_toward_zero_and_saturates() {
        assert_eq!(to16(0.5), 16383);
        assert_eq!(to16(-0.5), -16383);
        assert_eq!(to16(2.0), 32767);
        assert_eq!(to16(-2.0), -32768);
    }

    #[test]
    fn constant_signal_high_pass_settles_to_zero() {
        let mut hp = HighPass::new(60.0);
        let out = hp.block(&vec![1.0; 16000]);
        assert!(out[15999].abs() < 1e-6);
    }

    #[test]
    fn blocks_fir_gives_the_same_as_one_convolution() {
        let x: Vec<f64> = (0..100).map(|i| f64::from(i % 7) - 3.0).collect();
        let taps = vec![0.5, -0.25, 0.125];
        let whole = convolve_full(&x, &taps);
        let mut fir = Fir::new(taps);
        let mut out = fir.block(&x[..37]);
        out.extend(fir.block(&x[37..]));
        for (a, b) in out.iter().zip(&whole) {
            assert!((a - b).abs() < 1e-12);
        }
    }
}
