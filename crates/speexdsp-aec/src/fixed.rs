//! Fixed-point primitives of speexdsp's `fixed_generic.h` and `arch.h`.
//!
//! The C macros operate on `int` expressions: every 16-bit operand is promoted to
//! `int` before arithmetic, and a value is cut to 16 bits only where a macro casts
//! (`MULT16_16` casts both operands to `spx_word16_t`) or where the caller stores the
//! result into a 16-bit variable. Every function here takes and returns `i32` and
//! performs exactly the casts of its macro, so the ported code can be read side by
//! side with `mdf.c`.
//!
//! Overflow wraps. Signed overflow is undefined in C, but the arithmetic in
//! speexdsp relies on two's complement wrap-around (its divergence check even reads
//! an overflowed energy as negative), and clang's AArch64 code wraps. The same holds
//! for variable shifts, which use the shift amount modulo 32 like `ASRV`/`LSLV`, and
//! for division, where `SDIV` gives 0 for a zero divisor instead of trapping.

/// `EXTRACT16`: keep the low 16 bits.
#[inline(always)]
pub(crate) const fn extract16(x: i32) -> i16 {
    x as i16
}

/// The value of a 16-bit cast, as C would promote it again: `(spx_word16_t)(x)`.
#[inline(always)]
pub(crate) const fn to16(x: i32) -> i32 {
    x as i16 as i32
}

/// `SHL32` / `SHL16` with a constant shift: wraps out the high bits.
#[inline(always)]
pub(crate) const fn shl32(a: i32, shift: u32) -> i32 {
    a.wrapping_shl(shift)
}

/// `PSHR32` / `PSHR`: arithmetic right shift rounding to nearest.
#[inline(always)]
pub(crate) const fn pshr32(a: i32, shift: u32) -> i32 {
    a.wrapping_add((1i32 << shift) >> 1) >> shift
}

/// `VSHR32`: right shift for a positive amount, left shift for a negative one.
///
/// Both shifts are computed and one is selected, which compiles to a conditional
/// select instead of a branch and lets loops over it vectorise; the amounts are
/// taken modulo 32 either way, as `ASRV`/`LSLV` do.
#[inline(always)]
pub(crate) const fn vshr32(a: i32, shift: i32) -> i32 {
    let right = a.wrapping_shr(shift as u32);
    let left = a.wrapping_shl(shift.wrapping_neg() as u32);
    if shift > 0 { right } else { left }
}

/// `SATURATE32(x, a)` / `SATURATE16(x, a)`: clamp to `[-a, a]`.
#[inline(always)]
pub(crate) const fn saturate(x: i32, a: i32) -> i32 {
    if x > a {
        a
    } else if x < -a {
        -a
    } else {
        x
    }
}

/// `ADD16`: both operands and the sum are cut to 16 bits.
#[inline(always)]
pub(crate) const fn add16(a: i32, b: i32) -> i32 {
    (to16(a) + to16(b)) as i16 as i32
}

/// `SUB16`: the operands are cut to 16 bits, the difference is **not**.
#[inline(always)]
pub(crate) const fn sub16(a: i32, b: i32) -> i32 {
    to16(a) - to16(b)
}

/// `ADD32`.
#[inline(always)]
pub(crate) const fn add32(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

/// `SUB32`.
#[inline(always)]
pub(crate) const fn sub32(a: i32, b: i32) -> i32 {
    a.wrapping_sub(b)
}

/// `MULT16_16`: 16 × 16 → 32 bits. Cannot overflow: |product| ≤ 2³⁰.
#[inline(always)]
pub(crate) const fn mult16_16(a: i32, b: i32) -> i32 {
    to16(a) * to16(b)
}

/// `MULT16_32_32`: the first operand cut to 16 bits, the product wraps.
#[inline(always)]
pub(crate) const fn mult16_32_32(a: i32, b: i32) -> i32 {
    to16(a).wrapping_mul(b)
}

/// `MAC16_16(c, a, b)`: `c + a·b`.
#[inline(always)]
pub(crate) const fn mac16_16(c: i32, a: i32, b: i32) -> i32 {
    c.wrapping_add(mult16_16(a, b))
}

/// `MULT16_32_Q15`: a 16-bit by 32-bit product, Q15, split into the high and the
/// low 15 bits of `b` so that it never needs 48 bits.
#[inline(always)]
pub(crate) const fn mult16_32_q15(a: i32, b: i32) -> i32 {
    mult16_32_32(a, b >> 15).wrapping_add(mult16_16(a, b & 0x7fff) >> 15)
}

/// `MAC16_32_Q15(c, a, b)`: `c + MULT16_32_Q15(a, b)`.
#[inline(always)]
pub(crate) const fn mac16_32_q15(c: i32, a: i32, b: i32) -> i32 {
    c.wrapping_add(mult16_32_q15(a, b))
}

/// `MULT16_16_Q13`/`_Q14`/`_Q15`: product shifted right, truncating.
#[inline(always)]
pub(crate) const fn mult16_16_q(a: i32, b: i32, q: u32) -> i32 {
    mult16_16(a, b) >> q
}

/// `MULT16_16_P13`/`_P14`/`_P15`: product shifted right, rounding.
#[inline(always)]
pub(crate) const fn mult16_16_p(a: i32, b: i32, q: u32) -> i32 {
    ((1i32 << (q - 1)) + mult16_16(a, b)) >> q
}

/// Integer division as AArch64 `SDIV` computes it: truncating, 0 for a zero
/// divisor, and `i32::MIN / -1 = i32::MIN`.
#[inline(always)]
pub(crate) const fn sdiv(a: i32, b: i32) -> i32 {
    if b == 0 { 0 } else { a.wrapping_div(b) }
}

/// `DIV32_16`: the divisor cut to 16 bits, the quotient too.
#[inline(always)]
pub(crate) const fn div32_16(a: i32, b: i32) -> i32 {
    sdiv(a, to16(b)) as i16 as i32
}

/// `DIV32`.
#[inline(always)]
pub(crate) const fn div32(a: i32, b: i32) -> i32 {
    sdiv(a, b)
}

/// `ABS32`, wrapping for `i32::MIN` like `-x` does in practice.
#[inline(always)]
pub(crate) const fn abs32(x: i32) -> i32 {
    x.wrapping_abs()
}

/// `WORD2INT` (fixed point): clamp to the 16-bit range.
#[inline(always)]
pub(crate) const fn word2int(x: i32) -> i16 {
    if x < -32767 {
        -32768
    } else if x > 32766 {
        32767
    } else {
        x as i16
    }
}

/// `QCONST16(x, 15)` for the constants `mdf.c` uses, evaluated the way C does:
/// `(spx_word16_t)(.5 + x * 32768)`. The test module recomputes each one.
pub(crate) mod q15 {
    /// `QCONST16(.9, 15)`: pre-emphasis, and the notch radius below 12 kHz.
    pub const POINT_9: i32 = 29491;
    /// `QCONST16(.982, 15)`: notch radius from 12 to 24 kHz.
    pub const POINT_982: i32 = 32178;
    /// `QCONST16(.992, 15)`: notch radius from 24 kHz.
    pub const POINT_992: i32 = 32506;
    /// `QCONST16(.7, 15)`.
    pub const POINT_7: i32 = 22938;
    /// `QCONST16(.3, 15)`.
    pub const POINT_3: i32 = 9830;
    /// `QCONST16(.8f, 15)`.
    pub const POINT_8: i32 = 26214;
    /// `QCONST16(.1f, 15)`.
    pub const POINT_1: i32 = 3277;
    /// `QCONST16(.99f, 15)`.
    pub const POINT_99: i32 = 32440;
    /// `QCONST16(.6f, 15)`.
    pub const POINT_6: i32 = 19661;
    /// `QCONST16(.4f, 15)`.
    pub const POINT_4: i32 = 13107;
    /// `QCONST16(.85f, 15)`.
    pub const POINT_85: i32 = 27853;
    /// `QCONST16(.15f, 15)`.
    pub const POINT_15: i32 = 4915;
    /// `QCONST16(.03f, 15)`.
    pub const POINT_03: i32 = 983;
    /// `QCONST16(.25f, 15)`.
    pub const POINT_25: i32 = 8192;
    /// `QCONST16(2.4, 11)`.
    pub const TWO_POINT_4_Q11: i32 = 4915;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qconst16_f64(x: f64, bits: u32) -> i32 {
        (0.5 + x * f64::from(1u32 << bits)) as i16 as i32
    }

    fn qconst16_f32(x: f32, bits: u32) -> i32 {
        // `x` is a float literal: the product is a float, then promoted for `.5 +`.
        (0.5 + f64::from(x * (1u32 << bits) as f32)) as i16 as i32
    }

    #[test]
    fn constants_equal_their_c_expressions() {
        assert_eq!(q15::POINT_9, qconst16_f64(0.9, 15));
        assert_eq!(q15::POINT_982, qconst16_f64(0.982, 15));
        assert_eq!(q15::POINT_992, qconst16_f64(0.992, 15));
        assert_eq!(q15::POINT_7, qconst16_f64(0.7, 15));
        assert_eq!(q15::POINT_3, qconst16_f64(0.3, 15));
        assert_eq!(q15::POINT_8, qconst16_f32(0.8, 15));
        assert_eq!(q15::POINT_1, qconst16_f32(0.1, 15));
        assert_eq!(q15::POINT_99, qconst16_f32(0.99, 15));
        assert_eq!(q15::POINT_6, qconst16_f32(0.6, 15));
        assert_eq!(q15::POINT_4, qconst16_f32(0.4, 15));
        assert_eq!(q15::POINT_85, qconst16_f32(0.85, 15));
        assert_eq!(q15::POINT_15, qconst16_f32(0.15, 15));
        assert_eq!(q15::POINT_03, qconst16_f32(0.03, 15));
        assert_eq!(q15::POINT_25, qconst16_f32(0.25, 15));
        assert_eq!(q15::TWO_POINT_4_Q11, qconst16_f64(2.4, 11));
    }

    #[test]
    fn sub16_keeps_the_carry_and_add16_drops_it() {
        assert_eq!(sub16(30000, -10000), 40000);
        assert_eq!(add16(30000, 10000), 40000 - 65536);
        // the operands are cut before the operation
        assert_eq!(sub16(65536 + 5, 3), 2);
    }

    #[test]
    fn mult16_32_q15_matches_the_exact_product_within_one() {
        for &(a, b) in &[(32767, 1 << 30), (-32768, -(1 << 30)), (12345, -987_654_321), (-3, 7)] {
            let exact = (i64::from(a) * i64::from(b)) >> 15;
            let got = i64::from(mult16_32_q15(a, b));
            assert!((got - exact).abs() <= 1, "{a} {b}: {got} vs {exact}");
        }
    }

    #[test]
    fn rounding_and_variable_shifts() {
        assert_eq!(pshr32(5, 1), 3);
        assert_eq!(pshr32(-5, 1), -2);
        assert_eq!(pshr32(7, 0), 7);
        assert_eq!(vshr32(-256, 4), -16);
        assert_eq!(vshr32(3, -4), 48);
        assert_eq!(vshr32(1, 0), 1);
        // the hardware takes the amount modulo 32
        assert_eq!(vshr32(1, -33), 2);
        assert_eq!(mult16_16_p(3, 16384, 15), 2);
        assert_eq!(mult16_16_q(3, 16384, 15), 1);
    }

    #[test]
    fn division_behaves_like_sdiv() {
        assert_eq!(div32(7, -2), -3);
        assert_eq!(div32(5, 0), 0);
        assert_eq!(div32(i32::MIN, -1), i32::MIN);
        // DIV32_16 cuts the divisor: 48000 is -17536 in 16 bits
        assert_eq!(div32_16(160 << 15, 48000), (5_242_880 / -17_536) as i16 as i32);
    }

    #[test]
    fn saturation_and_clamps() {
        assert_eq!(saturate(40000, 32767), 32767);
        assert_eq!(saturate(-40000, 32767), -32767);
        assert_eq!(saturate(12, 32767), 12);
        assert_eq!(word2int(-40000), -32768);
        assert_eq!(word2int(-32768), -32768);
        assert_eq!(word2int(32767), 32767);
        assert_eq!(word2int(100), 100);
        assert_eq!(abs32(i32::MIN), i32::MIN);
        assert_eq!(extract16(65537), 1);
        assert_eq!(shl32(1 << 30, 2), 0);
        assert_eq!(mac16_32_q15(1, 16384, 2), 2);
        assert_eq!(sub32(i32::MIN, 1), i32::MAX);
        assert_eq!(add32(i32::MAX, 1), i32::MIN);
    }
}
