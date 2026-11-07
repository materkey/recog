//! From the device's blocks, each with its own ADC time, to the pipe's frames.
//!
//! Each output frame is stamped from the input block its first sample lies in, so the
//! stamps follow the device's clock as CoreAudio reports it, not a count of samples
//! (the device's clock drifts against host time by tens of ppm). A block whose stamp
//! does not continue the previous one (CoreAudio skipped a cycle, or the ring dropped
//! a block nobody read) ends the segment: the converter is flushed, and the next frame
//! starts at the new stamp, so the parent sees the hole in the stamps as before.

use std::collections::VecDeque;

use crate::resample::Resampler;

/// A gap or overlap between blocks above this ends the segment. Real holes are whole
/// IO cycles; the stamps' own jitter is microseconds.
pub const DISCONTINUITY_S: f64 = 0.0005;

/// One output frame: the ADC time of its first sample and its samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Host uptime seconds of the first sample.
    pub adc_time: f64,
    /// Mono samples at the output rate.
    pub samples: Vec<f32>,
}

/// Turns stamped blocks at the input rate into stamped frames at the output rate.
pub struct Framer {
    in_rate: f64,
    resampler: Resampler,
    /// (index of the block's first sample in the segment, its ADC time)
    blocks: VecDeque<(u64, f64)>,
    expected: Option<f64>,
    pending: Vec<f32>,
    pending_first: u64,
    frames: Vec<Frame>,
}

impl Framer {
    /// A framer converting from `in_rate` to `out_rate`.
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Framer {
            in_rate: f64::from(in_rate),
            resampler: Resampler::new(in_rate, out_rate),
            blocks: VecDeque::new(),
            expected: None,
            pending: Vec::new(),
            pending_first: 0,
            frames: Vec::new(),
        }
    }

    /// One block of mono input, `adc_time` being the ADC time of its first sample.
    pub fn push_block(&mut self, adc_time: f64, samples: &[f32]) {
        if let Some(expected) = self.expected
            && (adc_time - expected).abs() > DISCONTINUITY_S
        {
            self.end_segment();
        }
        if self.pending.is_empty() {
            self.pending_first = self.resampler.produced();
        }
        self.blocks.push_back((self.resampler.received(), adc_time));
        self.resampler.push(samples, &mut self.pending);
        self.expected = Some(adc_time + samples.len() as f64 / self.in_rate);
    }

    fn stamp(&self, n: u64) -> f64 {
        let position = self.resampler.position(n);
        let (start, adc) = self
            .blocks
            .iter()
            .rev()
            .find(|(start, _)| (*start as f64) <= position)
            .copied()
            .unwrap_or_else(|| self.blocks.front().copied().unwrap_or((0, 0.0)));
        adc + (position - start as f64) / self.in_rate
    }

    fn close_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let adc_time = self.stamp(self.pending_first);
        self.frames.push(Frame { adc_time, samples: std::mem::take(&mut self.pending) });
        // Blocks before the one the next output sample lies in are no longer needed.
        let next = self.resampler.position(self.resampler.produced());
        while self.blocks.len() > 1 && (self.blocks[1].0 as f64) <= next {
            self.blocks.pop_front();
        }
    }

    fn end_segment(&mut self) {
        if self.pending.is_empty() {
            self.pending_first = self.resampler.produced();
        }
        self.resampler.finish(&mut self.pending);
        // Positions depend only on the ratio, so the segment's blocks still stamp it.
        if !self.pending.is_empty() {
            let adc_time = self.stamp(self.pending_first);
            self.frames.push(Frame { adc_time, samples: std::mem::take(&mut self.pending) });
        }
        self.blocks.clear();
        self.expected = None;
    }

    /// The frames completed since the last call; a segment in progress yields what it has.
    pub fn take_frames(&mut self) -> Vec<Frame> {
        self.close_pending();
        std::mem::take(&mut self.frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 480;

    fn push_blocks(framer: &mut Framer, start: f64, period: f64, count: usize) {
        for k in 0..count {
            framer.push_block(start + k as f64 * period, &[0.1; BLOCK]);
        }
    }

    fn assert_close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn the_first_frame_carries_the_first_blocks_stamp_and_the_next_one_continues_it() {
        // given
        let mut framer = Framer::new(48_000, 16_000);
        push_blocks(&mut framer, 100.0, 0.01, 2);
        let first = framer.take_frames();
        // when
        framer.push_block(100.02, &[0.1; BLOCK]);
        let second = framer.take_frames();
        // then
        assert_eq!(first.len(), 1);
        assert_close(first[0].adc_time, 100.0);
        assert_close(second[0].adc_time, 100.0 + first[0].samples.len() as f64 / 16_000.0);
    }

    #[test]
    fn a_skipped_cycle_ends_the_frame_at_the_segments_end_and_the_next_starts_at_the_new_stamp() {
        // given: three blocks, then a block 20 ms late
        let mut framer = Framer::new(48_000, 16_000);
        push_blocks(&mut framer, 50.0, 0.01, 3);
        // when
        framer.push_block(50.05, &[0.1; BLOCK]);
        let frames = framer.take_frames();
        // then: the first segment is whole (30 ms), the second starts exactly at its stamp
        assert_eq!(frames.len(), 2);
        assert_close(frames[0].adc_time, 50.0);
        assert_eq!(frames[0].samples.len(), 3 * BLOCK / 3);
        assert_close(frames[1].adc_time, 50.05);
        let hole = frames[1].adc_time - (frames[0].adc_time + frames[0].samples.len() as f64 / 16_000.0);
        assert_close(hole, 0.02);
    }

    #[test]
    fn stamps_follow_a_drifting_device_clock_rather_than_the_sample_count() {
        // given: blocks 50 ppm longer than their sample count says
        let mut framer = Framer::new(48_000, 16_000);
        let period = 0.01 * (1.0 + 50e-6);
        push_blocks(&mut framer, 10.0, period, 1000);
        let before: usize = framer.take_frames().iter().map(|f| f.samples.len()).sum();
        // when
        framer.push_block(10.0 + 1000.0 * period, &[0.1; BLOCK]);
        let frames = framer.take_frames();
        // then: the frame starts in block 999, stamped by that block rather than by the
        // count of samples before it, which would be 999 blocks * 0.5 us early
        let by_count = 10.0 + before as f64 / 16_000.0;
        assert!((frames[0].adc_time - by_count - 999.0 * 0.01 * 50e-6).abs() < 1e-9);
    }

    #[test]
    fn jitter_under_half_a_millisecond_does_not_split_the_segment() {
        // given
        let mut framer = Framer::new(48_000, 16_000);
        framer.push_block(1.0, &[0.1; BLOCK]);
        // when
        framer.push_block(1.01 + 0.0002, &[0.1; BLOCK]);
        framer.push_block(1.02 - 0.0001, &[0.1; BLOCK]);
        // then
        assert_eq!(framer.take_frames().len(), 1);
    }

    #[test]
    fn taking_frames_with_nothing_new_gives_none() {
        // given
        let mut framer = Framer::new(48_000, 16_000);
        // when / then
        assert!(framer.take_frames().is_empty());
    }
}
