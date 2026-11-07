//! Rate conversion from the device's own rate to the rate the parent asked for.
//!
//! The device is captured at its current nominal rate: asking CoreAudio for another
//! one would change the device's rate for every process using it. The converter is a
//! polyphase windowed-sinc filter (Kaiser window, 80 dB stopband from half the lower
//! rate, passband to 90 % of it) evaluated at each output sample's exact position on
//! the input timeline. Its kernel is centred on that position, so the output has no
//! group delay: output sample `n` of a segment is the band-limited input at input
//! position `n * in_rate / out_rate`, and is stamped with that position's time. The
//! price is latency, half a kernel (about 3 ms at 48 kHz), which only delays a frame.

use std::f64::consts::PI;

/// Stopband attenuation in dB.
const ATTENUATION_DB: f64 = 80.0;
/// Passband edge as a fraction of the lower rate's Nyquist frequency; the stopband
/// starts at that Nyquist frequency.
const PASSBAND: f64 = 0.9;

/// Streaming converter for one contiguous run of input (a segment).
pub struct Resampler {
    /// Input samples per `phases` output samples: output `n` sits at input `n * step / phases`.
    step: u64,
    phases: u64,
    /// Kernel half-width in input samples; each phase has `2 * half` taps.
    half: usize,
    table: Vec<f32>,
    /// Input of the current segment from index `base` on.
    buffer: Vec<f32>,
    base: u64,
    received: u64,
    produced: u64,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..200 {
        term *= quarter_square / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 { 1.0 } else { (PI * x).sin() / (PI * x) }
}

impl Resampler {
    /// A converter from `in_rate` to `out_rate`, both in Hz and non-zero.
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        assert!(in_rate > 0 && out_rate > 0, "sample rates must be positive");
        let divisor = gcd(u64::from(in_rate), u64::from(out_rate));
        let step = u64::from(in_rate) / divisor;
        let phases = u64::from(out_rate) / divisor;
        let (half, table) = if step == phases { (1, vec![1.0, 0.0]) } else { Self::design(in_rate, out_rate, phases) };
        Resampler { step, phases, half, table, buffer: Vec::new(), base: 0, received: 0, produced: 0 }
    }

    fn design(in_rate: u32, out_rate: u32, phases: u64) -> (usize, Vec<f32>) {
        let in_rate = f64::from(in_rate);
        let lower = f64::from(out_rate).min(in_rate);
        // Normalised to the input rate, in cycles per input sample.
        let transition = (1.0 - PASSBAND) * lower / 2.0 / in_rate;
        let cutoff = (1.0 + PASSBAND) / 2.0 * lower / 2.0 / in_rate;
        let beta = 0.1102 * (ATTENUATION_DB - 8.7);
        let taps = (ATTENUATION_DB - 8.0) / (2.285 * 2.0 * PI * transition) + 1.0;
        let half = (taps / 2.0).ceil() as usize;
        let width = 2 * half;
        let norm = bessel_i0(beta);
        let mut table = Vec::with_capacity(phases as usize * width);
        for phase in 0..phases {
            let fraction = phase as f64 / phases as f64;
            let row: Vec<f64> = (0..width)
                .map(|j| {
                    let t = j as f64 - half as f64 + 1.0 - fraction;
                    let r = t / half as f64;
                    let window = if r.abs() < 1.0 { bessel_i0(beta * (1.0 - r * r).sqrt()) / norm } else { 0.0 };
                    2.0 * cutoff * sinc(2.0 * cutoff * t) * window
                })
                .collect();
            let gain: f64 = row.iter().sum();
            table.extend(row.iter().map(|tap| (tap / gain) as f32));
        }
        (half, table)
    }

    /// Input samples of the current segment so far.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Output samples of the current segment so far.
    pub fn produced(&self) -> u64 {
        self.produced
    }

    /// Position of output sample `n` of the segment on its input timeline, in input samples.
    pub fn position(&self, n: u64) -> f64 {
        (n * self.step) as f64 / self.phases as f64
    }

    fn sample(&self, index: i64) -> f32 {
        if index < self.base as i64 {
            return 0.0;
        }
        self.buffer.get((index - self.base as i64) as usize).copied().unwrap_or(0.0)
    }

    fn emit(&mut self, out: &mut Vec<f32>) {
        let width = 2 * self.half;
        let scaled = self.produced * self.step;
        let centre = (scaled / self.phases) as i64;
        let phase = (scaled % self.phases) as usize;
        let first = centre - self.half as i64 + 1;
        let taps = &self.table[phase * width..(phase + 1) * width];
        let value: f32 = taps.iter().enumerate().map(|(j, tap)| tap * self.sample(first + j as i64)).sum();
        out.push(value);
        self.produced += 1;
    }

    fn centre_of_next(&self) -> u64 {
        self.produced * self.step / self.phases
    }

    /// Feeds input and appends every output sample whose kernel it completes.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buffer.extend_from_slice(input);
        self.received += input.len() as u64;
        while self.centre_of_next() + (self.half as u64) < self.received {
            self.emit(out);
        }
        // Keep what the next kernel reaches back to.
        let needed = (self.centre_of_next() + 1).saturating_sub(self.half as u64);
        let drop = needed.saturating_sub(self.base) as usize;
        if drop >= 4096 {
            self.buffer.drain(..drop);
            self.base += drop as u64;
        }
    }

    /// Ends the segment: appends the output samples that lie inside its input, the
    /// input after it taken as silence, and starts a new segment.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        while self.produced * self.step < self.received * self.phases {
            self.emit(out);
        }
        self.buffer.clear();
        self.base = 0;
        self.received = 0;
        self.produced = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, freq: f64, phase: f64, len: usize) -> Vec<f32> {
        (0..len).map(|k| (0.5 * (2.0 * PI * freq * k as f64 / rate + phase).sin()) as f32).collect()
    }

    fn convert(in_rate: u32, out_rate: u32, input: &[f32]) -> Vec<f32> {
        let mut resampler = Resampler::new(in_rate, out_rate);
        let mut out = Vec::new();
        resampler.push(input, &mut out);
        resampler.finish(&mut out);
        out
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    fn assert_tone_at_exact_times(in_rate: u32, out_rate: u32, freq: f64) {
        // given: a tone of known phase on the input timeline
        let input = tone(f64::from(in_rate), freq, 0.3, in_rate as usize);
        // when
        let out = convert(in_rate, out_rate, &input);
        // then: away from the edges, output n is the same tone at time n / out_rate
        let expected = tone(f64::from(out_rate), freq, 0.3, out.len());
        let edge = out_rate as usize / 50;
        let worst = out[edge..out.len() - edge]
            .iter()
            .zip(&expected[edge..out.len() - edge])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "{in_rate} -> {out_rate}: worst error {worst}");
    }

    #[test]
    fn a_1_khz_tone_at_48_khz_comes_out_at_16_khz_at_its_exact_times_without_delay() {
        assert_tone_at_exact_times(48_000, 16_000, 1000.0);
    }

    #[test]
    fn a_3_khz_tone_at_44_1_khz_comes_out_at_16_khz_at_its_exact_times_without_delay() {
        assert_tone_at_exact_times(44_100, 16_000, 3000.0);
    }

    #[test]
    fn a_tone_at_96_khz_comes_out_at_16_khz_at_its_exact_times_without_delay() {
        assert_tone_at_exact_times(96_000, 16_000, 2500.0);
    }

    #[test]
    fn a_1_khz_tone_at_8_khz_is_upsampled_to_16_khz_at_its_exact_times() {
        assert_tone_at_exact_times(8_000, 16_000, 1000.0);
    }

    fn assert_no_alias(in_rate: u32, freq: f64) {
        // given: a tone above the output's Nyquist frequency, which would fold into speech
        let input = tone(f64::from(in_rate), freq, 0.0, in_rate as usize);
        // when
        let out = convert(in_rate, 16_000, &input);
        // then: it is attenuated by at least 70 dB
        let edge = 400;
        let ratio = rms(&out[edge..out.len() - edge]) / rms(&input);
        assert!(ratio < 10f64.powf(-70.0 / 20.0), "{in_rate} Hz, {freq} Hz tone: {ratio}");
    }

    #[test]
    fn a_tone_above_8_khz_at_48_khz_does_not_alias_into_the_16_khz_output() {
        assert_no_alias(48_000, 8_100.0);
        assert_no_alias(48_000, 12_000.0);
        assert_no_alias(48_000, 23_000.0);
    }

    #[test]
    fn a_tone_above_8_khz_at_44_1_khz_does_not_alias_into_the_16_khz_output() {
        assert_no_alias(44_100, 9_000.0);
        assert_no_alias(44_100, 20_000.0);
    }

    #[test]
    fn speech_band_tones_keep_their_level() {
        // given / when / then: within 0.1 dB up to 7 kHz
        for freq in [100.0, 1000.0, 4000.0, 7000.0] {
            let input = tone(48_000.0, freq, 0.0, 48_000);
            let out = convert(48_000, 16_000, &input);
            let gain_db = 20.0 * (rms(&out[400..out.len() - 400]) / rms(&input)).log10();
            assert!(gain_db.abs() < 0.1, "{freq} Hz: {gain_db} dB");
        }
    }

    #[test]
    fn one_second_of_input_becomes_one_second_of_output() {
        // given / when / then
        assert_eq!(convert(48_000, 16_000, &vec![0.0; 48_000]).len(), 16_000);
        assert_eq!(convert(44_100, 16_000, &vec![0.0; 44_100]).len(), 16_000);
        assert_eq!(convert(8_000, 16_000, &vec![0.0; 8_000]).len(), 16_000);
        // a segment that ends between output samples keeps the ones inside it
        assert_eq!(convert(48_000, 16_000, &vec![0.0; 481]).len(), 161);
    }

    #[test]
    fn input_fed_in_uneven_pieces_gives_the_same_output_as_in_one_piece() {
        // given
        let input = tone(44_100.0, 440.0, 0.0, 20_000);
        let whole = convert(44_100, 16_000, &input);
        // when
        let mut resampler = Resampler::new(44_100, 16_000);
        let mut pieces = Vec::new();
        for chunk in input.chunks(477) {
            resampler.push(chunk, &mut pieces);
        }
        resampler.finish(&mut pieces);
        // then
        assert_eq!(pieces, whole);
    }

    #[test]
    fn equal_rates_pass_the_samples_through_unchanged() {
        // given
        let input = tone(16_000.0, 440.0, 0.0, 1000);
        // when / then
        assert_eq!(convert(16_000, 16_000, &input), input);
    }

    #[test]
    fn output_positions_lie_on_the_input_timeline() {
        // given
        let resampler = Resampler::new(44_100, 16_000);
        // when / then: output 160 is input 441, one hundredth of a second in
        assert_eq!(resampler.position(160), 441.0);
        assert_eq!(resampler.position(1), 441.0 / 160.0);
    }
}
