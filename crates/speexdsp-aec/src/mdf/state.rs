//! The canceller's state: `SpeexEchoState_` with `speex_echo_state_init_mc`,
//! `speex_echo_state_reset` and the sampling-rate part of `speex_echo_ctl`, for one
//! microphone and one loudspeaker (what pyaec and recog use).

use super::settings::{Config, MAX_PARTITIONS, initial_prop, rate_settings, validate_half_size};
use crate::error::Error;
use crate::fft::SpeexFft;
use crate::fixed::{div32_16, mult16_16, q15, shl32};
use crate::math::cos;
use crate::pseudo_float::PFloat;

/// A fixed-point MDF acoustic echo canceller, bit-exact with speexdsp 1.2.1 built
/// with `FIXED_POINT` (as in pyaec 1.0.1).
///
/// ```
/// use speexdsp_aec::{Config, EchoCanceller};
///
/// let mut aec = EchoCanceller::new(Config::new(160, 1600)).unwrap();
/// let far_end = [1000i16; 160];
/// let mic = [0i16; 160];
/// let mut out = [0i16; 160];
/// aec.cancel(&mic, &far_end, &mut out).unwrap();
/// assert_eq!(out, [0; 160]); // nothing to remove from silence
/// assert!(aec.cancel(&mic[..80], &far_end, &mut out).is_err()); // pyaec would overflow here
/// ```
#[derive(Clone, Debug)]
pub struct EchoCanceller {
    // dimensions: n samples per frame, a window of N = 2n, M partitions
    pub(super) frame_size: usize,
    pub(super) window_size: usize,
    pub(super) partitions: usize,
    // counters and flags
    pub(super) cancel_count: i32,
    pub(super) adapted: bool,
    pub(super) saturated: i32,
    pub(super) screwed_up: i32,
    pub(super) resets: u64,
    // settings
    pub(super) sampling_rate: u32,
    pub(super) spec_average: i16,
    pub(super) beta0: i16,
    pub(super) beta_max: i16,
    pub(super) notch_radius: i16,
    pub(super) preemph: i16,
    pub(super) window: Vec<i16>,
    // adaptation statistics
    pub(super) sum_adapt: i32,
    pub(super) pey: PFloat,
    pub(super) pyy: PFloat,
    pub(super) davg1: i32,
    pub(super) davg2: i32,
    pub(super) dvar1: PFloat,
    pub(super) dvar2: PFloat,
    // time-domain signals: e (error), x (far end), input (microphone), y (echo)
    pub(super) e: Vec<i16>,
    pub(super) x: Vec<i16>,
    pub(super) notched: Vec<i16>,
    pub(super) input: Vec<i16>,
    pub(super) y: Vec<i16>,
    // spectra: X (far end, M + 1 frames of history in a ring whose age-0 slot is
    // far_head; C shifts the whole history by one frame instead), Y (echo), E (error)
    pub(super) far_spectra: Vec<i16>,
    pub(super) far_head: usize,
    pub(super) echo_spectrum: Vec<i16>,
    pub(super) error_spectrum: Vec<i16>,
    pub(super) phi: Vec<i32>,
    // filters: W (background, Q27), foreground (Q11), prop (per-partition rate share)
    pub(super) weights: Vec<i32>,
    pub(super) foreground: Vec<i16>,
    pub(super) prop: Vec<i16>,
    // per-bin statistics, n + 1 bins
    pub(super) power: Vec<i32>,
    pub(super) power_1: Vec<PFloat>,
    pub(super) rf: Vec<i32>,
    pub(super) yf: Vec<i32>,
    pub(super) xf: Vec<i32>,
    pub(super) eh: Vec<i32>,
    pub(super) yh: Vec<i32>,
    // scratch
    pub(super) wtmp: Vec<i16>,
    pub(super) wtmp2: Vec<i16>,
    pub(super) sums: Vec<i32>,
    pub(super) fft: SpeexFft,
    // filter memories: pre-emphasis of x and d, de-emphasis of e, the DC notch
    pub(super) mem_x: i16,
    pub(super) mem_d: i16,
    pub(super) mem_e: i16,
    pub(super) notch_mem: [i32; 2],
}

impl EchoCanceller {
    /// `speex_echo_state_init`, then the sampling rate of `config`.
    ///
    /// # Errors
    /// [`Error::InvalidFrameSize`], [`Error::InvalidFilterLength`] or
    /// [`Error::UnsupportedSamplingRate`] for a configuration speexdsp would mishandle
    /// (divide by zero, abort, or overflow its 16-bit constants).
    pub fn new(config: Config) -> Result<Self, Error> {
        let Config { frame_size: n, filter_length, sampling_rate } = config;
        validate_half_size(n).map_err(|reason| Error::InvalidFrameSize { frame_size: n, reason })?;
        if filter_length == 0 {
            return Err(Error::InvalidFilterLength { filter_length, reason: "must be at least 1" });
        }
        let m = filter_length.div_ceil(n);
        if m > MAX_PARTITIONS {
            return Err(Error::InvalidFilterLength { filter_length, reason: "needs more than 32767 frames" });
        }
        let rate = rate_settings(n, sampling_rate)?;
        Ok(Self::fresh(n, m, sampling_rate, rate, initial_prop(m)))
    }

    /// A canceller that has heard nothing yet, for a checked configuration and the
    /// rate shares `prop`.
    fn fresh(n: usize, m: usize, sampling_rate: u32, rate: [i16; 4], prop: Vec<i16>) -> Self {
        let [spec_average, beta0, beta_max, notch_radius] = rate;
        let nn = 2 * n;
        let mut window = vec![0i16; nn];
        for i in 0..nn / 2 {
            let phase = div32_16(mult16_16(25736, (i << 1) as i32), nn as i32) as i16;
            let w = (16383 - shl32(cos(phase) as i32, 1)) as i16;
            window[i] = w;
            window[nn - i - 1] = w;
        }
        Self {
            frame_size: n,
            window_size: nn,
            partitions: m,
            cancel_count: 0,
            adapted: false,
            saturated: 0,
            screwed_up: 0,
            resets: 0,
            sampling_rate,
            spec_average,
            beta0,
            beta_max,
            notch_radius,
            preemph: q15::POINT_9 as i16,
            window,
            sum_adapt: 0,
            pey: PFloat::ONE,
            pyy: PFloat::ONE,
            davg1: 0,
            davg2: 0,
            dvar1: PFloat::ZERO,
            dvar2: PFloat::ZERO,
            e: vec![0; nn],
            x: vec![0; nn],
            notched: vec![0; n],
            input: vec![0; n],
            y: vec![0; nn],
            far_spectra: vec![0; (m + 1) * nn],
            far_head: 0,
            echo_spectrum: vec![0; nn],
            error_spectrum: vec![0; nn],
            phi: vec![0; nn],
            weights: vec![0; m * nn],
            foreground: vec![0; m * nn],
            prop,
            power: vec![0; n + 1],
            power_1: vec![PFloat::ONE; n + 1],
            rf: vec![0; n + 1],
            yf: vec![0; n + 1],
            xf: vec![0; n + 1],
            eh: vec![0; n + 1],
            yh: vec![0; n + 1],
            wtmp: vec![0; nn],
            wtmp2: vec![0; nn],
            sums: vec![0; nn],
            fft: SpeexFft::plan(nn),
            mem_x: 0,
            mem_d: 0,
            mem_e: 0,
            notch_mem: [0; 2],
        }
    }

    /// `speex_echo_state_reset`: forget the echo path. Called by the canceller
    /// itself when it diverges (counted by [`resets`](Self::resets)).
    ///
    /// Like C, it keeps the partition rate shares (`prop`) and the rate settings;
    /// the rest is as in a new canceller. C leaves the per-frame buffers as they
    /// were, but every frame writes those before reading them.
    pub fn reset(&mut self) {
        let rate = [self.spec_average, self.beta0, self.beta_max, self.notch_radius];
        let prop = std::mem::take(&mut self.prop);
        let fresh = Self::fresh(self.frame_size, self.partitions, self.sampling_rate, rate, prop);
        *self = Self { resets: self.resets, ..fresh };
    }

    /// `SPEEX_ECHO_SET_SAMPLING_RATE`: retune the rate-dependent constants (notch
    /// radius, smoothing, step size). pyaec never calls it, so its canceller runs
    /// with 8 kHz settings whatever the real rate.
    ///
    /// ```
    /// use speexdsp_aec::{Config, EchoCanceller};
    ///
    /// let mut aec = EchoCanceller::new(Config::new(160, 4096)).unwrap();
    /// aec.set_sampling_rate(16_000).unwrap();
    /// assert_eq!(aec.sampling_rate(), 16_000);
    /// assert!(aec.set_sampling_rate(48_000).is_err()); // C would go negative here
    /// ```
    ///
    /// # Errors
    /// [`Error::UnsupportedSamplingRate`] outside 1–32767 Hz or when a frame would
    /// last more than half a second.
    pub fn set_sampling_rate(&mut self, rate: u32) -> Result<(), Error> {
        let [spec_average, beta0, beta_max, notch_radius] = rate_settings(self.frame_size, rate)?;
        self.sampling_rate = rate;
        self.spec_average = spec_average;
        self.beta0 = beta0;
        self.beta_max = beta_max;
        self.notch_radius = notch_radius;
        Ok(())
    }

    /// `SPEEX_ECHO_GET_SAMPLING_RATE`.
    pub fn sampling_rate(&self) -> u32 {
        self.sampling_rate
    }

    /// `SPEEX_ECHO_GET_FRAME_SIZE`: samples per call.
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// `SPEEX_ECHO_GET_IMPULSE_RESPONSE_SIZE`: the modelled echo length, a whole
    /// number of frames.
    pub fn filter_length(&self) -> usize {
        self.partitions * self.frame_size
    }

    /// How many times the canceller reset itself after diverging (C prints
    /// "The echo canceller started acting funny and got slapped" instead).
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// Whether the filter has passed its minimal adaptation phase.
    pub fn is_adapted(&self) -> bool {
        self.adapted
    }

    /// Where the far-end spectrum `age` frames old lives in `far_spectra`.
    #[inline]
    pub(super) fn far_slot(&self, age: usize) -> std::ops::Range<usize> {
        far_slot(self.far_head, self.partitions + 1, self.window_size, age)
    }
}

/// [`EchoCanceller::far_slot`] for callers holding other fields mutably.
#[inline]
pub(super) const fn far_slot(head: usize, slots: usize, nn: usize, age: usize) -> std::ops::Range<usize> {
    let slot = (head + age) % slots;
    slot * nn..(slot + 1) * nn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recog_configuration_dimensions_and_rate_settings() {
        let aec = EchoCanceller::new(Config::new(160, 4096)).unwrap();
        assert_eq!((aec.window_size, aec.partitions, aec.filter_length()), (320, 26, 4160));
        assert_eq!((aec.spec_average, aec.beta0, aec.beta_max), (655, 1310, 327));
        assert_eq!(aec.notch_radius, 29491);
        let at16 = EchoCanceller::new(Config { sampling_rate: 16_000, ..Config::new(160, 4096) }).unwrap();
        assert_eq!((at16.spec_average, at16.beta0, at16.beta_max, at16.notch_radius), (327, 655, 163, 32178));
        let at24 = EchoCanceller::new(Config { sampling_rate: 24_000, ..Config::new(160, 4096) }).unwrap();
        assert_eq!(at24.notch_radius, 32506);
        // a Hann window in Q15; spx_cos(0) is exactly 1.0, so C's first value is -1
        assert_eq!(aec.window[0], -1);
        assert!(i32::from(aec.window[160]) > 32700, "{}", aec.window[160]);
        assert_eq!(aec.window[1], aec.window[318]);
    }

    #[test]
    fn invalid_configurations_are_refused() {
        assert!(matches!(EchoCanceller::new(Config::new(0, 100)), Err(Error::InvalidFrameSize { .. })));
        assert!(matches!(EchoCanceller::new(Config::new(16384, 100)), Err(Error::InvalidFrameSize { .. })));
        assert!(matches!(EchoCanceller::new(Config::new(19 * 8, 1600)), Err(Error::InvalidFrameSize { .. })));
        assert!(matches!(EchoCanceller::new(Config::new(160, 0)), Err(Error::InvalidFilterLength { .. })));
        assert!(matches!(EchoCanceller::new(Config::new(2, 2 * 40_000)), Err(Error::InvalidFilterLength { .. })));
        let rate = |r| EchoCanceller::new(Config { sampling_rate: r, ..Config::new(160, 1600) });
        assert!(matches!(rate(0), Err(Error::UnsupportedSamplingRate { .. })));
        assert!(matches!(rate(48_000), Err(Error::UnsupportedSamplingRate { .. })));
        // a 160-sample frame is half a second at 320 Hz: beta0 would be 32768
        assert!(matches!(rate(320), Err(Error::UnsupportedSamplingRate { .. })));
        assert!(rate(321).is_ok());
    }

    #[test]
    fn the_partition_count_stops_where_div32_16_would_cut_it() {
        // every frame divides by M in 16 bits: DIV32_16(11469, M)
        let mut largest = EchoCanceller::new(Config::new(2, 2 * 32767)).unwrap();
        assert_eq!(largest.filter_length(), 2 * 32767);
        let mut out = [0i16; 2];
        largest.cancel(&[100, -100], &[200, -200], &mut out).unwrap();
        let one_more = EchoCanceller::new(Config::new(2, 2 * 32767 + 1));
        assert!(matches!(one_more, Err(Error::InvalidFilterLength { filter_length: 65535, .. })));
    }

    #[test]
    fn a_far_end_correlation_below_one_is_clamped_to_one() {
        // Pyy can only drop below one through pseudo-float truncation; force it
        let mut aec = EchoCanceller::new(Config::new(8, 64)).unwrap();
        aec.pyy = PFloat::new(16384, -16);
        let mut out = [0i16; 8];
        aec.cancel(&[0; 8], &[0; 8], &mut out).unwrap();
        assert_eq!(aec.pyy, PFloat::ONE);
    }

    #[test]
    fn reset_keeps_settings_and_prop() {
        let mut aec = EchoCanceller::new(Config::new(160, 1600)).unwrap();
        aec.set_sampling_rate(16_000).unwrap();
        aec.weights[3] = 7;
        aec.adapted = true;
        aec.prop[0] = 1;
        aec.reset();
        assert_eq!(aec.weights[3], 0);
        assert!(!aec.is_adapted());
        assert_eq!(aec.prop[0], 1);
        assert_eq!(aec.sampling_rate(), 16_000);
        assert_eq!(aec.frame_size(), 160);
        assert_eq!(aec.resets(), 0);
    }
}
