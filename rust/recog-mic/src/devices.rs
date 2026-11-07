//! Choosing the device and turning its channels into mono.

/// What the lookup needs to know of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    /// The name CoreAudio gives it (`kAudioObjectPropertyName`), as PortAudio reports it.
    pub name: String,
    /// Whether it has an input stream.
    pub has_input: bool,
}

/// Index of the input device called `name`: indices shift between starts, names do not,
/// and an output of the same name (a headset is two devices) must not be picked.
pub fn find_input_device(devices: &[DeviceEntry], name: &str) -> Option<usize> {
    devices.iter().position(|device| device.has_input && device.name == name)
}

/// Channels the stream is opened with: like the Python worker, at most two.
pub fn capture_channels(device_channels: u16) -> u16 {
    device_channels.clamp(1, 2)
}

/// The mean of the channels of each interleaved frame, times `scale` (full scale of an
/// integer type to 1.0).
pub fn mono_frames<T: Copy + Into<f64>>(
    interleaved: &[T],
    channels: usize,
    scale: f64,
) -> impl Iterator<Item = f32> + '_ {
    let channels = channels.max(1);
    let factor = scale / channels as f64;
    interleaved.chunks_exact(channels).map(move |frame| {
        let sum: f64 = frame.iter().map(|sample| (*sample).into()).sum();
        (sum * factor) as f32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, has_input: bool) -> DeviceEntry {
        DeviceEntry { name: name.to_owned(), has_input }
    }

    #[test]
    fn an_output_named_like_the_input_before_it_is_skipped_and_the_input_is_found() {
        // given
        let devices = [entry("Same", false), entry("Other", true), entry("Same", true)];
        // when
        let found = find_input_device(&devices, "Same");
        // then
        assert_eq!(found, Some(2));
    }

    #[test]
    fn a_name_no_input_has_is_not_found() {
        // given
        let devices = [entry("Speakers", false), entry("Mic", true)];
        // when / then
        assert_eq!(find_input_device(&devices, "Speakers"), None);
        assert_eq!(find_input_device(&devices, "Gone"), None);
    }

    #[test]
    fn a_cyrillic_name_is_matched_exactly() {
        // given
        let devices = [entry("Микрофон MacBook Pro", true)];
        // when / then
        assert_eq!(find_input_device(&devices, "Микрофон MacBook Pro"), Some(0));
        assert_eq!(find_input_device(&devices, "Микрофон"), None);
    }

    #[test]
    fn a_stereo_block_becomes_the_mean_of_its_channels() {
        // given
        let block = [0.2f32, 0.4, 0.0, 1.0];
        // when
        let mono: Vec<f32> = mono_frames(&block, 2, 1.0).collect();
        // then
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.3).abs() < 1e-7 && (mono[1] - 0.5).abs() < 1e-7);
    }

    #[test]
    fn a_mono_block_passes_unchanged() {
        // given
        let block = [0.25f32, -0.5];
        // when
        let mono: Vec<f32> = mono_frames(&block, 1, 1.0).collect();
        // then
        assert_eq!(mono, vec![0.25, -0.5]);
    }

    #[test]
    fn integer_samples_are_scaled_to_full_scale_one() {
        // given
        let block = [i16::MIN, 16_384];
        // when
        let mono: Vec<f32> = mono_frames(&block, 1, 1.0 / 32_768.0).collect();
        // then
        assert_eq!(mono, vec![-1.0, 0.5]);
    }

    #[test]
    fn a_device_of_eight_channels_is_opened_with_two_and_a_mono_one_with_one() {
        // given / when / then
        assert_eq!(capture_channels(8), 2);
        assert_eq!(capture_channels(1), 1);
        assert_eq!(capture_channels(0), 1);
    }
}
