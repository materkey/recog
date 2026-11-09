//! Integer approximations from speexdsp's `math_approx.h` (fixed-point build).
//!
//! Only the functions the echo canceller reaches are here: logarithms used to
//! normalise pseudo-floats, the square root, the cosine of the analysis window,
//! the exponential of the initial adaptation profile, and the normalised cosine
//! from which the FFT twiddles are built. Each one is a polynomial in Q13–Q15 with
//! the same intermediate truncations as C, so results are bit-identical.

use crate::fixed::{add16, add32, mult16_16_p, mult16_16_q, sub16, sub32, to16, vshr32};

/// `spx_ilog2`: index of the highest set bit, 0 for 0 and 1.
///
/// C finds it with five compare-and-shift steps; the count of leading zeros gives
/// the same index in one instruction (`CLZ`).
#[inline(always)]
pub(crate) const fn ilog2(x: u32) -> i32 {
    if x == 0 { 0 } else { 31 - x.leading_zeros() as i32 }
}

/// `spx_ilog4`: half of [`ilog2`], rounded down (C's four steps give the same).
#[inline(always)]
pub(crate) const fn ilog4(x: u32) -> i32 {
    ilog2(x) / 2
}

/// C's `spx_ilog2` and `spx_ilog4` are the same ladder with different rungs: if
/// `x` reaches the threshold, shift it down and add to the result. Kept, as data,
/// to check [`ilog2`] and [`ilog4`] against.
#[cfg(test)]
const SPX_ILOG2: [(u32, u32, i32); 5] = [(65536, 16, 16), (256, 8, 8), (16, 4, 4), (4, 2, 2), (2, 0, 1)];
#[cfg(test)]
const SPX_ILOG4: [(u32, u32, i32); 4] = [(65536, 16, 8), (256, 8, 4), (16, 4, 2), (4, 0, 1)];

/// Climbs one of C's ladders (`(threshold, shift, add)` per rung).
#[cfg(test)]
fn c_ladder(mut x: u32, rungs: &[(u32, u32, i32)]) -> i32 {
    let mut r = 0;
    for &(threshold, shift, add) in rungs {
        if x >= threshold {
            x >>= shift;
            r += add;
        }
    }
    r
}

/// `spx_sqrt`: square root of a Q0 integer, as a 16-bit value
/// (`C0 + C1·x + C2·x² + C3·x³` after normalising `x` by powers of 4).
pub(crate) const fn sqrt(x: i32) -> i16 {
    const C0: i32 = 3634;
    const C1: i32 = 21173;
    const C2: i32 = -12627;
    const C3: i32 = 4204;
    // spx_ilog4 takes spx_uint32_t: a negative argument is a large unsigned value
    let k = ilog4(x as u32) - 6;
    let x = vshr32(x, k * 2);
    let inner = add16(C2, mult16_16_q(x, C3, 14));
    let middle = add16(C1, mult16_16_q(x, inner, 14));
    let rt = add16(C0, mult16_16_q(x, middle, 14));
    vshr32(rt, 7 - k) as i16
}

/// `spx_cos` (fixed point): cosine of `x` in Q13 radians, result in Q14.
pub(crate) const fn cos(x: i16) -> i16 {
    const K1: i32 = 8192;
    const K2: i32 = -4096;
    const K3: i32 = 340;
    const K4: i32 = -10;
    const fn poly(x2: i32) -> i32 {
        mult16_16_p(x2, add32(K2, mult16_16_p(x2, add32(K3, mult16_16_p(K4, x2, 13)), 13)), 13)
    }
    if (x as i32) < 12868 {
        let x2 = to16(mult16_16_p(x as i32, x as i32, 13));
        add32(K1, poly(x2)) as i16
    } else {
        let x = to16(sub16(25736, x as i32));
        let x2 = to16(mult16_16_p(x, x, 13));
        sub32(-K1, poly(x2)) as i16
    }
}

/// `spx_exp2`: 2^x for `x` in Q11, result in Q14.
const fn exp2(x: i16) -> i32 {
    const D0: i32 = 16384;
    const D1: i32 = 11356;
    const D2: i32 = 3726;
    const D3: i32 = 1301;
    let integer = (x as i32) >> 11;
    if integer > 14 {
        return 0x7fff_ffff;
    } else if integer < -15 {
        return 0;
    }
    let frac = to16(((x as i32) - (integer << 11)) << 3);
    let inner = add16(D2, mult16_16_q(D3, frac, 14));
    let middle = add16(D1, mult16_16_q(frac, inner, 14));
    let frac = add16(D0, mult16_16_q(frac, middle, 14));
    vshr32(frac, -integer - 2)
}

/// `spx_exp`: e^x for `x` in Q11, result in Q14.
pub(crate) const fn exp(x: i16) -> i32 {
    if x > 21290 {
        0x7fff_ffff
    } else if x < -21290 {
        0
    } else {
        exp2(mult16_16_p(23637, x as i32, 14) as i16)
    }
}

/// `_spx_cos_pi_2`: cos(π/2 · x) for `x` in Q15 on [0, 1), result in Q15.
const fn cos_pi_2(x: i16) -> i32 {
    const L1: i32 = 32767;
    const L2: i32 = -7651;
    const L3: i32 = 8277;
    const L4: i32 = -626;
    let x2 = to16(mult16_16_p(x as i32, x as i32, 15));
    let inner = add32(L3, mult16_16_p(L4, x2, 15));
    let middle = add32(L2, mult16_16_p(x2, inner, 15));
    let sum = add32(sub16(L1, x2), mult16_16_p(x2, middle, 15));
    add16(1, if sum < 32766 { sum } else { 32766 })
}

/// `spx_cos_norm`: cos(π/2 · x) for `x` in Q15 over any period, result in Q15.
///
/// The FFT twiddles of the fixed-point build come from here, not from libm.
pub(crate) const fn cos_norm(x: i32) -> i16 {
    let mut x = x & 0x0001_ffff;
    if x > 1 << 16 {
        x = (1 << 17) - x;
    }
    if x & 0x0000_7fff != 0 {
        if x < 1 << 15 { cos_pi_2(x as i16) as i16 } else { (-cos_pi_2((65536 - x) as i16)) as i16 }
    } else if x & 0x0000_ffff != 0 {
        0
    } else if x & 0x0001_ffff != 0 {
        -32767
    } else {
        32767
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_is_computed_up_to_21290_and_saturates_beyond() {
        // 21290 in Q11 is e^10.4: exp2(30715), whose integer part 14 still shifts
        assert_eq!(exp(21290), 2_143_682_560);
        assert_eq!(exp(21291), 0x7fff_ffff);
        assert_eq!(exp(-21290), 2);
        assert_eq!(exp(-21291), 0);
    }

    #[test]
    fn logarithms() {
        assert_eq!(ilog2(0), 0);
        assert_eq!(ilog2(1), 0);
        assert_eq!(ilog2(2), 1);
        assert_eq!(ilog2(0x8000_0000), 31);
        assert_eq!(ilog2(65535), 15);
        for k in 0..32 {
            assert_eq!(ilog2(1 << k), k);
            assert_eq!(ilog4(1 << k), k / 2);
        }
    }

    #[test]
    fn clz_logarithms_equal_c_for_every_bit_pattern_class() {
        // every power of two, its neighbours, and a stride over the whole range
        let mut values: Vec<u32> =
            (0..32).flat_map(|k| [(1u32 << k).wrapping_sub(1), 1 << k, (1u32 << k) + 1]).collect();
        values.extend((0..=u32::MAX).step_by(65_521));
        values.push(u32::MAX);
        for x in values {
            assert_eq!(ilog2(x), c_ladder(x, &SPX_ILOG2), "ilog2({x})");
            assert_eq!(ilog4(x), c_ladder(x, &SPX_ILOG4), "ilog4({x})");
        }
    }

    #[test]
    fn sqrt_tracks_the_real_square_root() {
        for &x in &[1i32, 2, 100, 4096, 65536, 1 << 20, 536_870_912] {
            let real = f64::from(x).sqrt();
            let got = f64::from(sqrt(x));
            assert!((got - real).abs() <= real * 0.002 + 1.0, "sqrt({x}) = {got}, real {real}");
        }
    }

    #[test]
    fn cos_tracks_the_real_cosine() {
        // Q13 radians in, Q13 out: spx_cos(0) is 8192
        for i in 0..=25736 / 64 {
            let x = (i * 64) as i16;
            let real = (f64::from(x) / 8192.0).cos() * 8192.0;
            let got = f64::from(cos(x));
            assert!((got - real).abs() < 8.0, "cos({x}) = {got}, real {real}");
        }
    }

    #[test]
    fn exp_tracks_the_real_exponential() {
        assert_eq!(exp(21291), 0x7fff_ffff);
        assert_eq!(exp(-21291), 0);
        // Q11 in, Q16 out: spx_exp(0) is 65536 (the Q14 polynomial shifted up by 2)
        assert_eq!(exp(0), 65536);
        for &x in &[-20000i16, -4915, -189, 2048, 8000] {
            let real = (f64::from(x) / 2048.0).exp() * 65536.0;
            let got = f64::from(exp(x));
            assert!((got - real).abs() <= real * 0.003 + 2.0, "exp({x}) = {got}, real {real}");
        }
        assert_eq!(exp2(i16::MAX), 0x7fff_ffff);
        assert_eq!(exp2(i16::MIN), 0);
    }

    #[test]
    fn cos_norm_covers_every_quadrant() {
        assert_eq!(cos_norm(0), 32767);
        assert_eq!(cos_norm(1 << 15), 0);
        assert_eq!(cos_norm(1 << 16), -32767);
        assert_eq!(cos_norm(3 << 15), 0);
        assert_eq!(cos_norm(1 << 17), 32767);
        for i in (-(1 << 17)..(1 << 17)).step_by(977) {
            let real = (std::f64::consts::FRAC_PI_2 * f64::from(i) / 32768.0).cos() * 32768.0;
            let got = f64::from(cos_norm(i));
            assert!((got - real).abs() < 3.0, "cos_norm({i}) = {got}, real {real}");
        }
    }
}
