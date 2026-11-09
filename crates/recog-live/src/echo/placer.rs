//! `TimelinePlacer` and `_ReferenceRing`: stamped blocks onto one timeline.

use super::SR;

/// Turns stamped blocks into one stream that holds to the stamps.
///
/// A hole becomes zeros: audio closing up shifts the echo, while a hole barely troubles
/// Speex. The device clock's slow drift against its stamps is taken up one sample at a
/// time.
#[derive(Debug, Clone)]
pub struct TimelinePlacer {
    sample_rate: u32,
    /// The stream position the next block is expected at, None before the first.
    pub next_position: Option<i64>,
    drift: f64,
}

impl TimelinePlacer {
    /// Below this a difference is not a hole: stamps jitter by under 0.1 sample and
    /// drift keeps the difference under 2, while the microphone child loses 7-15
    /// samples now and then.
    pub const HOLE_SAMPLES: f64 = 4.0;
    /// Longer than this the capture was off: start over rather than fill it.
    pub const GAP_S: f64 = 1.0;
    pub const DRIFT_SMOOTHING_S: f64 = 5.0;

    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        Self { sample_rate, next_position: None, drift: 0.0 }
    }

    /// The block as it belongs in the stream, and whether the stream started over.
    pub fn place(&mut self, samples: &[f64], time_s: f64) -> (Vec<f64>, bool) {
        let rate = f64::from(self.sample_rate);
        let position = time_s * rate;
        #[allow(clippy::cast_possible_wrap)]
        let len = samples.len() as i64;
        let next = match self.next_position {
            #[allow(clippy::cast_precision_loss)]
            Some(next) if (position - next as f64).abs() <= Self::GAP_S * rate => next,
            previous => {
                #[allow(clippy::cast_possible_truncation)]
                let start = position.round_ties_even() as i64;
                self.next_position = Some(start + len);
                self.drift = 0.0;
                return (samples.to_vec(), previous.is_some());
            }
        };
        #[allow(clippy::cast_precision_loss)]
        let difference = position - next as f64;
        let skip: i64 = if difference.abs() > Self::HOLE_SAMPLES {
            self.drift = 0.0;
            #[allow(clippy::cast_possible_truncation)]
            let skip = difference.round_ties_even() as i64;
            skip
        } else {
            #[allow(clippy::cast_precision_loss)]
            let weight = (samples.len() as f64 / (Self::DRIFT_SMOOTHING_S * rate)).min(1.0);
            self.drift += weight * (difference - self.drift);
            if self.drift.abs() >= 1.0 {
                let skip = if self.drift > 0.0 { 1 } else { -1 };
                #[allow(clippy::cast_precision_loss)]
                {
                    self.drift -= skip as f64;
                }
                skip
            } else {
                0
            }
        };
        let placed: Vec<f64> = if skip > 0 {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let mut v = vec![0.0; skip as usize];
            v.extend_from_slice(samples);
            v
        } else if skip < 0 {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let drop = ((-skip) as usize).min(samples.len());
            samples[drop..].to_vec()
        } else {
            samples.to_vec()
        };
        #[allow(clippy::cast_possible_wrap)]
        let placed_len = placed.len() as i64;
        self.next_position = Some(next + placed_len);
        (placed, false)
    }
}

impl Default for TimelinePlacer {
    fn default() -> Self {
        Self::new(SR)
    }
}

/// The reference by its position on the timeline; one writer, one reader.
#[derive(Debug, Clone)]
pub struct ReferenceRing {
    data: Vec<f64>,
    /// One past the last position written, None before the first write.
    pub end: Option<i64>,
}

impl ReferenceRing {
    /// Four seconds of reference.
    pub const CAPACITY: usize = 4 * SR as usize;

    #[must_use]
    pub fn new() -> Self {
        Self { data: vec![0.0; Self::CAPACITY], end: None }
    }

    #[allow(clippy::cast_possible_wrap)]
    const CAP: i64 = Self::CAPACITY as i64;

    pub fn write(&mut self, position: i64, samples: &[f64]) {
        let n = samples.len().min(Self::CAPACITY);
        let samples = &samples[samples.len() - n..];
        #[allow(clippy::cast_possible_wrap)]
        let position = position + (samples.len() as i64 - n as i64);
        #[allow(clippy::cast_possible_wrap)]
        let n_i = n as i64;
        if let Some(end) = self.end
            && position > end
        {
            // Whatever was never delivered is silence, not an older round of the ring.
            let mut start = end;
            while start < position {
                let count = (position - start).min(Self::CAP);
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                self.put(start, &vec![0.0; count as usize]);
                start += Self::CAP;
            }
        }
        self.put(position, samples);
        self.end = Some(match self.end {
            None => position + n_i,
            Some(end) => end.max(position + n_i),
        });
    }

    fn put(&mut self, position: i64, samples: &[f64]) {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let at = position.rem_euclid(Self::CAP) as usize;
        let first = samples.len().min(Self::CAPACITY - at);
        self.data[at..at + first].copy_from_slice(&samples[..first]);
        let rest = samples.len() - first;
        self.data[..rest].copy_from_slice(&samples[first..]);
    }

    /// `n` samples from `position`, and whether all of them are real.
    #[must_use]
    pub fn read(&self, position: i64, n: usize) -> (Vec<f64>, bool) {
        #[allow(clippy::cast_possible_wrap)]
        let n_i = n as i64;
        let Some(end) = self.end else { return (vec![0.0; n], false) };
        if position + n_i > end || position + n_i <= end - Self::CAP {
            return (vec![0.0; n], false);
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let out = (position..position + n_i).map(|p| self.data[p.rem_euclid(Self::CAP) as usize]).collect();
        (out, position >= end - Self::CAP)
    }
}

impl Default for ReferenceRing {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consecutive_blocks_place_stays_untouched() {
        let mut placer = TimelinePlacer::new(SR);
        let (a, _) = placer.place(&[1.0; 160], 10.0);
        let (b, _) = placer.place(&[2.0; 160], 10.01);
        assert_eq!(a.len(), 160);
        assert_eq!(b, vec![2.0; 160]);
    }

    #[test]
    fn hole_place_zero_fills_to_the_block_time() {
        let mut placer = TimelinePlacer::new(SR);
        placer.place(&[1.0; 160], 10.0);
        let (placed, jumped) = placer.place(&[1.0; 160], 10.05);
        assert!(!jumped);
        assert_eq!(placed.len(), 800);
        assert!(placed[..640].iter().all(|&v| v == 0.0) && placed[640..].iter().all(|&v| v != 0.0));
    }

    #[test]
    fn overlap_place_drops_to_the_block_time() {
        let mut placer = TimelinePlacer::new(SR);
        placer.place(&[1.0; 1600], 10.0);
        let ramp: Vec<f64> = (0..320).map(f64::from).collect();
        let (placed, _) = placer.place(&ramp, 10.09);
        assert_eq!(placed, (160..320).map(f64::from).collect::<Vec<_>>());
    }

    /// given: a device whose samples run 0.1 per second behind their stamps, as measured
    /// when: minutes pass
    /// then: single zero samples hold the stream to the stamps, no larger steps
    #[test]
    fn slow_clock_drift_place_moves_one_sample_at_a_time() {
        let mut placer = TimelinePlacer::new(SR);
        let rate = 15_999.9;
        let mut t = 100.0;
        let mut extra = 0_i64;
        for _ in 0..120 * 100 {
            let (placed, _) = placer.place(&[1.0; 160], t);
            #[allow(clippy::cast_possible_wrap)]
            let extra_now = placed.len() as i64 - 160;
            assert!(extra_now.abs() <= 1);
            extra += extra_now;
            t += 160.0 / rate;
        }
        assert!((8..=16).contains(&extra), "{extra}");
    }

    /// given: the capture was off between meetings
    /// when: audio comes back an hour later
    /// then: nobody gets an hour of zeros
    #[test]
    fn long_gap_place_starts_over_instead_of_filling_it() {
        let mut placer = TimelinePlacer::new(SR);
        placer.place(&[1.0; 160], 10.0);
        let (placed, jumped) = placer.place(&[1.0; 160], 3610.0);
        assert!(jumped && placed.len() == 160);
    }

    #[test]
    fn skipped_positions_ring_reads_them_as_silence() {
        let mut ring = ReferenceRing::new();
        ring.write(0, &[1.0; 100]);
        ring.write(200, &[2.0; 100]);
        let (out, real) = ring.read(50, 200);
        assert!(real);
        assert!(out[..50].iter().all(|&v| v == 1.0) && out[50..150].iter().all(|&v| v == 0.0));
        assert!(out[150..].iter().all(|&v| v == 2.0));
    }

    #[test]
    fn beyond_what_was_written_ring_read_is_zeros_and_not_real() {
        let mut ring = ReferenceRing::new();
        assert_eq!(ring.read(0, 4), (vec![0.0; 4], false));
        ring.write(0, &[1.0; 10]);
        assert_eq!(ring.read(8, 4), (vec![0.0; 4], false));
    }
}
