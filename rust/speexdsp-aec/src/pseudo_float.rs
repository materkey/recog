//! speexdsp's pseudo-float (`spx_float_t`, `pseudofloat.h`, fixed-point build).
//!
//! A value is `m · 2^e` with a 16-bit signed mantissa and a 16-bit exponent. The
//! canceller keeps its correlation statistics in this format. Normalisation is a
//! single step after each operation (not a loop), and zero is not canonical, so the
//! exact bit patterns matter for later comparisons: every operation reproduces C's
//! 16-bit truncations, including the ones that look like accidents.

use crate::fixed::{abs32, div32_16, extract16, mult16_16_q, mult16_32_q15, shl32, vshr32};
use crate::math::{ilog2, sqrt};

/// `spx_float_t`: `m · 2^e`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PFloat {
    pub(crate) m: i16,
    pub(crate) e: i16,
}

impl PFloat {
    /// `FLOAT_ZERO`.
    pub(crate) const ZERO: Self = Self { m: 0, e: 0 };
    /// `FLOAT_ONE`.
    pub(crate) const ONE: Self = Self { m: 16384, e: -14 };

    pub(crate) const fn new(m: i16, e: i16) -> Self {
        Self { m, e }
    }

    /// One normalisation step, shared by `FLOAT_ADD`, `FLOAT_SUB` and `FLOAT_MULT`.
    const fn normalized(m: i32, e: i32) -> Self {
        let (m, e) = (m as i16, e as i16);
        // C tests each sign on its own; together that is |m| < 16384, zero included
        let shift = (m as i32).abs() < 16384;
        if shift {
            Self { m: ((m as i32) << 1) as i16, e: e.wrapping_sub(1) }
        } else {
            Self { m, e }
        }
    }

    /// `PSEUDOFLOAT(x)`.
    pub(crate) const fn from_i32(x: i32) -> Self {
        let negative = x < 0;
        let x = if negative { x.wrapping_neg() } else { x };
        if x == 0 {
            return Self::ZERO;
        }
        let e = ilog2(abs32(x) as u32) - 14;
        let x = vshr32(x, e);
        let m = if negative { x.wrapping_neg() } else { x };
        Self { m: m as i16, e: e as i16 }
    }

    /// `FLOAT_ADD(self, b)`.
    pub(crate) const fn add(self, b: Self) -> Self {
        if self.m == 0 {
            return b;
        } else if b.m == 0 {
            return self;
        }
        let (a, b) = (self, b);
        let (am, ae, bm, be) = (a.m as i32, a.e as i32, b.m as i32, b.e as i32);
        if ae > be {
            Self::normalized((am >> 1) + (bm >> min15(ae - be + 1)), ae + 1)
        } else {
            Self::normalized((bm >> 1) + (am >> min15(be - ae + 1)), be + 1)
        }
    }

    /// `FLOAT_SUB(FLOAT_ONE, b)`, the only subtraction the canceller performs.
    ///
    /// The general `FLOAT_SUB(a, b)` returns `b` when `a` is zero (it should be
    /// `-b`); with `a = FLOAT_ONE` that branch cannot be taken.
    pub(crate) const fn one_minus(b: Self) -> Self {
        let a = Self::ONE;
        if b.m == 0 {
            return a;
        }
        let (am, ae, bm, be) = (a.m as i32, a.e as i32, b.m as i32, b.e as i32);
        if ae > be {
            Self::normalized((am >> 1) - (bm >> min15(ae - be + 1)), ae + 1)
        } else {
            Self::normalized((am >> min15(be - ae + 1)) - (bm >> 1), be + 1)
        }
    }

    /// `FLOAT_LT(self, b)`.
    pub(crate) const fn lt(self, b: Self) -> bool {
        if self.m == 0 {
            return b.m > 0;
        }
        // C also answers `self.m < 0` when b.m == 0; the comparison below gives that
        let (am, ae, bm, be) = (self.m as i32, self.e as i32, b.m as i32, b.e as i32);
        if ae > be {
            (am >> 1) < (bm >> min15(ae - be + 1))
        } else {
            (bm >> 1) > (am >> min15(be - ae + 1))
        }
    }

    /// `FLOAT_GT(self, b)`.
    pub(crate) const fn gt(self, b: Self) -> bool {
        b.lt(self)
    }

    /// `FLOAT_MULT(self, b)`.
    pub(crate) const fn mult(self, b: Self) -> Self {
        let m = ((self.m as i32) * (b.m as i32)) >> 15;
        Self::normalized(m as i16 as i32, self.e as i32 + b.e as i32 + 15)
    }

    /// `FLOAT_AMULT(self, b)`: like [`mult`](Self::mult) without normalising.
    pub(crate) const fn amult(self, b: Self) -> Self {
        let m = ((self.m as i32) * (b.m as i32)) >> 15;
        Self { m: m as i16, e: (self.e as i32 + b.e as i32 + 15) as i16 }
    }

    /// `FLOAT_SHL(self, b)`.
    pub(crate) const fn shl(self, b: i32) -> Self {
        Self { m: self.m, e: (self.e as i32 + b) as i16 }
    }

    /// `FLOAT_EXTRACT16`.
    pub(crate) const fn extract16(self) -> i16 {
        let (m, e) = (self.m as i32, self.e as i32);
        if e < 0 {
            extract16(m.wrapping_add(1i32.wrapping_shl((-e - 1) as u32)).wrapping_shr((-e) as u32))
        } else {
            m.wrapping_shl(e as u32) as i16
        }
    }

    /// `FLOAT_EXTRACT32`.
    pub(crate) const fn extract32(self) -> i32 {
        let (m, e) = (self.m as i32, self.e as i32);
        if e < 0 {
            m.wrapping_add(1i32.wrapping_shl((-e - 1) as u32)).wrapping_shr((-e) as u32)
        } else {
            m.wrapping_shl(e as u32)
        }
    }

    /// `FLOAT_MUL32(self, b)`: a 32-bit integer scaled by this value.
    #[inline(always)]
    pub(crate) const fn mul32(self, b: i32) -> i32 {
        vshr32(mult16_32_q15(self.m as i32, b), -(self.e as i32) - 15)
    }

    /// `FLOAT_MUL32U(a, b)`: the product of two 32-bit integers as a pseudo-float.
    pub(crate) const fn mul32u(a: i32, b: i32) -> Self {
        if a == 0 || b == 0 {
            return Self::ZERO;
        }
        let e1 = ilog2(abs32(a) as u32);
        let a = vshr32(a, e1 - 14);
        let e2 = ilog2(abs32(b) as u32);
        let b = vshr32(b, e2 - 14);
        Self { m: mult16_16_q(a, b, 15) as i16, e: (e1 + e2 - 13) as i16 }
    }

    /// `FLOAT_DIV32_FLOAT(a, b)`: `a / b` for a positive `b`.
    pub(crate) const fn div32_float(a: i32, b: Self) -> Self {
        if a == 0 {
            return Self::ZERO;
        }
        let bm = b.m as i32;
        let mut e = ilog2(abs32(a) as u32) - ilog2((bm - 1) as u32) - 15;
        let mut a = vshr32(a, e);
        if abs32(a) >= shl32(bm - 1, 15) {
            a >>= 1;
            e += 1;
        }
        Self { m: div32_16(a, bm) as i16, e: (e - b.e as i32) as i16 }
    }

    /// `FLOAT_DIV32(a, b)`: `a / b` for a positive `b`.
    pub(crate) const fn div32(a: i32, b: i32) -> Self {
        if a == 0 {
            return Self::ZERO;
        }
        let mut b = b;
        let mut e0 = 0;
        if b > 32767 {
            e0 = ilog2(b as u32) - 14;
            b = vshr32(b, e0);
            e0 = -e0;
        }
        let mut e = ilog2(abs32(a) as u32) - ilog2((b - 1) as u32) - 15;
        let mut a = vshr32(a, e);
        if abs32(a) >= shl32(b - 1, 15) {
            a >>= 1;
            e += 1;
        }
        e += e0;
        Self { m: div32_16(a, b) as i16, e: e as i16 }
    }

    /// `FLOAT_DIVU(self, b)`: `self / b`; a non-positive `b` gives `FLOAT_ONE`, as C
    /// does after printing a warning.
    pub(crate) const fn divu(self, b: Self) -> Self {
        if b.m <= 0 {
            return Self::ONE;
        }
        let num = self.m as i32;
        // ABS16 into a 16-bit field: -32768 stays -32768
        let mut am = (self.m as i32).wrapping_abs() as i16;
        let mut e = 0;
        while am >= b.m {
            e += 1;
            am >>= 1;
        }
        let num = num.wrapping_shl((15 - e) as u32);
        Self { m: div32_16(num, b.m as i32) as i16, e: (self.e as i32 - b.e as i32 - 15 + e) as i16 }
    }

    /// `FLOAT_SQRT`.
    pub(crate) const fn sqrt(self) -> Self {
        let mut m = shl32(self.m as i32, 14);
        let e = (self.e as i32 - 14) as i16;
        // an odd exponent moves a bit into the mantissa; C also decrements it before
        // halving, which `>>` (rounding down) makes unnecessary
        if e & 1 != 0 {
            m = m.wrapping_shl(1);
        }
        Self { m: sqrt(m), e: e >> 1 }
    }
}

/// `MIN(15, x)` of the pseudo-float alignment shifts.
#[inline(always)]
const fn min15(x: i32) -> u32 {
    (if x < 15 { x } else { 15 }) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(x: PFloat) -> f64 {
        f64::from(x.m) * 2f64.powi(i32::from(x.e))
    }

    #[test]
    fn conversion_and_extraction_round_trip() {
        for &x in &[1i32, -1, 3, 16384, -16385, 1 << 20, -(1 << 29), 123_456_789] {
            let p = PFloat::from_i32(x);
            let rel = (value(p) - f64::from(x)).abs() / f64::from(x).abs();
            assert!(rel < 1e-4, "{x} -> {p:?}");
            assert!((i64::from(p.extract32()) - i64::from(x)).abs() <= i64::from(x).abs() / 8192 + 1);
        }
        assert_eq!(PFloat::from_i32(0), PFloat::ZERO);
        assert_eq!(PFloat::ONE.extract16(), 1);
        assert_eq!(PFloat::new(3, 2).extract16(), 12);
        assert_eq!(PFloat::new(3, 2).extract32(), 12);
    }

    #[test]
    fn arithmetic_tracks_real_values() {
        let a = PFloat::from_i32(30_000);
        let b = PFloat::from_i32(-1_000);
        assert!((value(a.add(b)) - 29_000.0).abs() < 4.0);
        assert!((value(b.add(a)) - 29_000.0).abs() < 4.0);
        assert!((value(a.mult(b)) + 30_000_000.0).abs() < 3_000.0);
        assert!((value(a.amult(b)) + 30_000_000.0).abs() < 3_000.0);
        assert!((value(PFloat::mul32u(30_000, -1_000)) + 30_000_000.0).abs() < 3_000.0);
        assert!((value(PFloat::div32(30_000, 1_000)) - 30.0).abs() < 0.01);
        assert!((value(PFloat::div32(30_000, 1_000_000)) - 0.03).abs() < 1e-5);
        assert!((value(PFloat::div32_float(30_000, PFloat::from_i32(1_000))) - 30.0).abs() < 0.01);
        assert!((value(a.divu(PFloat::from_i32(1_000))) - 30.0).abs() < 0.01);
        assert!((value(PFloat::from_i32(1 << 20).sqrt()) - 1024.0).abs() < 2.0);
        assert!((value(PFloat::from_i32(1 << 21).sqrt()) - 1448.15).abs() < 3.0);
        assert!((value(PFloat::one_minus(PFloat::from_i32(1).shl(-2))) - 0.75).abs() < 1e-3);
        // a subtrahend at or above one aligns to its own exponent instead
        assert!((value(PFloat::one_minus(PFloat::from_i32(3))) + 2.0).abs() < 1e-3);
        assert!((value(a.shl(-10)) - 30_000.0 / 1024.0).abs() < 0.01);
        assert_eq!(PFloat::ONE.mul32(120_000), 120_000);
    }

    #[test]
    fn zero_and_degenerate_operands() {
        let a = PFloat::from_i32(7);
        assert_eq!(PFloat::ZERO.add(a), a);
        assert_eq!(a.add(PFloat::ZERO), a);
        assert_eq!(PFloat::one_minus(PFloat::ZERO), PFloat::ONE);
        assert_eq!(PFloat::mul32u(0, 5), PFloat::ZERO);
        assert_eq!(PFloat::mul32u(5, 0), PFloat::ZERO);
        assert_eq!(PFloat::div32(0, 5), PFloat::ZERO);
        assert_eq!(PFloat::div32_float(0, a), PFloat::ZERO);
        assert_eq!(a.divu(PFloat::ZERO), PFloat::ONE);
        assert_eq!(a.divu(PFloat::from_i32(-3)), PFloat::ONE);
        // ABS16(-32768) is -32768 again, so the alignment loop never runs and the
        // quotient -2^30 / 16384 = -65536 truncates to a zero mantissa
        let min = PFloat::new(-32768, 0);
        assert_eq!(min.divu(PFloat::new(16384, 0)), PFloat::new(0, -15));
    }

    #[test]
    fn ordering_agrees_with_real_values() {
        let values = [-50_000i32, -3, -1, 0, 1, 2, 3, 1000, 70_000, 1 << 28];
        for &x in &values {
            for &y in &values {
                let (px, py) = (PFloat::from_i32(x), PFloat::from_i32(y));
                if x < y {
                    assert!(px.lt(py), "{x} < {y}");
                    assert!(py.gt(px));
                } else {
                    assert!(!px.lt(py), "!({x} < {y})");
                }
            }
        }
    }

    #[test]
    fn a_mantissa_of_one_half_is_normalised_and_a_smaller_one_doubles() {
        assert_eq!(PFloat::normalized(16384, 0), PFloat::new(16384, 0));
        assert_eq!(PFloat::normalized(-16384, 0), PFloat::new(-16384, 0));
        assert_eq!(PFloat::normalized(16383, 0), PFloat::new(32766, -1));
        assert_eq!(PFloat::normalized(-16383, 0), PFloat::new(-32766, -1));
        // a product of exactly -1/2 stays as it is
        assert_eq!(PFloat::new(-16384, 0).mult(PFloat::new(32767, 0)), PFloat::new(-16384, 15));
    }

    #[test]
    fn zero_compares_by_sign_against_any_mantissa_and_exponent() {
        assert!(PFloat::ZERO.lt(PFloat::new(1, 0)));
        assert!(!PFloat::ZERO.lt(PFloat::new(-1, 0)));
        assert!(PFloat::new(-1, -20).lt(PFloat::new(0, 20)));
        assert!(PFloat::new(-1, 20).lt(PFloat::new(0, -20)));
        assert!(!PFloat::new(1, 20).lt(PFloat::new(0, -20)));
        assert!(!PFloat::new(1, -20).lt(PFloat::new(0, 20)));
    }

    #[test]
    fn division_realigns_a_numerator_that_would_overflow_the_quotient() {
        // the divisor's mantissa is 1/2 or the largest: ilog2(m - 1) sets the shift
        assert_eq!(PFloat::div32_float(536_870_911, PFloat::new(16384, 0)), PFloat::new(16383, 1));
        assert_eq!(PFloat::div32_float(1_073_741_823, PFloat::new(32767, 0)), PFloat::new(16384, 1));
        // only a mantissa at or below zero counts as division by zero
        assert_eq!(PFloat::new(16384, 0).divu(PFloat::new(1, 0)), PFloat::new(16384, 0));
    }
}

