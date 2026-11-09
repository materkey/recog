//! Real FFTs in speexdsp's packed layout: `kiss_fftr2` / `kiss_fftri2` and the
//! `spx_fft` / `spx_ifft` wrappers of `fftwrap.c` (KISS, fixed point).
//!
//! A spectrum of an `n`-sample real frame is `n` 16-bit values:
//! `[DC, Re 1, Im 1, Re 2, Im 2, …, Re n/2-1, Im n/2-1, Nyquist]`.

use super::complex::{ComplexFft, Cpx, cexp2};
use crate::fixed::{add32, div32, mult16_16, pshr32, shl32, sub32};

/// `kiss_fftr_state`: a real FFT of `2 · ncfft` samples through a complex FFT of
/// `ncfft`, its working samples split into real and imaginary parts.
#[derive(Clone, Debug)]
struct RealFft {
    ncfft: usize,
    sub: ComplexFft,
    super_twiddles: Vec<Cpx>,
    re: Vec<i16>,
    im: Vec<i16>,
    tmp_re: Vec<i16>,
    tmp_im: Vec<i16>,
}

impl RealFft {
    /// `kiss_fftr_alloc(nfft, inverse)` for an even `nfft`.
    fn new(nfft: usize, inverse: bool) -> Self {
        let ncfft = nfft / 2;
        let super_twiddles = (0..ncfft)
            .map(|i| {
                let phase = (i + ncfft / 2) as i32;
                let phase = if inverse { phase } else { -phase };
                cexp2(div32(shl32(phase, 16), ncfft as i32))
            })
            .collect();
        Self {
            ncfft,
            sub: ComplexFft::new(ncfft, inverse),
            super_twiddles,
            re: vec![0; ncfft],
            im: vec![0; ncfft],
            tmp_re: vec![0; ncfft],
            tmp_im: vec![0; ncfft],
        }
    }

    /// `kiss_fftr2`: time samples to a packed spectrum, scaled by 1/n.
    fn forward(&mut self, time: &[i16], freq: &mut [i16]) {
        let n = self.ncfft;
        // the samples as complex pairs (time[2k], time[2k+1]), in shuffle order
        for ((r, i), &src) in self.re.iter_mut().zip(self.im.iter_mut()).zip(&self.sub.perm) {
            let at = 2 * src as usize;
            (*r, *i) = (time[at], time[at + 1]);
        }
        self.sub.run(&mut self.re, &mut self.im);
        let (re, im) = (&self.re, &self.im);
        let at = |k: usize| Cpx { r: re[k], i: im[k] };
        let tdc = at(0).fixdiv(2);
        freq[0] = tdc.r.wrapping_add(tdc.i);
        freq[2 * n - 1] = tdc.r.wrapping_sub(tdc.i);
        for k in 1..=n / 2 {
            let (a, b, tw) = (at(k), at(n - k), self.super_twiddles[k]);
            let f2k = Cpx {
                r: (sub32(a.r as i32, b.r as i32) >> 1) as i16,
                i: pshr32(add32(a.i as i32, b.i as i32), 1) as i16,
            };
            let f1kr = shl32(add32(a.r as i32, b.r as i32), 13);
            let f1ki = shl32(sub32(a.i as i32, b.i as i32), 13);
            let twr = sub32(mult16_16(f2k.r as i32, tw.r as i32), mult16_16(f2k.i as i32, tw.i as i32)) >> 1;
            let twi = add32(mult16_16(f2k.i as i32, tw.r as i32), mult16_16(f2k.r as i32, tw.i as i32)) >> 1;
            // at k = n/2 both pairs are the same two slots and the second write wins
            freq[2 * k - 1] = pshr32(f1kr.wrapping_add(twr), 15) as i16;
            freq[2 * k] = pshr32(f1ki.wrapping_add(twi), 15) as i16;
            freq[2 * (n - k) - 1] = pshr32(f1kr.wrapping_sub(twr), 15) as i16;
            freq[2 * (n - k)] = pshr32(twi.wrapping_sub(f1ki), 15) as i16;
        }
    }

    /// `kiss_fftri2`: a packed spectrum back to time samples, not scaled.
    fn inverse(&mut self, freq: &[i16], time: &mut [i16]) {
        let n = self.ncfft;
        self.tmp_re[0] = freq[0].wrapping_add(freq[2 * n - 1]);
        self.tmp_im[0] = freq[0].wrapping_sub(freq[2 * n - 1]);
        for k in 1..=n / 2 {
            let fk = Cpx { r: freq[2 * k - 1], i: freq[2 * k] };
            let fnkc = Cpx { r: freq[2 * (n - k) - 1], i: freq[2 * (n - k)].wrapping_neg() };
            let fek = fk.add(fnkc);
            let fok = fk.sub(fnkc).mul(self.super_twiddles[k]);
            let (high, low) = (fek.add(fok), fek.sub(fok));
            // at k = n/2 both land in the same slot and the second, negated, wins
            (self.tmp_re[k], self.tmp_im[k]) = (high.r, high.i);
            (self.tmp_re[n - k], self.tmp_im[n - k]) = (low.r, low.i.wrapping_neg());
        }
        for ((r, i), &src) in self.re.iter_mut().zip(self.im.iter_mut()).zip(&self.sub.perm) {
            (*r, *i) = (self.tmp_re[src as usize], self.tmp_im[src as usize]);
        }
        self.sub.run(&mut self.re, &mut self.im);
        for ((pair, &r), &i) in time.as_chunks_mut::<2>().0.iter_mut().zip(&self.re).zip(&self.im) {
            (pair[0], pair[1]) = (r, i);
        }
    }
}

/// The fixed-point real FFT speexdsp's echo canceller uses (`spx_fft_init(n)`),
/// with its exact rounding.
///
/// ```
/// use speexdsp_aec::fft::SpeexFft;
///
/// let mut fft = SpeexFft::new(8).unwrap();
/// let mut time = [1000i16, 0, 0, 0, 0, 0, 0, 0];
/// let mut spectrum = [0i16; 8];
/// fft.forward(&mut time, &mut spectrum);
/// // an impulse has a flat spectrum: DC, then (re, im) pairs, then Nyquist, all 1000/8
/// assert_eq!(spectrum, [125, 125, 0, 125, 0, 125, 0, 125]);
/// let mut back = [0i16; 8];
/// fft.inverse(&spectrum, &mut back);
/// assert_eq!(back, [1000, 0, 0, 0, 0, 0, 0, 0]);
/// ```
#[derive(Clone, Debug)]
pub struct SpeexFft {
    n: usize,
    forward: RealFft,
    backward: RealFft,
}

impl SpeexFft {
    /// A plan for frames of `n` samples: `n` even, `n / 2` below 16384 and made of
    /// radices of at most 17.
    ///
    /// # Errors
    /// [`crate::Error::InvalidFrameSize`] when `n` does not satisfy that.
    pub fn new(n: usize) -> Result<Self, crate::Error> {
        crate::mdf::validate_half_size(n / 2)
            .map_err(|reason| crate::Error::InvalidFrameSize { frame_size: n / 2, reason })?;
        if !n.is_multiple_of(2) {
            return Err(crate::Error::InvalidFrameSize { frame_size: n / 2, reason: "the window must be even" });
        }
        Ok(Self::plan(n))
    }

    /// [`new`](Self::new) for a length the caller has already validated.
    pub(crate) fn plan(n: usize) -> Self {
        Self { n, forward: RealFft::new(n, false), backward: RealFft::new(n, true) }
    }

    /// The frame length.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Always false: a plan has at least two samples.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// `spx_fft`: `input` to a packed spectrum scaled by 1/n.
    ///
    /// Like the C function, it first scales `input` up in place to use the full
    /// 16-bit range and afterwards scales it back down, so `input` comes back
    /// unchanged — except for a frame containing −32768, which C mis-scales.
    ///
    /// # Panics
    /// If either slice is not [`len`](Self::len) long.
    pub fn forward(&mut self, input: &mut [i16], output: &mut [i16]) {
        assert!(input.len() == self.n && output.len() == self.n, "frames must be {} samples", self.n);
        let shift = maximize_range(input, 32000);
        self.forward.forward(input, output);
        renorm_range(input, shift);
        renorm_range(output, shift);
    }

    /// `spx_ifft`: a packed spectrum back to `n` samples, not scaled.
    ///
    /// # Panics
    /// If either slice is not [`len`](Self::len) long.
    pub fn inverse(&mut self, input: &[i16], output: &mut [i16]) {
        assert!(input.len() == self.n && output.len() == self.n, "frames must be {} samples", self.n);
        self.backward.inverse(input, output);
    }
}

/// `maximize_range(in, in, bound, len)`: shift the frame left while its peak stays
/// at or below `bound`, returning the shift.
fn maximize_range(frame: &mut [i16], bound: i16) -> u32 {
    let mut max_val: i16 = if frame.contains(&i16::MIN) {
        // C's running maximum, overflow included (see the test below)
        let mut running: i16 = 0;
        for &v in frame.iter() {
            if v > running {
                running = v;
            }
            // `-in[i]` is an int; storing 32768 into the 16-bit max_val gives -32768
            if -(v as i32) > running as i32 {
                running = (-(v as i32)) as i16;
            }
        }
        running
    } else {
        // without -32768 the running maximum is just the largest magnitude, which
        // vectorises as a reduction
        frame.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0) as i16
    };
    let mut shift = 0;
    while max_val as i32 <= (bound as i32 >> 1) && max_val != 0 {
        max_val = ((max_val as i32) << 1) as i16;
        shift += 1;
    }
    for v in frame.iter_mut() {
        *v = ((*v as i32) << shift) as i16;
    }
    shift
}

/// `renorm_range(in, in, shift, len)`: `PSHR16` every sample by `shift`.
fn renorm_range(frame: &mut [i16], shift: u32) {
    let round = (1i32 << shift) >> 1;
    for v in frame.iter_mut() {
        *v = ((*v as i32 + round) >> shift) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_refuse_lengths_kiss_fft_cannot_do_and_name_the_frame() {
        let refused = |n| match SpeexFft::new(n) {
            Err(crate::Error::InvalidFrameSize { frame_size, reason }) => (frame_size, reason),
            other => panic!("{n}: {other:?}"),
        };
        assert_eq!(refused(1), (0, "must be at least 1"));
        assert_eq!(refused(7), (3, "the window must be even"));
        // 19 is a prime above 17
        assert_eq!(refused(38).0, 19);
        assert!(SpeexFft::new(34).is_ok_and(|plan| plan.len() == 34 && !plan.is_empty()));
    }

    #[test]
    fn range_scaling_round_trips_except_for_the_minimum() {
        let mut frame = [100i16, -3000, 7, 0];
        let shift = maximize_range(&mut frame, 32000);
        assert_eq!(shift, 3);
        assert_eq!(frame, [800, -24000, 56, 0]);
        renorm_range(&mut frame, shift);
        assert_eq!(frame, [100, -3000, 7, 0]);

        let mut silent = [0i16; 4];
        assert_eq!(maximize_range(&mut silent, 32000), 0);

        // speexdsp's bug: -32768 stores 32768 into the 16-bit max_val, which becomes
        // -32768, so the running maximum restarts from the samples after it. Here it
        // ends at 1 and the frame is shifted by 14, wrapping the real peaks to zero.
        let mut bad = [20000i16, -32768, 1, 0];
        assert_eq!(maximize_range(&mut bad, 32000), 14);
        assert_eq!(bad, [0, 0, 16384, 0]);
        // with nothing after it the overflowed maximum gives a shift of 1
        let mut last = [100i16, -32768];
        assert_eq!(maximize_range(&mut last, 32000), 1);
        assert_eq!(last, [200, 0]);
    }

    #[test]
    fn forward_then_inverse_returns_the_frame() {
        for &n in &[8usize, 160, 256, 280, 320, 480, 512] {
            let mut fft = SpeexFft::new(n).unwrap();
            let mut input: Vec<i16> = (0..n).map(|t| (((t * 7919) % 4001) as i32 - 2000) as i16 * 4).collect();
            let original = input.clone();
            let mut spectrum = vec![0i16; n];
            fft.forward(&mut input, &mut spectrum);
            assert_eq!(input, original, "spx_fft restores its input");
            let mut back = vec![0i16; n];
            fft.inverse(&spectrum, &mut back);
            let worst = original.iter().zip(&back).map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap();
            assert!(worst <= n as i32 / 2 + 8, "n={n}: round trip off by {worst}");
        }
    }

    #[test]
    #[should_panic(expected = "frames must be 8 samples")]
    fn forward_refuses_a_short_input() {
        SpeexFft::new(8).unwrap().forward(&mut [0; 4], &mut [0; 8]);
    }

    #[test]
    #[should_panic(expected = "frames must be 8 samples")]
    fn forward_refuses_a_short_output() {
        SpeexFft::new(8).unwrap().forward(&mut [0; 8], &mut [0; 4]);
    }

    #[test]
    #[should_panic(expected = "frames must be 8 samples")]
    fn inverse_refuses_a_short_input() {
        SpeexFft::new(8).unwrap().inverse(&[0; 4], &mut [0; 8]);
    }

    #[test]
    #[should_panic(expected = "frames must be 8 samples")]
    fn inverse_refuses_a_short_output() {
        SpeexFft::new(8).unwrap().inverse(&[0; 8], &mut [0; 4]);
    }

    #[test]
    fn invalid_sizes_are_rejected() {
        assert!(SpeexFft::new(7).is_err());
        assert!(SpeexFft::new(2 * 19).is_err());
        assert!(SpeexFft::new(0).is_err());
        let fft = SpeexFft::new(8).unwrap();
        assert_eq!(fft.len(), 8);
        assert!(!fft.is_empty());
    }
}
