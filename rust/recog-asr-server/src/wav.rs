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
}
