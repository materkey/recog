//! The numpy operations `recog/echo_cancel.py` decides with, computed the way numpy
//! computes them, so the decisions come out the same.
//!
//! Exact: `np.sum`/`np.mean`/`np.std` of a contiguous float64 array (numpy's pairwise
//! summation, checked against numpy on sizes 3 to 100 000), `np.median`,
//! `np.percentile(..., method="linear")`, `np.cumsum`, `np.hanning` (cos from the
//! platform's libm, as numpy's float64 cos on macOS).
//!
//! Not exact: the FFTs (numpy's pocketfft orders its butterflies and twiddles its own
//! way; here realfft) and `np.convolve` (numpy's inner products go through BLAS `ddot`,
//! whose summation order is the library's): both agree with numpy to a few ulps.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

pub use realfft::num_complex::Complex64;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

/// numpy's `PW_BLOCKSIZE`.
const PW_BLOCKSIZE: usize = 128;

/// `np.add.reduce` of a contiguous float64 array: numpy's `pairwise_sum` from 0.
#[must_use]
pub fn sum(a: &[f64]) -> f64 {
    0.0 + pairwise_sum(a)
}

fn pairwise_sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut res = 0.0;
        for &v in a {
            res += v;
        }
        res
    } else if n <= PW_BLOCKSIZE {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - (n % 8) {
            for (k, acc) in r.iter_mut().enumerate() {
                *acc += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise_sum(&a[..n2]) + pairwise_sum(&a[n2..])
    }
}

/// `np.mean` of a non-empty float64 array.
#[must_use]
pub fn mean(a: &[f64]) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = a.len() as f64;
    sum(a) / n
}

/// `np.std` (population) of a non-empty float64 array: numpy's `_var` then `sqrt`.
#[must_use]
pub fn std(a: &[f64]) -> f64 {
    let m = mean(a);
    let squares: Vec<f64> = a
        .iter()
        .map(|&v| {
            let d = v - m;
            d * d
        })
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let n = a.len() as f64;
    (sum(&squares) / n).sqrt()
}

/// `np.median` of a non-empty array without NaN.
#[must_use]
pub fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 { values[n / 2] } else { (values[n / 2 - 1] + values[n / 2]) / 2.0 }
}

/// `np.percentile(values, q)` with the default linear method, of a non-empty array
/// without NaN: numpy's virtual index `(n - 1) * q / 100` and its two-sided `_lerp`.
#[must_use]
pub fn percentile_linear(values: &mut [f64], q: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    #[allow(clippy::cast_precision_loss)]
    let virtual_index = (n - 1) as f64 * (q / 100.0);
    let floor = virtual_index.floor();
    let (previous, next) = if virtual_index >= (n - 1) as f64 {
        (n - 1, n - 1)
    } else {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let p = floor as usize;
        (p, p + 1)
    };
    let gamma = virtual_index - floor;
    let (a, b) = (values[previous], values[next]);
    let diff = b - a;
    if gamma >= 0.5 { b - diff * (1.0 - gamma) } else { a + diff * gamma }
}

/// `np.cumsum`: a running sum, left to right.
#[must_use]
pub fn cumsum(a: &[f64]) -> Vec<f64> {
    let mut total = 0.0;
    a.iter()
        .map(|&v| {
            total += v;
            total
        })
        .collect()
}

/// `np.hanning(m)`: `0.5 + 0.5 * cos(pi * n / (m - 1))` for `n` in `1-m, 3-m, ..., m-1`.
#[must_use]
pub fn hanning(m: usize) -> Vec<f64> {
    if m == 0 {
        return Vec::new();
    }
    if m == 1 {
        return vec![1.0];
    }
    #[allow(clippy::cast_precision_loss)]
    let mf = m as f64;
    (0..m)
        .map(|k| {
            #[allow(clippy::cast_precision_loss)]
            let n = (1.0 - mf) + 2.0 * k as f64;
            0.5 + 0.5 * (std::f64::consts::PI * n / (mf - 1.0)).cos()
        })
        .collect()
}

/// `np.convolve(a, v)` (mode "full").
#[must_use]
pub fn convolve_full(a: &[f64], v: &[f64]) -> Vec<f64> {
    if a.is_empty() || v.is_empty() {
        return Vec::new();
    }
    let n = a.len() + v.len() - 1;
    let mut out = vec![0.0; n];
    for (i, &x) in a.iter().enumerate() {
        for (j, &h) in v.iter().enumerate() {
            out[i + j] += x * h;
        }
    }
    out
}

/// `np.convolve(a, v, mode="same")` for a kernel no longer than `a`.
#[must_use]
pub fn convolve_same(a: &[f64], v: &[f64]) -> Vec<f64> {
    let full = convolve_full(a, v);
    let start = (v.len() - 1) / 2;
    full[start..start + a.len()].to_vec()
}

thread_local! {
    static PLANNER: RefCell<RealFftPlanner<f64>> = RefCell::new(RealFftPlanner::new());
    static FORWARD: RefCell<HashMap<usize, Arc<dyn RealToComplex<f64>>>> = RefCell::new(HashMap::new());
    static INVERSE: RefCell<HashMap<usize, Arc<dyn ComplexToReal<f64>>>> = RefCell::new(HashMap::new());
}

/// `np.fft.rfft(x, n)`: `x` cut or zero-padded to `n`, `n / 2 + 1` bins.
#[must_use]
pub fn rfft(x: &[f64], n: usize) -> Vec<Complex64> {
    let plan = FORWARD.with(|plans| {
        plans.borrow_mut().entry(n).or_insert_with(|| PLANNER.with(|p| p.borrow_mut().plan_fft_forward(n))).clone()
    });
    let mut input = vec![0.0; n];
    let take = x.len().min(n);
    input[..take].copy_from_slice(&x[..take]);
    let mut output = plan.make_output_vec();
    plan.process(&mut input, &mut output).unwrap_or_else(|e| unreachable!("rfft of {n}: {e}"));
    output
}

/// `np.fft.irfft(spectrum, n)`: the first `n / 2 + 1` bins (zero-padded), the
/// imaginary parts of the DC and Nyquist bins ignored, scaled by `1 / n`.
#[must_use]
pub fn irfft(spectrum: &[Complex64], n: usize) -> Vec<f64> {
    let plan = INVERSE.with(|plans| {
        plans.borrow_mut().entry(n).or_insert_with(|| PLANNER.with(|p| p.borrow_mut().plan_fft_inverse(n))).clone()
    });
    let mut input = plan.make_input_vec();
    let take = spectrum.len().min(input.len());
    input[..take].copy_from_slice(&spectrum[..take]);
    input[0].im = 0.0;
    if n.is_multiple_of(2) {
        let last = input.len() - 1;
        input[last].im = 0.0;
    }
    let mut output = plan.make_output_vec();
    plan.process(&mut input, &mut output).unwrap_or_else(|e| unreachable!("irfft of {n}: {e}"));
    #[allow(clippy::cast_precision_loss)]
    let scale = 1.0 / n as f64;
    output.iter_mut().for_each(|v| *v *= scale);
    output
}

/// `abs(z) ** 2` of numpy: the square of `hypot(re, im)`.
#[must_use]
pub fn abs_squared(z: Complex64) -> f64 {
    let a = z.re.hypot(z.im);
    a * a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_array_sum_adds_left_to_right_from_zero() {
        assert_eq!(sum(&[0.1, 0.2, 0.3]), (0.0 + 0.1 + 0.2) + 0.3);
        assert_eq!(sum(&[]), 0.0);
    }

    #[test]
    fn odd_and_even_counts_median_takes_the_middle_or_the_mean_of_two() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
    }

    #[test]
    fn known_values_percentile_linear_interpolates_like_numpy() {
        // np.percentile([1, 2, 3, 4], 30) == 1.9; one value is itself.
        assert!((percentile_linear(&mut [4.0, 1.0, 3.0, 2.0], 30.0) - 1.9).abs() < 1e-15);
        assert_eq!(percentile_linear(&mut [5.0], 30.0), 5.0);
        assert_eq!(percentile_linear(&mut [1.0, 2.0], 100.0), 2.0);
    }

    #[test]
    fn hanning_window_ends_at_zero_and_peaks_at_one() {
        let w = hanning(5);
        assert_eq!(w.len(), 5);
        assert!(w[0].abs() < 1e-16 && (w[2] - 1.0).abs() < 1e-16);
    }

    #[test]
    fn impulse_convolve_same_keeps_the_signal_centred() {
        let out = convolve_same(&[0.0, 1.0, 0.0, 0.0, 0.0], &[1.0, 2.0, 3.0]);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 0.0, 0.0]);
    }

    #[test]
    fn rfft_then_irfft_gives_the_signal_back() {
        let x: Vec<f64> = (0..64).map(|i| f64::from(i).sin()).collect();
        let back = irfft(&rfft(&x, 64), 64);
        for (a, b) in x.iter().zip(&back) {
            assert!((a - b).abs() < 1e-12);
        }
    }
}
