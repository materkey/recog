//! One frame of `speex_echo_cancellation`, as stages over the state.
//!
//! The order of the stages, and of every rounding inside them, is C's. Two of C's
//! accidents are reproduced on purpose because the output depends on them: the
//! far-end energy `Sxx` is accumulated twice per frame, and the de-emphasis memory
//! takes the output before it is clamped.

use super::spectral::{filter_dc_notch16, inner_prod, power_spectrum, spectral_mul_accum};
use super::state::{EchoCanceller, far_slot};
use crate::error::Error;
use crate::fixed::{add32, div32_16, extract16, mult16_16, mult16_16_p, sub16, sub32, to16, word2int};

/// The energies one frame hands from the filters to the adaptation.
pub(super) struct FrameEnergies {
    /// Far-end energy (twice, see the module docs).
    pub(super) sxx: i32,
    /// Background-filter error energy, with a noise floor.
    pub(super) see: i32,
    /// Error × background echo estimate.
    pub(super) sey: i32,
    /// Background echo estimate energy.
    pub(super) syy: i32,
}

/// `d[i] = s[i] − preemph·s[i−1]` clamped to ±32767, with `s[−1] = memory`. Returns
/// whether any sample was clamped.
///
/// C runs this as one loop that also sets the saturation flag per sample; the flag
/// only ever takes one value per frame, so it is raised once afterwards, and the
/// loop body is a pure function of two input samples, which vectorises.
fn pre_emphasis(signal: &[i16], memory: i16, preemph: i16, out: &mut [i16]) -> bool {
    let p = preemph as i32;
    let emphasis = |current: i16, previous: i16| sub32(current as i32, mult16_16_p(p, previous as i32, 15));
    let first = emphasis(signal[0], memory);
    let mut clipped = !(-32767..=32767).contains(&first);
    out[0] = extract16(first.clamp(-32767, 32767));
    for (o, pair) in out[1..].iter_mut().zip(signal.windows(2)) {
        let tmp32 = emphasis(pair[1], pair[0]);
        clipped |= !(-32767..=32767).contains(&tmp32);
        *o = extract16(tmp32.clamp(-32767, 32767));
    }
    clipped
}

/// Whether the canceller adds echo instead of removing it: a quarter of the
/// foreground error energy above the microphone energy plus a floor of
/// `N · 10000 / 64`.
fn adds_echo(sff: i32, sdd: i32, nn: i32) -> bool {
    sff >> 2 > add32(sdd, mult16_16(nn, 10000) >> 6)
}

impl EchoCanceller {
    /// `speex_echo_cancellation`: one frame of microphone and far-end (loudspeaker)
    /// samples in, the microphone without the echo out.
    ///
    /// ```
    /// use speexdsp_aec::{Config, EchoCanceller};
    ///
    /// let mut aec = EchoCanceller::new(Config::new(160, 1600)).unwrap();
    /// let mut out = vec![0i16; 160];
    /// let speech: Vec<i16> = (0..160).map(|t| ((t * 37 % 200) as i16 - 100) * 50).collect();
    /// aec.cancel(&speech, &[0; 160], &mut out).unwrap();
    /// assert_ne!(out, vec![0; 160]); // with a silent far end the voice passes
    /// ```
    ///
    /// # Errors
    /// [`Error::FrameLength`] unless all three slices are [`frame_size`](Self::frame_size)
    /// long.
    pub fn cancel(&mut self, mic: &[i16], far_end: &[i16], out: &mut [i16]) -> Result<(), Error> {
        let n = self.frame_size;
        if mic.len() != n || far_end.len() != n || out.len() != n {
            return Err(Error::FrameLength { expected: n, mic: mic.len(), far_end: far_end.len(), out: out.len() });
        }
        self.process(mic, far_end, out);
        Ok(())
    }

    fn process(&mut self, mic: &[i16], far_end: &[i16], out: &mut [i16]) {
        let nn = self.window_size as i32;
        self.cancel_count = self.cancel_count.wrapping_add(1);
        let ss = div32_16(11469, self.partitions as i32);
        let ss_1 = to16(sub16(32767, ss));

        self.read_microphone(mic);
        let sxx = self.read_far_end(far_end);
        let sff = self.foreground_error();
        self.update_weights();
        let (dbf, see) = self.background_error();
        let see = self.choose_filter(sff, see, dbf);
        let (sey, syy, sdd) = self.write_output(mic, out);

        if !(syy >= 0 && sxx >= 0 && see >= 0) {
            // "Things have gone really bad"
            self.screwed_up += 50;
            out.fill(0);
        } else if adds_echo(sff, sdd, nn) {
            self.screwed_up += 1;
        } else {
            self.screwed_up = 0;
        }
        if self.screwed_up >= 50 {
            self.reset();
            self.resets += 1;
            return;
        }

        let see = see.max(mult16_16(nn, 100) >> 6);
        let n = self.frame_size;
        let sxx = sxx.wrapping_add(inner_prod(&self.x[n..], &self.x[n..]));
        let current = self.far_slot(0);
        power_spectrum(&self.far_spectra[current], &mut self.xf);
        self.update_statistics(&FrameEnergies { sxx, see, sey, syy }, ss, ss_1);
    }

    /// DC notch, then pre-emphasis of the microphone into `input`.
    fn read_microphone(&mut self, mic: &[i16]) {
        filter_dc_notch16(mic, self.notch_radius, &mut self.notched, &mut self.notch_mem);
        let clipped = pre_emphasis(&self.notched, self.mem_d, self.preemph, &mut self.input);
        self.mem_d = self.notched[self.frame_size - 1];
        if clipped && self.saturated == 0 {
            self.saturated = 1;
        }
    }

    /// Pre-emphasis of the far end into the second half of `x`, its spectrum into
    /// partition 0 of the history. Returns the far-end energy.
    fn read_far_end(&mut self, far_end: &[i16]) -> i32 {
        let n = self.frame_size;
        self.x.copy_within(n.., 0);
        let clipped = pre_emphasis(far_end, self.mem_x, self.preemph, &mut self.x[n..]);
        self.mem_x = far_end[n - 1];
        if clipped {
            // a saturated far end freezes adaptation for M + 1 frames
            self.saturated = self.partitions as i32 + 1;
        }
        // the oldest spectrum's slot becomes the newest (C moves the whole history)
        self.far_head = (self.far_head + self.partitions) % (self.partitions + 1);
        let current = self.far_slot(0);
        self.fft.forward(&mut self.x, &mut self.far_spectra[current]);
        // C also accumulates the far-end power spectrum here, into Xf, which is
        // zeroed before anything reads it; that dead pass is left out
        inner_prod(&self.x[n..], &self.x[n..])
    }

    /// Echo estimate of the foreground filter into `e[n..]`, the error with it into
    /// `e[..n]`. Returns the error energy.
    fn foreground_error(&mut self) -> i32 {
        let n = self.frame_size;
        let (far, head, slots, nn) = (&self.far_spectra, self.far_head, self.partitions + 1, self.window_size);
        let filter = &self.foreground;
        let partitions = (0..self.partitions).map(|j| (&far[far_slot(head, slots, nn, j)], &filter[j * nn..(j + 1) * nn]));
        spectral_mul_accum(partitions, &mut self.echo_spectrum, &mut self.sums, |w| w as i32);
        self.fft.inverse(&self.echo_spectrum, &mut self.e);
        for i in 0..n {
            self.e[i] = sub16(self.input[i] as i32, self.e[i + n] as i32) as i16;
        }
        inner_prod(&self.e[..n], &self.e[..n])
    }

    /// Echo estimate of the background filter into `y[n..]`. Returns the energy of
    /// the difference between the two estimates (plus 10) and the background error
    /// energy, leaving that error in `e[..n]`.
    fn background_error(&mut self) -> (i32, i32) {
        let n = self.frame_size;
        let (far, head, slots, nn) = (&self.far_spectra, self.far_head, self.partitions + 1, self.window_size);
        let filter = &self.weights;
        let partitions = (0..self.partitions).map(|j| (&far[far_slot(head, slots, nn, j)], &filter[j * nn..(j + 1) * nn]));
        spectral_mul_accum(partitions, &mut self.echo_spectrum, &mut self.sums, |w| w >> 16);
        self.fft.inverse(&self.echo_spectrum, &mut self.y);
        for i in 0..n {
            self.e[i] = sub16(self.e[i + n] as i32, self.y[i + n] as i32) as i16;
        }
        let dbf = 10i32.wrapping_add(inner_prod(&self.e[..n], &self.e[..n]));
        for i in 0..n {
            self.e[i] = sub16(self.input[i] as i32, self.y[i + n] as i32) as i16;
        }
        (dbf, inner_prod(&self.e[..n], &self.e[..n]))
    }

    /// The output with de-emphasis, then the error and echo spectra for the next
    /// update. Returns `Sey`, `Syy` and `Sdd`.
    fn write_output(&mut self, mic: &[i16], out: &mut [i16]) -> (i32, i32, i32) {
        let n = self.frame_size;
        for i in 0..n {
            let mut tmp_out = sub32(self.input[i] as i32, self.e[i + n] as i32);
            tmp_out = add32(tmp_out, mult16_16_p(self.preemph as i32, self.mem_e as i32, 15));
            // an arbitrary test for saturation in the microphone signal
            if (mic[i] <= -32000 || mic[i] >= 32000) && self.saturated == 0 {
                self.saturated = 1;
            }
            out[i] = word2int(tmp_out);
            // C stores the unclamped value into the 16-bit memory: it wraps
            self.mem_e = tmp_out as i16;
        }
        for i in 0..n {
            self.e[i + n] = self.e[i];
            self.e[i] = 0;
        }
        let sey = inner_prod(&self.e[n..], &self.y[n..]);
        let syy = inner_prod(&self.y[n..], &self.y[n..]);
        let sdd = inner_prod(&self.input, &self.input);
        self.fft.forward(&mut self.e, &mut self.error_spectrum);
        self.y[..n].fill(0);
        self.fft.forward(&mut self.y, &mut self.echo_spectrum);
        power_spectrum(&self.error_spectrum, &mut self.rf);
        power_spectrum(&self.echo_spectrum, &mut self.yf);
        (sey, syy, sdd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn echo_counts_as_added_only_above_the_floor() {
        // N = 320: the floor is 320 * 10000 / 64 = 50000
        assert!(!adds_echo(4 * 50_000, 0, 320));
        assert!(adds_echo(4 * 50_001, 0, 320));
        assert!(!adds_echo(4 * 51_000, 1000, 320));
    }

    #[test]
    fn a_microphone_sample_at_32000_freezes_the_next_update() {
        for (sample, frozen) in [(32_000, 1), (31_999, 0), (-32_000, 1), (-31_999, 0)] {
            let mut aec = EchoCanceller::new(Config::new(160, 1600)).unwrap();
            let mut mic = [0i16; 160];
            mic[100] = sample;
            let mut out = [0i16; 160];
            aec.cancel(&mic, &[0; 160], &mut out).unwrap();
            assert_eq!(aec.saturated, frozen, "microphone sample {sample}");
        }
    }

    #[test]
    fn an_overflowing_far_end_energy_resets_the_canceller_and_silences_the_frame() {
        // alternating full scale: 80 pairs of 2 * 32767^2 >> 6 wrap Sxx negative
        let far: Vec<i16> = (0..160).map(|i| if i % 2 == 0 { 32767 } else { -32767 }).collect();
        let mut aec = EchoCanceller::new(Config::new(160, 1600)).unwrap();
        let mut out = [1i16; 160];
        aec.cancel(&[1000; 160], &far, &mut out).unwrap();
        assert_eq!(aec.resets(), 1);
        assert_eq!(out, [0; 160]);
    }
}

