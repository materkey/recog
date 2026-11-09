//! Opening a stream without changing the device for other processes.
//!
//! cpal 0.18 sets the device's physical format before opening an input stream: it
//! takes the first physical format with the requested sample type, bit depth, rate and
//! enough channels (`find_matching_physical_format` in coreaudio-rs) and writes it to
//! the device unless it equals the current one; with no such format it sets the
//! nominal rate instead, which is a read when the rate is the current one. A 32-bit
//! float request to a device running 24-bit integers would switch the device's format
//! under every other process using it (CoreAudio then fails their render calls with
//! -10863, as a sample-rate change did). So the request's sample type is chosen to be
//! one for which that write cannot happen; CoreAudio converts to it inside this process.

/// One entry of the device's available physical formats, as cpal reads them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalFormat {
    /// The entry's own rate and the range it covers.
    pub rate: f64,
    pub min_rate: f64,
    pub max_rate: f64,
    /// `mFormatID == kAudioFormatLinearPCM`.
    pub linear_pcm: bool,
    pub flags: u32,
    pub bits: u32,
    pub channels: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
}

const FLAG_IS_FLOAT: u32 = 1;
const FLAG_IS_SIGNED_INTEGER: u32 = 4;

/// The sample types the worker can ask for, in order of preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    F32,
    I32,
    I16,
}

impl Request {
    fn float(self) -> bool {
        self == Request::F32
    }

    fn bits(self) -> u32 {
        match self {
            Request::F32 | Request::I32 => 32,
            Request::I16 => 16,
        }
    }

    /// The sample type as cpal's `SampleFormat` prints it.
    pub fn name(self) -> &'static str {
        match self {
            Request::F32 => "f32",
            Request::I32 => "i32",
            Request::I16 => "i16",
        }
    }
}

/// coreaudio-rs's rate test, with its truncating casts.
fn covers(format: &PhysicalFormat, rate: u32) -> bool {
    let rate = rate as usize;
    format.rate as usize == rate || (rate >= format.min_rate as usize && rate <= format.max_rate as usize)
}

/// coreaudio-rs's `find_matching_physical_format` test of one entry.
fn matches(format: &PhysicalFormat, request: Request, rate: u32, channels: u16) -> bool {
    if !format.linear_pcm {
        return false;
    }
    let float = format.flags & FLAG_IS_FLOAT != 0;
    let int = format.flags & FLAG_IS_SIGNED_INTEGER != 0;
    if (float && int) || (request.float() && !float) || (!request.float() && !int) {
        return false;
    }
    request.bits() == format.bits && u32::from(channels) <= format.channels && covers(format, rate)
}

/// coreaudio-rs's `asbds_are_equal`.
fn same(a: &PhysicalFormat, b: &PhysicalFormat) -> bool {
    a.rate as u32 == b.rate as u32
        && a.linear_pcm == b.linear_pcm
        && a.flags == b.flags
        && a.bytes_per_packet == b.bytes_per_packet
        && a.frames_per_packet == b.frames_per_packet
        && a.bytes_per_frame == b.bytes_per_frame
        && a.channels == b.channels
        && a.bits == b.bits
}

/// The first sample type for which opening at the device's current `rate` leaves its
/// physical format as it is, or `None` when every one could change it.
///
/// Safe means: cpal finds no matching format (and only reads the nominal rate), or the
/// format it finds is the only one the device has at this rate, which therefore is the
/// current physical format, and cpal skips writing an equal one.
pub fn safe_request(formats: &[PhysicalFormat], rate: u32, channels: u16) -> Option<Request> {
    [Request::F32, Request::I32, Request::I16].into_iter().find(|request| {
        match formats.iter().find(|format| matches(format, *request, rate, channels)) {
            None => true,
            Some(found) => formats.iter().filter(|format| covers(format, rate)).all(|format| same(format, found)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(rate: f64, flags: u32, bits: u32, channels: u32) -> PhysicalFormat {
        let bytes = bits.div_ceil(8) * channels;
        PhysicalFormat {
            rate,
            min_rate: rate,
            max_rate: rate,
            linear_pcm: true,
            flags,
            bits,
            channels,
            bytes_per_packet: bytes,
            frames_per_packet: 1,
            bytes_per_frame: bytes,
        }
    }

    const FLOAT: u32 = 9; // float | packed, as the built-in microphone reports
    const INT: u32 = 12; // signed integer | packed

    #[test]
    fn the_built_in_microphone_with_one_float_format_per_rate_is_asked_for_float() {
        // given: what the MacBook Pro microphone lists
        let formats: Vec<_> = [96_000.0, 88_200.0, 48_000.0, 44_100.0].map(|r| format(r, FLOAT, 32, 1)).into();
        // when / then: cpal finds the one 48 kHz entry, the current format, and writes nothing
        assert_eq!(safe_request(&formats, 48_000, 1), Some(Request::F32));
    }

    #[test]
    fn a_device_with_float_and_integer_formats_at_its_rate_is_not_asked_for_float() {
        // given: an interface that may be running 24-bit integers
        let formats = [format(48_000.0, FLOAT, 32, 2), format(48_000.0, INT, 24, 2)];
        // when / then: a float request could switch it; no 32-bit integer entry exists
        assert_eq!(safe_request(&formats, 48_000, 2), Some(Request::I32));
    }

    #[test]
    fn an_integer_only_usb_microphone_is_asked_for_float_which_matches_nothing() {
        // given
        let formats = [format(48_000.0, INT, 16, 1), format(48_000.0, INT, 24, 1)];
        // when / then
        assert_eq!(safe_request(&formats, 48_000, 1), Some(Request::F32));
    }

    #[test]
    fn a_device_whose_every_type_could_be_switched_is_refused() {
        // given: float, 32-bit and 16-bit integer formats side by side at one rate
        let formats = [format(48_000.0, FLOAT, 32, 2), format(48_000.0, INT, 32, 2), format(48_000.0, INT, 16, 2)];
        // when / then
        assert_eq!(safe_request(&formats, 48_000, 2), None);
    }

    #[test]
    fn formats_at_other_rates_do_not_count() {
        // given: an integer format only at 96 kHz
        let formats = [format(48_000.0, FLOAT, 32, 1), format(96_000.0, INT, 24, 1)];
        // when / then
        assert_eq!(safe_request(&formats, 48_000, 1), Some(Request::F32));
    }

    #[test]
    fn a_ranged_entry_covering_the_rate_counts_like_coreaudio_rs_counts_it() {
        // given: one float entry for 44.1-96 kHz and an integer entry at 48 kHz
        let mut ranged = format(44_100.0, FLOAT, 32, 1);
        ranged.max_rate = 96_000.0;
        let formats = [ranged, format(48_000.0, INT, 24, 1)];
        // when / then
        assert_eq!(safe_request(&formats, 48_000, 1), Some(Request::I32));
    }

    #[test]
    fn entries_that_are_not_linear_pcm_or_claim_float_and_integer_at_once_match_nothing() {
        // given: a compressed float entry and a float-and-integer entry beside a 16-bit one
        let mut compressed = format(48_000.0, FLOAT, 32, 1);
        compressed.linear_pcm = false;
        let formats = [compressed, format(48_000.0, FLOAT | INT, 32, 1), format(48_000.0, INT, 16, 1)];
        // when / then: neither counts for a float or a 32-bit request, so cpal writes nothing
        assert!(!matches(&formats[0], Request::F32, 48_000, 1));
        assert!(!matches(&formats[1], Request::F32, 48_000, 1));
        assert!(!matches(&formats[1], Request::I32, 48_000, 1));
        assert_eq!(safe_request(&formats, 48_000, 1), Some(Request::F32));
    }

    #[test]
    fn each_request_is_named_as_cpal_prints_its_sample_format() {
        // given / when / then: the "Capturing ..." line names the type this way
        assert_eq!([Request::F32, Request::I32, Request::I16].map(Request::name), ["f32", "i32", "i16"]);
    }

    #[test]
    fn a_stereo_request_skips_mono_entries_like_coreaudio_rs() {
        // given: a mono float entry and a stereo float entry at the same rate
        let formats = [format(48_000.0, FLOAT, 32, 1), format(48_000.0, FLOAT, 32, 2)];
        // when / then: two different entries, so float is unsafe; no integer entries
        assert_eq!(safe_request(&formats, 48_000, 2), Some(Request::I32));
    }
}
