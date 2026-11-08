//! The meeting WAV writer — the port of `IncrementalWavWriter` in `recog/live.py`.
//!
//! 16-bit PCM written chunk by chunk as capture delivers it, skipping the overlap the
//! capture repeats at the start of every chunk but the first. The header's sizes are
//! re-patched after every chunk, so a killed process never leaves a header claiming
//! 0 data bytes, and the data stops at a cap (2 GiB) before the format's 32-bit sizes
//! would overflow.
//!
//! Samples convert as NumPy's `(chunk * 32767).astype(np.int16)` does: the product in
//! float32, truncated toward zero to 32 bits, then wrapped to 16 bits, so out-of-range
//! input wraps instead of clipping. NaN, infinities and values beyond the 32-bit range
//! are undefined in C and follow the CPU: saturating on arm64, `i32::MIN` on x86-64
//! (`to_int16`). The parity test checks this against NumPy on arm64 and x86-64, NaN
//! and infinities included.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

/// WAV stores RIFF/data sizes as 32-bit values; 2 GiB is safe for signed parsers too
/// (~18.6 h of 16 kHz mono int16).
pub const MAX_WAV_DATA_BYTES: u64 = 1 << 31;

/// `IncrementalWavWriter(path, sample_rate, channels, overlap_seconds, max_data_bytes)`.
#[derive(Debug)]
pub struct IncrementalWavWriter {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    file: Option<File>,
    samples_written: u64,
    overlap_samples: usize,
    first_chunk: bool,
    max_data_bytes: u64,
    cap_warned: bool,
}

impl IncrementalWavWriter {
    /// A writer with recog's settings: 16 kHz, mono, 0.5 s overlap, the 2 GiB cap.
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self::with_settings(path, 16000, 1, 0.5, MAX_WAV_DATA_BYTES)
    }

    #[must_use]
    pub fn with_settings(
        path: &Path,
        sample_rate: u32,
        channels: u16,
        overlap_seconds: f64,
        max_data_bytes: u64,
    ) -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let overlap_samples = (overlap_seconds * f64::from(sample_rate)) as usize;
        Self {
            path: path.to_path_buf(),
            sample_rate,
            channels,
            file: None,
            samples_written: 0,
            overlap_samples,
            first_chunk: true,
            max_data_bytes,
            cap_warned: false,
        }
    }

    /// `open(continuation)`: append to an existing WAV (its sample count restored
    /// from the file size) or start a new one with a placeholder header. With
    /// `continuation` the capture is still running (a meeting change), so the first
    /// chunk's overlap is skipped too.
    ///
    /// # Errors
    /// What creating or opening the file fails with.
    pub fn open(&mut self, continuation: bool) -> io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let existing = fs::metadata(&self.path).ok().map(|m| m.len());
        if let Some(size) = existing.filter(|&size| size >= 44) {
            self.samples_written = (size - 44) / 2;
            let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
            file.seek(SeekFrom::End(0))?;
            self.file = Some(file);
        } else {
            let mut file = File::create(&self.path)?;
            file.write_all(&self.header(0))?;
            self.file = Some(file);
        }
        self.first_chunk = !continuation;
        Ok(())
    }

    /// `write_chunk(chunk)`: append float samples in [-1, 1], skipping the overlap
    /// of every chunk but the first. Returns the samples skipped at its head, or None
    /// when nothing was written (not open, nothing left after the cut, the cap).
    ///
    /// # Errors
    /// A failed write.
    pub fn write_chunk(&mut self, chunk: &[f32]) -> io::Result<Option<usize>> {
        if self.file.is_none() {
            return Ok(None);
        }
        let (skipped, samples) = if self.first_chunk {
            self.first_chunk = false;
            (0, chunk)
        } else {
            (self.overlap_samples, chunk.get(self.overlap_samples..).unwrap_or(&[]))
        };
        if samples.is_empty() {
            return Ok(None);
        }
        if (self.samples_written + samples.len() as u64) * 2 > self.max_data_bytes {
            if !self.cap_warned {
                self.cap_warned = true;
                let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                eprintln!(
                    "Warning: {name} reached max WAV size ({} bytes), stopping audio recording",
                    self.max_data_bytes
                );
            }
            return Ok(None);
        }
        let bytes: Vec<u8> = samples.iter().flat_map(|&x| to_int16(x).to_le_bytes()).collect();
        let file = self.file.as_mut().unwrap_or_else(|| unreachable!("checked above"));
        file.write_all(&bytes)?;
        self.samples_written += samples.len() as u64;
        self.patch_header_sizes()?;
        Ok(Some(skipped))
    }

    /// `mark_discontinuity()`: the capture stream broke, so the next chunk's head is
    /// new audio and is written whole.
    pub fn mark_discontinuity(&mut self) {
        self.first_chunk = true;
    }

    fn data_size(&self) -> u32 {
        u32::try_from(self.samples_written * 2).unwrap_or(u32::MAX)
    }

    fn patch_header_sizes(&mut self) -> io::Result<()> {
        let data_size = self.data_size();
        let Some(file) = self.file.as_mut() else { return Ok(()) };
        let pos = file.stream_position()?;
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&(36u32.wrapping_add(data_size)).to_le_bytes())?;
        file.seek(SeekFrom::Start(40))?;
        file.write_all(&data_size.to_le_bytes())?;
        file.seek(SeekFrom::Start(pos))?;
        Ok(())
    }

    /// `get_offset_ms()`: the offset of the next sample, from the samples written.
    #[must_use]
    pub fn offset_ms(&self) -> i64 {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let ms = (self.samples_written as f64 / f64::from(self.sample_rate) * 1000.0) as i64;
        ms
    }

    /// `finalize()`: the final header, and the file closed.
    ///
    /// # Errors
    /// A failed write.
    pub fn finalize(&mut self) -> io::Result<()> {
        let header = self.header(self.data_size());
        let Some(mut file) = self.file.take() else { return Ok(()) };
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        Ok(())
    }

    fn header(&self, data_size: u32) -> Vec<u8> {
        let byte_rate = self.sample_rate * u32::from(self.channels) * 2;
        let block_align = self.channels * 2;
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&36u32.wrapping_add(data_size).to_le_bytes());
        h.extend_from_slice(b"WAVE");
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&self.channels.to_le_bytes());
        h.extend_from_slice(&self.sample_rate.to_le_bytes());
        h.extend_from_slice(&byte_rate.to_le_bytes());
        h.extend_from_slice(&block_align.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&data_size.to_le_bytes());
        h
    }
}

/// `np.float32(x * 32767).astype(np.int16)` as the C cast does it on arm64/x86-64:
/// through a 32-bit integer, whose low half is kept. A value outside the 32-bit range
/// saturates on arm64 (NaN to 0), as Rust's `as` does, but becomes `i32::MIN`
/// ("integer indefinite") on x86-64, so infinity writes 0 there and -1 on a Mac.
#[must_use]
pub fn to_int16(x: f32) -> i16 {
    let scaled = x * 32767.0;
    #[allow(clippy::cast_possible_truncation)]
    let wide = if cfg!(target_arch = "x86_64") && !(-2_147_483_648.0..2_147_483_648.0).contains(&scaled) {
        i32::MIN
    } else {
        scaled as i32
    };
    #[allow(clippy::cast_possible_truncation)]
    let narrow = wide as i16;
    narrow
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(path: &Path) -> Vec<u8> {
        fs::read(path).unwrap()
    }

    #[test]
    fn first_chunk_whole_then_overlap_skipped_and_header_valid_without_finalize() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let mut writer = IncrementalWavWriter::with_settings(&path, 10, 1, 0.5, MAX_WAV_DATA_BYTES);
        writer.open(false).unwrap();
        // when
        let first = writer.write_chunk(&[0.5; 10]).unwrap();
        let second = writer.write_chunk(&[-1.0; 10]).unwrap();
        // then
        assert_eq!((first, second), (Some(0), Some(5)));
        let bytes = data(&path);
        assert_eq!(bytes.len(), 44 + 30);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 30);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 30);
        assert_eq!(i16::from_le_bytes([bytes[44], bytes[45]]), 16383);
        assert_eq!(i16::from_le_bytes([bytes[72], bytes[73]]), -32767);
        assert_eq!(writer.offset_ms(), 1500);
    }

    #[test]
    fn write_stops_at_max_data_bytes_and_reports_nothing_written() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let mut writer = IncrementalWavWriter::with_settings(&path, 16000, 1, 0.0, 100);
        writer.open(false).unwrap();
        // when / then
        assert_eq!(writer.write_chunk(&[0.1; 40]).unwrap(), Some(0));
        assert_eq!(writer.write_chunk(&[0.1; 20]).unwrap(), None);
        assert_eq!(data(&path).len(), 44 + 80);
    }

    #[test]
    fn short_chunk_unopened_writer_and_reopen_continue_like_python() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let mut unopened = IncrementalWavWriter::with_settings(&path, 10, 1, 0.5, MAX_WAV_DATA_BYTES);
        assert_eq!(unopened.write_chunk(&[0.1; 10]).unwrap(), None);
        let mut writer = IncrementalWavWriter::with_settings(&path, 10, 1, 0.5, MAX_WAV_DATA_BYTES);
        writer.open(false).unwrap();
        writer.write_chunk(&[0.1; 10]).unwrap();
        assert_eq!(writer.write_chunk(&[0.1; 3]).unwrap(), None);
        writer.finalize().unwrap();
        // when
        let mut reopened = IncrementalWavWriter::with_settings(&path, 10, 1, 0.5, MAX_WAV_DATA_BYTES);
        reopened.open(true).unwrap();
        // then
        assert_eq!(reopened.offset_ms(), 1000);
        assert_eq!(reopened.write_chunk(&[0.1; 10]).unwrap(), Some(5));
    }

    #[test]
    fn out_of_range_samples_wrap_like_numpy() {
        // given / when / then
        assert_eq!(to_int16(1.0), 32767);
        assert_eq!(to_int16(-1.0), -32767);
        assert_eq!(to_int16(1.5), -16386);
        assert_eq!(to_int16(f32::NAN), 0);
        assert_eq!(to_int16(0.99999), 32766);
    }

    #[test]
    fn infinite_samples_written_then_low_half_of_the_platform_cast() {
        // given / when / then
        let expected = if cfg!(target_arch = "x86_64") { (0, 0) } else { (-1, 0) };
        assert_eq!((to_int16(f32::INFINITY), to_int16(f32::NEG_INFINITY)), expected);
        assert_eq!(to_int16(1e6), if cfg!(target_arch = "x86_64") { 0 } else { -1 });
    }
}
