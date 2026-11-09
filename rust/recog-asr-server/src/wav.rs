//! The WAV ffmpeg writes for the backend (`-ar 16000 -ac 1 -c:a pcm_s16le`), read as
//! float samples in [-1, 1].

/// The samples of a 16 kHz mono 16-bit PCM WAV file.
///
/// # Errors
/// A file that is not such a WAV.
pub fn read_pcm16_mono(raw: &[u8]) -> Result<Vec<f32>, String> {
    if raw.len() < 12 || &raw[..4] != b"RIFF" || &raw[8..12] != b"WAVE" {
        return Err("file does not start with RIFF id".to_owned());
    }
    let mut at = 12;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    while at + 8 <= raw.len() {
        let id = &raw[at..at + 4];
        let size = u32::from_le_bytes([raw[at + 4], raw[at + 5], raw[at + 6], raw[at + 7]]) as usize;
        let body = at + 8;
        match id {
            b"fmt " => {
                if size < 16 || body + 16 > raw.len() {
                    return Err("bad fmt chunk".to_owned());
                }
                let f = &raw[body..body + 16];
                let tag = u16::from_le_bytes([f[0], f[1]]);
                let channels = u16::from_le_bytes([f[2], f[3]]);
                let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
                let bits = u16::from_le_bytes([f[14], f[15]]);
                format = Some((tag, channels, rate, bits));
            }
            b"data" => {
                let Some((tag, channels, rate, bits)) = format else {
                    return Err("data chunk before fmt chunk".to_owned());
                };
                // 1 is PCM; 0xFFFE (extensible) is what ffmpeg writes for some layouts.
                if !matches!(tag, 1 | 0xFFFE) || channels != 1 || rate != 16_000 || bits != 16 {
                    return Err(format!(
                        "unexpected WAV format: tag {tag}, {channels} channel(s), {rate} Hz, {bits} bits"
                    ));
                }
                let end = body.saturating_add(size).min(raw.len());
                let (pairs, _) = raw[body..end].as_chunks::<2>();
                return Ok(pairs.iter().map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0).collect());
            }
            _ => {}
        }
        at = body.saturating_add(size).saturating_add(size % 2);
    }
    Err("no data chunk".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(samples: &[i16], extra_chunk: bool) -> Vec<u8> {
        let mut out = b"RIFF\0\0\0\0WAVE".to_vec();
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&16_000u32.to_le_bytes());
        out.extend_from_slice(&32_000u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        if extra_chunk {
            out.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        }
        out.extend_from_slice(b"data");
        out.extend_from_slice(&u32::try_from(samples.len() * 2).unwrap().to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn an_ffmpeg_wav_with_a_list_chunk_when_read_then_its_samples() {
        assert_eq!(read_pcm16_mono(&wav(&[0, 16384, -32768], true)).unwrap(), vec![0.0, 0.5, -1.0]);
    }

    #[test]
    fn not_a_wav_when_read_then_an_error() {
        assert!(read_pcm16_mono(b"hello").is_err());
    }

    /// A RIFF/WAVE file of these chunks, each padded to an even size.
    fn riff(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = b"RIFF\0\0\0\0WAVE".to_vec();
        for (id, body) in chunks {
            out.extend_from_slice(*id);
            out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
            out.extend_from_slice(body);
            if body.len() % 2 == 1 {
                out.push(0);
            }
        }
        out
    }

    fn fmt(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out
    }

    const SAMPLES: &[u8] = &[0x00, 0x40, 0x00, 0xc0];

    #[test]
    fn a_file_without_riff_or_wave_when_read_then_the_riff_error() {
        let riff_error = Err("file does not start with RIFF id".to_owned());
        assert_eq!(read_pcm16_mono(b"hello"), riff_error);
        assert_eq!(read_pcm16_mono(b"RIFF\0\0\0\0WAVX"), riff_error);
        assert_eq!(read_pcm16_mono(b"RIFX\0\0\0\0WAVE"), riff_error);
    }

    #[test]
    fn a_short_or_cut_fmt_chunk_when_read_then_bad_fmt() {
        let short = riff(&[(b"fmt ", &fmt(1, 1, 16_000, 16)[..14]), (b"data", SAMPLES)]);
        assert_eq!(read_pcm16_mono(&short), Err("bad fmt chunk".to_owned()));
        let mut cut = riff(&[(b"fmt ", &fmt(1, 1, 16_000, 16))]);
        cut.truncate(cut.len() - 1);
        assert_eq!(read_pcm16_mono(&cut), Err("bad fmt chunk".to_owned()));
    }

    #[test]
    fn data_before_fmt_or_no_data_when_read_then_the_errors() {
        let early = riff(&[(b"data", SAMPLES), (b"fmt ", &fmt(1, 1, 16_000, 16))]);
        assert_eq!(read_pcm16_mono(&early), Err("data chunk before fmt chunk".to_owned()));
        let none = riff(&[(b"fmt ", &fmt(1, 1, 16_000, 16)), (b"LIST", b"abc")]);
        assert_eq!(read_pcm16_mono(&none), Err("no data chunk".to_owned()));
        let mut cut_header = none.clone();
        cut_header.extend_from_slice(b"data\x04\0");
        assert_eq!(read_pcm16_mono(&cut_header), Err("no data chunk".to_owned()));
    }

    #[test]
    fn a_format_other_than_16_khz_mono_16_bit_pcm_when_read_then_it_is_named() {
        for ((tag, channels, rate, bits), message) in [
            ((3, 1, 16_000, 16), "tag 3, 1 channel(s), 16000 Hz, 16 bits"),
            ((1, 2, 16_000, 16), "tag 1, 2 channel(s), 16000 Hz, 16 bits"),
            ((1, 1, 44_100, 16), "tag 1, 1 channel(s), 44100 Hz, 16 bits"),
            ((1, 1, 16_000, 8), "tag 1, 1 channel(s), 16000 Hz, 8 bits"),
        ] {
            let raw = riff(&[(b"fmt ", &fmt(tag, channels, rate, bits)), (b"data", SAMPLES)]);
            assert_eq!(read_pcm16_mono(&raw), Err(format!("unexpected WAV format: {message}")));
        }
    }

    #[test]
    fn an_extensible_fmt_of_18_bytes_when_read_then_its_samples() {
        let mut extensible = fmt(0xFFFE, 1, 16_000, 16);
        extensible.extend_from_slice(&[0, 0]);
        let raw = riff(&[(b"fmt ", &extensible), (b"data", SAMPLES)]);
        assert_eq!(read_pcm16_mono(&raw), Ok(vec![0.5, -0.5]));
    }

    #[test]
    fn a_data_size_past_the_end_or_odd_when_read_then_the_whole_samples_there_are() {
        let mut raw = riff(&[(b"fmt ", &fmt(1, 1, 16_000, 16)), (b"data", &[0x00, 0x40, 0x00])]);
        raw.pop();
        let at = raw.len() - 7;
        raw[at..at + 4].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(read_pcm16_mono(&raw), Ok(vec![0.5]));
    }
}
