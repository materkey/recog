//! `_Suppressor`: takes down what Speex leaves, in proportion to what the reference
//! plays.

use realfft::num_complex::Complex64;

use super::numpy::{abs_squared, convolve_same, hanning, irfft, mean, median, percentile_linear, rfft};
use super::{HOP, SR, SUPPRESSOR_REFERENCE_DELAY};

const N: usize = 512;
const H: usize = HOP;
const BINS: usize = N / 2 + 1;

/// The residual suppressor, one hop of 256 samples at a time.
#[derive(Debug, Clone)]
pub struct Suppressor {
    beta: f64,
    floor: f64,
    decay: f64,
    smooth: f64,
    win: Vec<f64>,
    memory: Vec<f64>,
    output_power: Vec<f64>,
    coupling: Vec<f64>,
    kernel: Vec<f64>,
    block_hops: usize,
    history: usize,
    output_buffer: Vec<f64>,
    reference_buffer: Vec<f64>,
    overlap: Vec<f64>,
    past: Vec<Vec<f64>>,
    ratios: Vec<Vec<f64>>,
    hops: usize,
}

impl Suppressor {
    /// `_Suppressor()` with its defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::with(2.0, 0.2, SUPPRESSOR_REFERENCE_DELAY)
    }

    #[must_use]
    pub fn with(beta: f64, floor: f64, delay: usize) -> Self {
        let win = hanning(N + 1)[..N].iter().map(|v| v.sqrt()).collect();
        // The coupling tracks causally: 30th percentile, over 30 s, of the median
        // output-to-reference ratio per 2 s.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let block_hops = (2.0 * f64::from(SR) / H as f64) as usize;
        Self {
            beta,
            floor,
            decay: 0.6,
            smooth: 0.6,
            win,
            memory: vec![0.0; BINS],
            output_power: vec![0.0; BINS],
            coupling: vec![0.0; BINS],
            kernel: vec![1.0 / 9.0; 9],
            block_hops,
            history: 15,
            output_buffer: vec![0.0; N],
            reference_buffer: vec![0.0; N + delay],
            overlap: vec![0.0; N],
            past: Vec::new(),
            ratios: Vec::new(),
            hops: 0,
        }
    }

    /// The window, for comparing with numpy's.
    #[must_use]
    pub fn window(&self) -> &[f64] {
        &self.win
    }

    /// One hop of Speex's output and the reference that went with it.
    pub fn hop(&mut self, output: &[f64], reference: &[f64]) -> Vec<f64> {
        shift_in(&mut self.output_buffer, output);
        shift_in(&mut self.reference_buffer, reference);
        let windowed: Vec<f64> = self.output_buffer.iter().zip(&self.win).map(|(a, w)| a * w).collect();
        let e = rfft(&windowed, N);
        let windowed: Vec<f64> = self.reference_buffer[..N].iter().zip(&self.win).map(|(a, w)| a * w).collect();
        let x = rfft(&windowed, N);
        for (m, xb) in self.memory.iter_mut().zip(&x) {
            let p = abs_squared(*xb);
            let d = self.decay * *m;
            *m = if p >= d { p } else { d };
        }
        let spread = convolve_same(&self.memory, &self.kernel);
        let power: Vec<f64> = e.iter().map(|z| abs_squared(*z)).collect();
        for (op, p) in self.output_power.iter_mut().zip(&power) {
            *op = self.smooth * *op + (1.0 - self.smooth) * p;
        }
        let gain: Vec<f64> = (0..BINS)
            .map(|i| {
                let g = 1.0 - self.beta * self.coupling[i] * spread[i] / (self.output_power[i] + 1e-14);
                g.clamp(self.floor, 1.0)
            })
            .collect();
        if mean(&spread) > 1e-2 {
            // unscaled FFT power: the reference above silence
            self.ratios.push(power.iter().zip(&spread).map(|(p, s)| p / (s + 1e-14)).collect());
        }
        self.hops += 1;
        if self.hops.is_multiple_of(self.block_hops) {
            if self.ratios.len() > self.block_hops / 3 {
                let medians =
                    (0..BINS).map(|i| median(&mut self.ratios.iter().map(|r| r[i]).collect::<Vec<_>>())).collect();
                self.past.push(medians);
                if self.past.len() > self.history {
                    self.past.drain(..self.past.len() - self.history);
                }
                self.coupling = (0..BINS)
                    .map(|i| percentile_linear(&mut self.past.iter().map(|r| r[i]).collect::<Vec<_>>(), 30.0))
                    .collect();
            }
            self.ratios.clear();
        }
        let scaled: Vec<Complex64> = gain.iter().zip(&e).map(|(g, z)| Complex64::new(g * z.re, g * z.im)).collect();
        let y = irfft(&scaled, N);
        for ((o, v), w) in self.overlap.iter_mut().zip(&y).zip(&self.win) {
            *o += v * w;
        }
        let out = self.overlap[..H].to_vec();
        self.overlap.copy_within(H.., 0);
        self.overlap[N - H..].iter_mut().for_each(|v| *v = 0.0);
        out
    }
}

impl Default for Suppressor {
    fn default() -> Self {
        Self::new()
    }
}

/// `buffer = np.r_[buffer[len(new):], new]`.
pub(crate) fn shift_in(buffer: &mut [f64], new: &[f64]) {
    let n = new.len().min(buffer.len());
    buffer.copy_within(n.., 0);
    let len = buffer.len();
    buffer[len - n..].copy_from_slice(&new[new.len() - n..]);
}
