//! `inverse_low_cut_fir`: a causal inverse of what Speex does to a microphone with a
//! silent reference, measured from the canceller once per process.
//!
//! The noise is numpy's `default_rng(0).normal(0, 0.02, 30 * SR)` draw for draw
//! ([`super::random`]) and Speex is the bit-exact port, so the measurement starts from
//! Python's very samples; what differs is the FFT (realfft for pocketfft) and the solve
//! (Levinson's recursion for LAPACK's LU), a few parts in 10^10 of the taps.

use std::sync::{Arc, OnceLock};

use realfft::num_complex::Complex64;

use super::SR;
use super::filters::Speex;
use super::numpy::{abs_squared, irfft, rfft};
use super::random::Pcg64;

/// The default number of taps.
pub const TAPS: usize = 1024;
/// The default regularization of the autocorrelation's zero lag.
pub const REGULARIZATION: f64 = 1e-3;

/// `inverse_low_cut_fir(taps, regularization)`.
///
/// # Panics
/// When `taps` is zero or longer than the measurement.
#[must_use]
pub fn inverse_low_cut_fir(taps: usize, regularization: f64) -> Vec<f64> {
    let sr = SR as usize;
    let noise = Pcg64::seed0().normal(0.0, 0.02, 30 * sr);
    let thinned = Speex::new().block(&noise, &vec![0.0; noise.len()]);
    let (x, y) = (&thinned[sr..], &noise[sr..]);
    let size = (x.len() + taps).next_power_of_two();
    let (big_x, big_y) = (rfft(x, size), rfft(y, size));
    let power: Vec<Complex64> = big_x.iter().map(|z| Complex64::new(abs_squared(*z), 0.0)).collect();
    let mut autocorrelation = irfft(&power, size);
    autocorrelation.truncate(taps);
    let cross_spectrum: Vec<Complex64> = big_y.iter().zip(&big_x).map(|(a, b)| a * b.conj()).collect();
    let mut cross = irfft(&cross_spectrum, size);
    cross.truncate(taps);
    autocorrelation[0] *= 1.0 + regularization;
    solve_symmetric_toeplitz(&autocorrelation, &cross)
}

/// The process's inverse filter with the default settings, measured on first use (it
/// takes about a second and never changes in a process).
pub fn shared_inverse_fir() -> Arc<Vec<f64>> {
    static FIR: OnceLock<Arc<Vec<f64>>> = OnceLock::new();
    FIR.get_or_init(|| Arc::new(inverse_low_cut_fir(TAPS, REGULARIZATION))).clone()
}

/// Solves `T h = y` for the symmetric Toeplitz `T` whose first row is `t`, by Levinson's
/// recursion.
///
/// # Panics
/// When the lengths differ or are zero.
#[must_use]
pub fn solve_symmetric_toeplitz(t: &[f64], y: &[f64]) -> Vec<f64> {
    let n = t.len();
    assert!(n > 0 && y.len() == n, "a non-empty system");
    let mut forward = vec![1.0 / t[0]];
    let mut x = vec![y[0] / t[0]];
    for k in 1..n {
        let ef: f64 = (0..k).map(|i| t[k - i] * forward[i]).sum();
        let denom = 1.0 - ef * ef;
        let mut next = vec![0.0; k + 1];
        for (i, v) in next.iter_mut().enumerate() {
            let f = if i < k { forward[i] } else { 0.0 };
            // (0; reverse(forward))[i] = forward[k - i] for i >= 1
            let b = if i >= 1 { forward[k - i] } else { 0.0 };
            *v = (f - ef * b) / denom;
        }
        forward = next;
        let ex: f64 = (0..k).map(|i| t[k - i] * x[i]).sum();
        let scale = y[k] - ex;
        x.push(0.0);
        for (i, v) in x.iter_mut().enumerate() {
            *v += scale * forward[k - i];
        }
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_system_toeplitz_solve_gives_the_solution() {
        let t = [4.0, 1.0, 0.5];
        let h = [1.0, -2.0, 3.0];
        let y: Vec<f64> =
            (0..3).map(|i| (0..3).map(|j| t[(i as i64 - j as i64).unsigned_abs() as usize] * h[j]).sum()).collect();
        let got = solve_symmetric_toeplitz(&t, &y);
        for (a, b) in got.iter().zip(&h) {
            assert!((a - b).abs() < 1e-12, "{got:?}");
        }
    }
}
