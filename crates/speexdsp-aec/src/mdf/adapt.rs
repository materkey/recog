//! Adaptation: the weight gradient with the AUMDF constraint, and the statistics
//! that set the next frame's learning rate (leak estimate, residual-to-error ratio,
//! per-bin step).

use super::cancel::FrameEnergies;
use super::spectral::{WEIGHT_SHIFT, adjust_prop, weighted_spectral_mul_conj};
use super::state::EchoCanceller;
use crate::fixed::{add32, extract16, mac16_32_q15, mult16_16, mult16_32_q15, pshr32, q15, shl32, sub16};
use crate::pseudo_float::PFloat;

/// "We don't really hope to get better than 33 dB (MIN_LEAK-3dB) attenuation."
const MIN_LEAK: PFloat = PFloat::new(20972, -22);
/// The AUMDF constraint brings weights to Q(15 + 16 − 5) for its FFTs and back up
/// by 3 (`NORMALIZE_SCALEDOWN`, `NORMALIZE_SCALEUP`).
const NORMALIZE_SCALEDOWN: u32 = 5;
const NORMALIZE_SCALEUP: u32 = 3;

impl EchoCanceller {
    /// Proportional rate shares, the gradient step on the background filter, and
    /// the circular-convolution constraint on one or two partitions (AUMDF).
    pub(super) fn update_weights(&mut self) {
        if self.adapted {
            adjust_prop(&self.weights, &mut self.prop);
        }
        if self.saturated == 0 {
            self.step_gradient();
        } else {
            self.saturated -= 1;
        }
        // "Alternatively Updated MDF": partition 0 always, one more in turn
        self.constrain_partition(0);
        if let Some(j) = self.partition_in_turn() {
            self.constrain_partition(j);
        }
    }

    /// The gradient step on every partition of the background filter, from the
    /// oldest: the error spectrum against the far end of the frame before.
    fn step_gradient(&mut self) {
        let nn = self.window_size;
        for j in (0..self.partitions).rev() {
            let share = PFloat::from_i32(self.prop[j] as i32).shl(-15);
            let history = &self.far_spectra[self.far_slot(j + 1)];
            weighted_spectral_mul_conj(&self.power_1, share, history, &self.error_spectrum, &mut self.phi);
            for (w, &step) in self.weights[j * nn..(j + 1) * nn].iter_mut().zip(&self.phi) {
                *w = w.wrapping_add(step);
            }
        }
    }

    /// The partition besides 0 that AUMDF constrains this frame. C tests
    /// `cancel_count % (M − 1) == j − 1` for every j, which picks at most one; a
    /// count that has wrapped negative picks none, as there.
    fn partition_in_turn(&self) -> Option<usize> {
        let m = self.partitions as i32;
        if m < 2 {
            return None;
        }
        let turn = self.cancel_count % (m - 1) + 1;
        (turn >= 1).then_some(turn as usize)
    }

    /// Zeroes the second half of partition `j`'s impulse response, through an
    /// inverse and a forward FFT, so the frequency-domain filter stays a linear
    /// convolution.
    fn constrain_partition(&mut self, j: usize) {
        let (n, nn) = (self.frame_size, self.window_size);
        let partition = j * nn..(j + 1) * nn;
        for (t, &w) in self.wtmp2.iter_mut().zip(&self.weights[partition.clone()]) {
            *t = extract16(pshr32(w, NORMALIZE_SCALEDOWN + 16));
        }
        self.fft.inverse(&self.wtmp2, &mut self.wtmp);
        self.wtmp[..n].fill(0);
        for t in self.wtmp[n..].iter_mut() {
            *t = ((*t as i32) << NORMALIZE_SCALEUP) as i16;
        }
        self.fft.forward(&mut self.wtmp, &mut self.wtmp2);
        // "The -1 in the shift is a sort of kludge that trades less efficient
        // update speed for decrease noise"
        let shift = 16 + NORMALIZE_SCALEDOWN - NORMALIZE_SCALEUP - 1;
        for (w, &t) in self.weights[partition].iter_mut().zip(&self.wtmp2) {
            *w = w.wrapping_sub(shl32(t as i32, shift));
        }
    }

    /// Far-end power, correlations, leak estimate and RER, then the per-bin
    /// learning rate `power_1` for the next frame's gradient.
    pub(super) fn update_statistics(&mut self, f: &FrameEnergies, ss: i32, ss_1: i32) {
        for (p, &x) in self.power.iter_mut().zip(&self.xf) {
            *p = mult16_32_q15(ss_1, *p).wrapping_add(1).wrapping_add(mult16_32_q15(ss, x));
        }
        let leak = self.leak_estimate(f);
        let rer = Self::residual_to_error_ratio(f, leak);
        self.check_adapted(f, leak);
        if self.adapted {
            self.adapted_learning_rate(rer, leak);
        } else {
            self.initial_learning_rate(f);
        }
    }

    /// The leak estimate: how much of the echo estimate is still in the error, the
    /// slope of a recursive regression of error spectrum on echo spectrum. Updates
    /// the regression's correlations.
    fn leak_estimate(&mut self, f: &FrameEnergies) -> i16 {
        let (pey, pyy) = self.smoothed_correlations();
        let pyy = pyy.sqrt();
        let pey = pey.divu(pyy);
        // correlation update rate
        let rate = mult16_32_q15(self.beta0 as i32, f.syy).min(mult16_32_q15(self.beta_max as i32, f.see));
        let alpha = PFloat::div32(rate, f.see);
        let alpha_1 = PFloat::one_minus(alpha);
        self.pey = alpha_1.mult(self.pey).add(alpha.mult(pey));
        self.pyy = alpha_1.mult(self.pyy).add(alpha.mult(pyy));
        if self.pyy.lt(PFloat::ONE) {
            self.pyy = PFloat::ONE;
        }
        if self.pey.lt(MIN_LEAK.mult(self.pyy)) {
            self.pey = MIN_LEAK.mult(self.pyy);
        }
        if self.pey.gt(self.pyy) {
            self.pey = self.pyy;
        }
        // Q14 to Q15: "looks like a stupid bug, but it's right"
        let leak = self.pey.divu(self.pyy).shl(14).extract16();
        if leak > 16383 { 32767 } else { ((leak as i32) << 1) as i16 }
    }

    /// Whether the filter has had minimal adaptation: enough adaptation spent, and
    /// a leak estimate above 3%.
    fn check_adapted(&mut self, f: &FrameEnergies, leak: i16) {
        if !self.adapted
            && self.sum_adapt > shl32(self.partitions as i32, 15)
            && mult16_32_q15(leak as i32, f.syy) > mult16_32_q15(q15::POINT_03, f.syy)
        {
            self.adapted = true;
        }
    }

    /// Correlation of the error and echo spectra over the bins, against their
    /// recursive averages, which are updated on the way.
    fn smoothed_correlations(&mut self) -> (PFloat, PFloat) {
        let (mut pey, mut pyy) = (PFloat::ONE, PFloat::ONE);
        let average = self.spec_average as i32;
        let keep = sub16(32767, average);
        for j in (0..=self.frame_size).rev() {
            let eh = PFloat::from_i32(self.rf[j].wrapping_sub(self.eh[j]));
            let yh = PFloat::from_i32(self.yf[j].wrapping_sub(self.yh[j]));
            pey = pey.add(eh.mult(yh));
            pyy = pyy.add(yh.mult(yh));
            self.eh[j] = mac16_32_q15(mult16_32_q15(keep, self.eh[j]), average, self.rf[j]);
            self.yh[j] = mac16_32_q15(mult16_32_q15(keep, self.yh[j]), average, self.yf[j]);
        }
        (pey, pyy)
    }

    /// RER, bounded below by the echo still correlated with the error and above by ½.
    fn residual_to_error_ratio(f: &FrameEnergies, leak: i16) -> i16 {
        let mut tmp32 = mult16_32_q15(leak as i32, f.syy);
        tmp32 = add32(f.sxx >> 13, add32(tmp32, shl32(tmp32, 1)));
        let sey = PFloat::from_i32(f.sey);
        let bound = sey.mult(sey).divu(PFloat::from_i32(add32(1, f.syy)));
        tmp32 = if bound.gt(PFloat::from_i32(f.see)) { f.see } else { tmp32.max(bound.extract32()) };
        PFloat::div32(tmp32.min(f.see >> 1), f.see).shl(15).extract16()
    }

    /// Per-bin step once adapted: leak-weighted echo over error, mixed with RER.
    fn adapted_learning_rate(&mut self, rer: i16, leak: i16) {
        for i in 0..=self.frame_size {
            let e = shl32(self.rf[i], 3).wrapping_add(1);
            let r = mult16_32_q15(leak as i32, shl32(self.yf[i], 3)).min(e >> 1);
            let r =
                mult16_32_q15(q15::POINT_7, r).wrapping_add(mult16_32_q15(q15::POINT_3, mult16_32_q15(rer as i32, e)));
            let denominator = PFloat::mul32u(e, self.power[i].wrapping_add(10));
            self.power_1[i] = PFloat::div32_float(r, denominator).shl(WEIGHT_SHIFT as i32 + 16);
        }
    }

    /// The temporary step before the filter has adapted, from the far-end energy.
    fn initial_learning_rate(&mut self, f: &FrameEnergies) {
        let mut adapt_rate: i16 = 0;
        if f.sxx > mult16_16(self.window_size as i32, 1000) >> 6 {
            let tmp32 = mult16_32_q15(q15::POINT_25, f.sxx).min(f.see >> 2);
            adapt_rate = PFloat::div32(tmp32, f.see).shl(15).extract16();
        }
        for (rate, &p) in self.power_1.iter_mut().zip(&self.power) {
            *rate = PFloat::div32(adapt_rate as i32, add32(p, 10)).shl(WEIGHT_SHIFT as i32 + 1);
        }
        self.sum_adapt = add32(self.sum_adapt, adapt_rate as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::super::settings::Config;
    use super::*;

    /// C's AUMDF loop: every partition `j > 0` with `count % (M − 1) == j − 1`.
    fn c_partitions_in_turn(m: usize, count: i32) -> Vec<usize> {
        (1..m).filter(|&j| count % (m as i32 - 1) == j as i32 - 1).collect()
    }

    #[test]
    fn adaptation_starts_only_above_a_far_end_energy_of_n_times_1000_over_64() {
        let mut aec = EchoCanceller::new(Config::new(160, 1600)).unwrap();
        let at = FrameEnergies { sxx: (320 * 1000) >> 6, see: 1 << 20, sey: 0, syy: 0 };
        aec.initial_learning_rate(&at);
        assert_eq!(aec.sum_adapt, 0);
        aec.initial_learning_rate(&FrameEnergies { sxx: at.sxx + 1, ..at });
        assert!(aec.sum_adapt > 0, "{}", aec.sum_adapt);
    }

    #[test]
    fn the_partition_in_turn_is_the_one_cs_loop_picks() {
        for m in 1..=40 {
            let mut aec = EchoCanceller::new(Config::new(8, 8 * m)).unwrap();
            assert_eq!(aec.partitions, m);
            for count in (-200..=200).chain([i32::MIN, i32::MIN + 1, i32::MAX - 1, i32::MAX]) {
                aec.cancel_count = count;
                let ours: Vec<usize> = aec.partition_in_turn().into_iter().collect();
                assert_eq!(ours, c_partitions_in_turn(m, count), "M = {m}, cancel_count = {count}");
            }
        }
    }
}
