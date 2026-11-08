//! `peak_correlation` and `EchoPathDetector`: does the microphone hear the reference?

use std::collections::VecDeque;

use realfft::num_complex::Complex64;

use super::numpy::{cumsum, irfft, median, rfft, std};
use super::suppressor::shift_in;
use super::{
    PATH_BAND_HZ, PATH_ENTER, PATH_HOP_S, PATH_LEAVE, PATH_LEAVE_WINDOWS, PATH_MAX_LEAD_S, PATH_WINDOW_S, QUIET_RMS, SR,
};

/// Peak normalised correlation in the speech band, with the reference leading.
///
/// `reference` is longer than `mic` by the largest lead and ends at the same moment.
#[must_use]
pub fn peak_correlation(mic: &[f64], reference: &[f64]) -> f64 {
    let max_lead = reference.len() - mic.len();
    let size = (reference.len() + mic.len()).next_power_of_two();
    // np.fft.rfftfreq(size, 1 / SR): k * (1 / (size * d)), which puts 3500 Hz exactly
    // on a bin edge for some sizes, so it is computed the same way.
    #[allow(clippy::cast_precision_loss)]
    let val = 1.0 / (size as f64 * (1.0 / f64::from(SR)));
    let band = |k: usize| {
        #[allow(clippy::cast_precision_loss)]
        let f = k as f64 * val;
        f >= PATH_BAND_HZ.0 && f <= PATH_BAND_HZ.1
    };
    let filtered = |x: &[f64]| -> Vec<Complex64> {
        rfft(x, size).into_iter().enumerate().map(|(k, z)| if band(k) { z } else { Complex64::new(0.0, 0.0) }).collect()
    };
    let m_spec = filtered(mic);
    let r_spec = filtered(reference);
    let r = irfft(&r_spec, size);
    let m = irfft(&m_spec, size);
    let r = &r[..reference.len()];
    let m = &m[..mic.len()];
    // correlation[k]: the microphone against the reference starting at k, so k == 0
    // is the largest lead and k == max_lead no lead at all.
    let cross: Vec<Complex64> = r_spec.iter().zip(&m_spec).map(|(a, b)| a * b.conj()).collect();
    let correlation = irfft(&cross, size);
    let squares: Vec<f64> = r.iter().map(|v| v * v).collect();
    let mut energy = vec![0.0];
    energy.extend(cumsum(&squares));
    let mic_norm = m.iter().map(|v| v * v).sum::<f64>().sqrt();
    let mut peak = f64::NEG_INFINITY;
    for k in 0..=max_lead {
        let reference_norm = (energy[mic.len() + k] - energy[k]).sqrt();
        let value = correlation[k].abs() / (mic_norm * reference_norm + 1e-12);
        if value > peak || value.is_nan() {
            peak = value;
        }
    }
    peak
}

/// Does the microphone hear the reference at all?
///
/// Starts absent. A silent reference says nothing about the path, so such windows do
/// not count.
#[derive(Debug, Clone)]
pub struct EchoPathDetector {
    window: usize,
    hop: usize,
    mic: Vec<f64>,
    reference: Vec<f64>,
    since_window: usize,
    recent: VecDeque<f64>,
    /// Whether the path is present.
    pub present: bool,
}

impl EchoPathDetector {
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (window, lead, hop) =
            ((PATH_WINDOW_S * rate) as usize, (PATH_MAX_LEAD_S * rate) as usize, (PATH_HOP_S * rate) as usize);
        Self {
            window,
            hop,
            mic: vec![0.0; window],
            reference: vec![0.0; window + lead],
            since_window: 0,
            recent: VecDeque::with_capacity(PATH_LEAVE_WINDOWS),
            present: false,
        }
    }

    pub fn feed(&mut self, mic: &[f64], reference: &[f64]) {
        shift_in(&mut self.mic, mic);
        shift_in(&mut self.reference, reference);
        self.since_window += mic.len();
        if self.since_window < self.hop {
            return;
        }
        self.since_window = 0;
        if std(&self.reference[self.reference.len() - self.window..]) < QUIET_RMS {
            return;
        }
        let peak = peak_correlation(&self.mic, &self.reference);
        if self.present {
            if self.recent.len() == PATH_LEAVE_WINDOWS {
                self.recent.pop_front();
            }
            self.recent.push_back(peak);
            if self.recent.len() == PATH_LEAVE_WINDOWS
                && median(&mut self.recent.iter().copied().collect::<Vec<_>>()) < PATH_LEAVE
            {
                self.present = false;
                self.recent.clear();
            }
        } else if peak > PATH_ENTER {
            self.present = true;
            self.recent.clear();
        }
    }
}

impl Default for EchoPathDetector {
    fn default() -> Self {
        Self::new(SR)
    }
}
