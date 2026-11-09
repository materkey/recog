//! The pipe's frames, the same as `recog/mic_worker.py` writes: a `<ddI` header (ADC
//! time of the first sample on the host uptime clock, wall clock of the same sample,
//! sample count, little endian) followed by that many float32 mono samples.

/// Bytes of the header: two f64 and one u32.
pub const HEADER_LEN: usize = 8 + 8 + 4;

/// Appends one frame to `out`.
pub fn encode_frame(adc_time: f64, wall_time: f64, samples: &[f32], out: &mut Vec<u8>) {
    let count = u32::try_from(samples.len()).expect("a frame holds fewer than 2^32 samples");
    out.reserve(HEADER_LEN + samples.len() * 4);
    out.extend_from_slice(&adc_time.to_le_bytes());
    out.extend_from_slice(&wall_time.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_samples_stamped_at_one_and_two_seconds_encode_as_struct_ddi_then_float32() {
        // given
        let mut out = Vec::new();
        // when
        encode_frame(1.0, 2.0, &[0.5, -0.25], &mut out);
        // then
        assert_eq!(out.len(), HEADER_LEN + 8);
        assert_eq!(&out[0..8], &1.0f64.to_le_bytes());
        assert_eq!(&out[8..16], &2.0f64.to_le_bytes());
        assert_eq!(&out[16..20], &2u32.to_le_bytes());
        assert_eq!(&out[20..24], &0.5f32.to_le_bytes());
        assert_eq!(&out[24..28], &(-0.25f32).to_le_bytes());
    }

    #[test]
    fn frames_appended_one_after_another_are_concatenated_in_order() {
        // given
        let mut out = Vec::new();
        // when
        encode_frame(1.0, 1.0, &[0.1], &mut out);
        encode_frame(3.0, 3.0, &[], &mut out);
        // then
        assert_eq!(out.len(), 2 * HEADER_LEN + 4);
        assert_eq!(&out[HEADER_LEN + 4..HEADER_LEN + 12], &3.0f64.to_le_bytes());
        assert_eq!(&out[2 * HEADER_LEN + 4 - 4..], &0u32.to_le_bytes());
    }
}
