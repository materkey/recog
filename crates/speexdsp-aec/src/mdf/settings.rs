//! What the canceller is built for and the constants derived from it:
//! `speex_echo_state_init`'s dimensions, the rate-dependent constants of
//! `SPEEX_ECHO_SET_SAMPLING_RATE`, and the initial per-partition rate shares. The
//! checks here are the port's own: speexdsp accepts every configuration and then
//! divides by zero, aborts, or overflows its 16-bit constants.

use crate::error::Error;
use crate::fft::{MAX_RADIX, factor};
use crate::fixed::{div32, div32_16, mult16_16, mult16_16_q, q15, shl32};
use crate::math::exp;

/// speexdsp's default rate, and the one pyaec leaves in place.
pub const DEFAULT_SAMPLING_RATE: u32 = 8000;
/// `M` is cast to 16 bits by `DIV32_16(…, M)`.
pub(super) const MAX_PARTITIONS: usize = 32767;

/// What an [`EchoCanceller`](crate::EchoCanceller) is built for: `speex_echo_state_init(frame_size,
/// filter_length)` followed by `SPEEX_ECHO_SET_SAMPLING_RATE`.
///
/// ```
/// use speexdsp_aec::Config;
///
/// let recog = Config::new(160, 4096); // recog/echo_cancel.py's FRAME and TAIL
/// assert_eq!(recog.sampling_rate, 8000); // what pyaec runs with, whatever SR it is given
/// let fixed = Config { sampling_rate: 16_000, ..recog };
/// assert_eq!(fixed.frame_size, 160);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Samples per [`EchoCanceller::cancel`](crate::EchoCanceller::cancel) call: 1 to 16383, made of prime
    /// factors up to 17, and at most half a second at `sampling_rate`.
    pub frame_size: usize,
    /// How long an echo the filter models, in samples; rounded up to whole frames.
    pub filter_length: usize,
    /// The rate the constants are tuned for, 1 to 32767 Hz. speexdsp's default is
    /// 8000; nothing else in the state depends on it.
    pub sampling_rate: u32,
}

impl Config {
    /// A configuration at speexdsp's default 8 kHz settings.
    pub const fn new(frame_size: usize, filter_length: usize) -> Self {
        Self { frame_size, filter_length, sampling_rate: DEFAULT_SAMPLING_RATE }
    }
}

/// Checks a complex FFT length the fixed-point KISS FFT can do: 1 to 16383 points,
/// radices up to 17 (speexdsp aborts the process on a larger one).
pub(crate) fn validate_half_size(n: usize) -> Result<(), &'static str> {
    if n == 0 {
        return Err("must be at least 1");
    }
    // the window of 2n samples goes through 16-bit casts (MULT16_16(N, ...))
    if i16::try_from(2 * n).is_err() {
        return Err("must be at most 16383: the window is cast to 16 bits");
    }
    if factor(n).iter().any(|&(p, _)| p > MAX_RADIX) {
        return Err("has a prime factor above 17, which KISS FFT cannot do");
    }
    Ok(())
}

/// `spec_average`, `beta0`, `beta_max` and the notch radius for a rate.
pub(super) fn rate_settings(frame_size: usize, rate: u32) -> Result<[i16; 4], Error> {
    let unsupported = |reason| Error::UnsupportedSamplingRate { sampling_rate: rate, reason };
    // DIV32_16 cuts the divisor to 16 bits: 48000 would become -17536
    if rate == 0 || i16::try_from(rate).is_err() {
        return Err(unsupported("must be 1 to 32767 Hz: the fixed-point constants divide by it in 16 bits"));
    }
    let (n, rate_i) = (frame_size as i32, rate as i32);
    // beta0 = (n << 16) / rate, the largest of the three, is stored in 16 bits
    if i16::try_from((i64::from(n) << 16) / i64::from(rate_i)).is_err() {
        return Err(unsupported("leaves frames of half a second or more, whose step size overflows 16 bits"));
    }
    let notch = if rate_i < 12000 {
        q15::POINT_9
    } else if rate_i < 24000 {
        q15::POINT_982
    } else {
        q15::POINT_992
    };
    Ok([
        div32_16(shl32(n, 15), rate_i) as i16,
        div32_16(shl32(n, 16), rate_i) as i16,
        div32_16(shl32(n, 14), rate_i) as i16,
        notch as i16,
    ])
}

/// The initial per-partition rate shares: a decay of about 10 between the first
/// and the last partition, scaled to sum to 0.8.
pub(super) fn initial_prop(m: usize) -> Vec<i16> {
    let decay = (exp((-div32_16(q15::TWO_POINT_4_Q11, m as i32)) as i16) >> 1) as i16;
    let mut prop = vec![0i16; m];
    prop[0] = q15::POINT_7 as i16;
    let mut sum = prop[0] as i32;
    for i in 1..m {
        prop[i] = mult16_16_q(prop[i - 1] as i32, decay as i32, 15) as i16;
        sum = sum.wrapping_add(prop[i] as i32);
    }
    for p in prop.iter_mut().rev() {
        *p = div32(mult16_16(q15::POINT_8, *p as i32), sum) as i16;
    }
    prop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_prop_decays_and_sums_to_point_8() {
        let prop = initial_prop(26);
        assert!(prop.windows(2).all(|w| w[0] >= w[1]), "{prop:?}");
        let sum: i32 = prop.iter().map(|&p| i32::from(p)).sum();
        assert!((sum - q15::POINT_8).abs() < 26, "{sum}");
        assert_eq!(initial_prop(1), vec![div32(mult16_16(q15::POINT_8, q15::POINT_7), q15::POINT_7) as i16]);
    }

    #[test]
    fn rate_settings_follow_the_c_formulas() {
        assert_eq!(rate_settings(160, 8000).unwrap(), [655, 1310, 327, 29491]);
        assert_eq!(rate_settings(160, 16000).unwrap(), [327, 655, 163, 32178]);
    }

    #[test]
    fn frame_sizes_stop_where_the_window_leaves_16_bits() {
        assert!(validate_half_size(1).is_ok());
        // 16380 = 2²·3²·5·7·13 is the largest frame KISS FFT can do whose window fits
        assert!(validate_half_size(16380).is_ok());
        assert_eq!(validate_half_size(16384), Err("must be at most 16383: the window is cast to 16 bits"));
    }

    #[test]
    fn radix_17_is_the_largest_kiss_fft_does() {
        assert!(validate_half_size(17).is_ok());
        assert!(validate_half_size(4 * 17).is_ok());
        assert_eq!(validate_half_size(19), Err("has a prime factor above 17, which KISS FFT cannot do"));
    }

    #[test]
    fn rate_limits_sit_on_the_c_overflow_boundaries() {
        assert!(rate_settings(8, 32767).is_ok());
        assert!(rate_settings(8, 32768).is_err());
        // half a second: 160 samples at 320 Hz make beta0 = 32768
        assert!(rate_settings(160, 321).is_ok());
        assert!(rate_settings(160, 320).is_err());
    }

    #[test]
    fn the_notch_radius_steps_at_12_and_24_khz() {
        let radius = |rate| rate_settings(8, rate).unwrap()[3];
        assert_eq!([radius(11999), radius(12000)], [29491, 32178]);
        assert_eq!([radius(23999), radius(24000)], [32178, 32506]);
    }
}
