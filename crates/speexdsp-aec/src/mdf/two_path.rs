//! The two-path filter of `speex_echo_cancellation`: a background filter adapts
//! every frame, a foreground filter produces the output, and the background is
//! copied into the foreground when it is significantly better, or restored from it
//! when it is significantly worse. "Significantly" is judged on two recursive
//! averages of the error-energy difference against their estimated variances.

use super::state::EchoCanceller;
use crate::fixed::{abs32, add32, extract16, mult16_16_q, mult16_32_q15, pshr32, q15, shl32, sub16, sub32};
use crate::pseudo_float::PFloat;

/// Smoothing of the two variance estimators.
const VAR1_SMOOTH: PFloat = PFloat::new(23593, -16);
const VAR2_SMOOTH: PFloat = PFloat::new(23675, -15);
/// How far above its variance an improvement must be.
const VAR1_UPDATE: PFloat = PFloat::new(16384, -15);
const VAR2_UPDATE: PFloat = PFloat::new(16384, -16);
/// How far above its variance a regression must be.
const VAR_BACKTRACK: PFloat = PFloat::new(16384, -12);

/// `v · |v|` as a pseudo-float: a signed square.
fn signed_square(v: i32) -> PFloat {
    PFloat::mul32u(v, abs32(v))
}

impl EchoCanceller {
    /// Chooses between the two filters for this frame. `sff` and `see` are the
    /// foreground and background error energies, `dbf` the energy of the
    /// difference between their echo estimates. Returns the error energy to adapt
    /// on.
    pub(super) fn choose_filter(&mut self, sff: i32, see: i32, dbf: i32) -> i32 {
        let diff = self.track_variances(sff, see, dbf);
        if self.background_is_better(diff, sff, dbf) {
            self.adopt_background();
            see
        } else if self.background_is_worse(diff, sff, dbf) {
            self.restore_background();
            sff
        } else {
            see
        }
    }

    /// Updates both averages of `Sff − See` and their variances; returns the
    /// difference.
    fn track_variances(&mut self, sff: i32, see: i32, dbf: i32) -> i32 {
        let diff = sub32(sff, see);
        self.davg1 = add32(mult16_32_q15(q15::POINT_6, self.davg1), mult16_32_q15(q15::POINT_4, diff));
        self.davg2 = add32(mult16_32_q15(q15::POINT_85, self.davg2), mult16_32_q15(q15::POINT_15, diff));
        let var1 = PFloat::mul32u(mult16_32_q15(q15::POINT_4, sff), mult16_32_q15(q15::POINT_4, dbf));
        let var2 = PFloat::mul32u(mult16_32_q15(q15::POINT_15, sff), mult16_32_q15(q15::POINT_15, dbf));
        self.dvar1 = VAR1_SMOOTH.mult(self.dvar1).add(var1);
        self.dvar2 = VAR2_SMOOTH.mult(self.dvar2).add(var2);
        diff
    }

    /// A statistically significant reduction of the residual echo, this frame or
    /// over either average ("not Gaussian, so careful about the longer tail").
    fn background_is_better(&self, diff: i32, sff: i32, dbf: i32) -> bool {
        signed_square(diff).gt(PFloat::mul32u(sff, dbf))
            || signed_square(self.davg1).gt(VAR1_UPDATE.mult(self.dvar1))
            || signed_square(self.davg2).gt(VAR2_UPDATE.mult(self.dvar2))
    }

    /// The background filter is significantly worse than the foreground.
    /// C writes `FLOAT_MUL32U(NEG32(d), ABS32(d))`; |−d| = |d| even for `i32::MIN`
    /// under wrapping, so that is the signed square of `−d`.
    fn background_is_worse(&self, diff: i32, sff: i32, dbf: i32) -> bool {
        signed_square(diff.wrapping_neg()).gt(VAR_BACKTRACK.mult(PFloat::mul32u(sff, dbf)))
            || signed_square(self.davg1.wrapping_neg()).gt(VAR_BACKTRACK.mult(self.dvar1))
            || signed_square(self.davg2.wrapping_neg()).gt(VAR_BACKTRACK.mult(self.dvar2))
    }

    /// Copies the background filter into the foreground, fading the output from
    /// the foreground's echo estimate to the background's over one frame "so as not
    /// to introduce blocking artifacts".
    fn adopt_background(&mut self) {
        let n = self.frame_size;
        self.clear_variances();
        for (f, &w) in self.foreground.iter_mut().zip(&self.weights) {
            *f = extract16(pshr32(w, 16));
        }
        for i in 0..n {
            let faded = mult16_16_q(self.window[i + n] as i32, self.e[i + n] as i32, 15)
                + mult16_16_q(self.window[i] as i32, self.y[i + n] as i32, 15);
            self.e[i + n] = faded as i16;
        }
    }

    /// Restores the background filter from the foreground, with its echo estimate
    /// and error, "so as to get correct adaptation".
    fn restore_background(&mut self) {
        let n = self.frame_size;
        for (w, &f) in self.weights.iter_mut().zip(&self.foreground) {
            *w = shl32(f as i32, 16);
        }
        for i in 0..n {
            self.y[i + n] = self.e[i + n];
            self.e[i] = sub16(self.input[i] as i32, self.y[i + n] as i32) as i16;
        }
        self.clear_variances();
    }

    fn clear_variances(&mut self) {
        self.davg1 = 0;
        self.davg2 = 0;
        self.dvar1 = PFloat::ZERO;
        self.dvar2 = PFloat::ZERO;
    }
}
