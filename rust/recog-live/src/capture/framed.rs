//! Captures that read framed audio from a child process, in place of
//! `recog/mic_capture.py`: the microphone worker (`recog/mic_worker.py` or
//! `rust/recog-mic`) and, with echo cancellation on, the system audio with its time
//! stamps as the stage's reference (`sck_audio_capture --frames`).
//!
//! Both are a [`SubprocessCapture`] with their own [`Decoder`]; restart, loss and
//! stderr handling are the system capture's, and a respawn keeps the buffer (the
//! overlap the WAV writer relies on is still in it).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::capture::subprocess::{BYTES_PER_SAMPLE, CaptureBuilder, DecodeContext, Decoder, SubprocessCapture};
use crate::echo::EchoSink;
use crate::log::Logger;

/// Below this a gap in ADC time is the clock's own jitter (seconds).
pub const ADC_HOLE_REPORT_S: f64 = 0.005;
/// Exact zeros for this long are reported: a refused microphone or a closed lid.
pub const ONLY_ZEROS_REPORT_S: f64 = 30.0;

/// The header of a pipe's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameHeader {
    /// `<ddI`: ADC time, wall clock, frame count (the microphone worker).
    Mic,
    /// `<ddIB`: presentation time, wall clock, frame count, whether the default
    /// output can make an echo (`sck_audio_capture --frames`).
    Reference,
}

impl FrameHeader {
    /// `struct.Struct(...).size`.
    #[must_use]
    pub fn size(self) -> usize {
        match self {
            FrameHeader::Mic => 20,
            FrameHeader::Reference => 21,
        }
    }
}

/// One frame: its two times, its count, the reference's echo flag (0 for the
/// microphone) and its samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub time: f64,
    pub wall: f64,
    pub count: u32,
    pub echo_possible: u8,
    pub samples: Vec<f32>,
}

fn f64_at(data: &[u8], at: usize) -> f64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[at..at + 8]);
    f64::from_le_bytes(b)
}

/// `split_frames`: the whole frames of `data` and what is left over.
#[must_use]
pub fn split_frames(data: &[u8], header: FrameHeader) -> (Vec<Frame>, Vec<u8>) {
    let size = header.size();
    let mut frames = Vec::new();
    let mut offset = 0;
    while data.len() - offset >= size {
        let mut c = [0u8; 4];
        c.copy_from_slice(&data[offset + 16..offset + 20]);
        let count = u32::from_le_bytes(c);
        let end = offset + size + count as usize * BYTES_PER_SAMPLE;
        if end > data.len() {
            break;
        }
        let samples = data[offset + size..end].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        frames.push(Frame {
            time: f64_at(data, offset),
            wall: f64_at(data, offset + 8),
            count,
            echo_possible: if header == FrameHeader::Reference { data[offset + 20] } else { 0 },
            samples,
        });
        offset = end;
    }
    (frames, data[offset..].to_vec())
}

/// One frame as the microphone worker writes it (`encode_frame`).
#[must_use]
pub fn encode_mic_frame(adc_time: f64, wall_time: f64, samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + samples.len() * 4);
    out.extend_from_slice(&adc_time.to_le_bytes());
    out.extend_from_slice(&wall_time.to_le_bytes());
    out.extend_from_slice(&u32::try_from(samples.len()).unwrap_or(u32::MAX).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// One frame as `sck_audio_capture --frames` writes it.
#[must_use]
pub fn encode_reference_frame(presentation_time: f64, wall_time: f64, echo_possible: bool, samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(21 + samples.len() * 4);
    out.extend_from_slice(&presentation_time.to_le_bytes());
    out.extend_from_slice(&wall_time.to_le_bytes());
    out.extend_from_slice(&u32::try_from(samples.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.push(u8::from(echo_possible));
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The Rust worker under `repo_root` (`RUST_MIC_WORKER`): the installed one in
/// `bin/` when there is one, else the release build.
#[must_use]
pub fn rust_mic_worker(repo_root: &Path) -> PathBuf {
    recog_core::install::program(repo_root, "recog-mic")
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `mic_worker_argv`: the worker `[live] mic_worker` names, Python's (`python -I
/// recog/mic_worker.py`, `python` standing for `sys.executable`) or the Rust binary
/// when it is built; the Rust choice without a build falls back to Python and says so.
///
/// # Errors
/// The setting is neither "python" nor "rust", in Python's `ValueError` words.
pub fn mic_worker_argv(
    choice: &str,
    repo_root: &Path,
    python: &Path,
    device_name: Option<&str>,
    sample_rate: u32,
    log: &Logger,
) -> Result<Vec<String>, String> {
    if choice != "python" && choice != "rust" {
        return Err(format!(r#"[live] mic_worker must be "python" or "rust", not {}"#, recog_core::py::repr(choice)));
    }
    let binary = rust_mic_worker(repo_root);
    let mut argv = if choice == "rust" && is_executable(&binary) {
        vec![binary.display().to_string(), "--sample-rate".into(), sample_rate.to_string()]
    } else {
        if choice == "rust" {
            log.line(&format!(
                "[mic] {} is not built (./setup.sh builds it when cargo is installed): the microphone runs in the Python worker",
                binary.display()
            ));
        }
        let worker = repo_root.join("recog").join("mic_worker.py");
        vec![
            python.display().to_string(),
            "-I".into(),
            worker.display().to_string(),
            "--sample-rate".into(),
            sample_rate.to_string(),
        ]
    };
    if let Some(name) = device_name {
        argv.push("--device".into());
        argv.push(name.to_owned());
    }
    Ok(argv)
}

#[allow(clippy::cast_precision_loss)]
fn seconds(samples: i64, sample_rate: u32) -> f64 {
    samples as f64 / f64::from(sample_rate)
}

/// The microphone worker's frames through the echo stage (`ChildMicCapture._decode_read`).
pub struct MicDecoder {
    frame_tail: Vec<u8>,
    next_adc_time: Option<f64>,
    zeros_reported: bool,
    echo_stage: Option<Arc<dyn EchoSink>>,
    stage_held: i64,
    stage_active: bool,
}

impl MicDecoder {
    #[must_use]
    pub fn new(echo_stage: Option<Arc<dyn EchoSink>>) -> Self {
        Self {
            frame_tail: Vec::new(),
            next_adc_time: None,
            zeros_reported: false,
            echo_stage,
            stage_held: 0,
            stage_active: false,
        }
    }

    /// Whether the echo stage is still in use.
    #[must_use]
    pub fn has_stage(&self) -> bool {
        self.echo_stage.is_some()
    }

    fn note_adc_time(&mut self, adc_time: f64, frames: u32, ctx: &DecodeContext<'_>) {
        if let Some(next) = self.next_adc_time
            && adc_time - next >= ADC_HOLE_REPORT_S
        {
            ctx.log.rate_limited(
                "mic-adc-hole",
                &format!("[{}] {:.0} ms hole in the captured audio", ctx.name, (adc_time - next) * 1000.0),
            );
        }
        self.next_adc_time = Some(adc_time + seconds(i64::from(frames), ctx.sample_rate));
    }

    fn note_zeros(&mut self, ctx: &DecodeContext<'_>) {
        if ctx.silence_s < ONLY_ZEROS_REPORT_S {
            self.zeros_reported = false;
        } else if !self.zeros_reported {
            self.zeros_reported = true;
            ctx.log.line(&format!(
                "[{}] only exact zeros from the microphone for {ONLY_ZEROS_REPORT_S:.0}s: is the lid closed, or its permission not granted to Recog?",
                ctx.name
            ));
        }
    }

    fn through_stage(
        &mut self,
        samples: Vec<f32>,
        adc_time: f64,
        wall_time: f64,
        ctx: &DecodeContext<'_>,
    ) -> (Vec<f32>, f64) {
        let Some(stage) = self.echo_stage.clone() else { return (samples, wall_time) };
        match stage.add_mic(&samples, adc_time) {
            Err(e) => {
                ctx.log.line(&format!("[{}] Echo cancellation failed, the microphone stays raw: {e}", ctx.name));
                self.echo_stage = None;
                let held = std::mem::take(&mut self.stage_held);
                let mut out = stage.flush_raw();
                out.extend_from_slice(&samples);
                (out, wall_time - seconds(held, ctx.sample_rate))
            }
            Ok(output) => {
                #[allow(clippy::cast_possible_wrap)]
                let (n_in, n_out) = (samples.len() as i64, output.len() as i64);
                self.stage_held += n_in - n_out;
                let active = stage.active();
                if active != self.stage_active {
                    self.stage_active = active;
                    let what = if active { "Echo cancelling" } else { "Back to the raw microphone" };
                    ctx.log.line(&format!("[{}] {what}", ctx.name));
                }
                (output, wall_time + seconds(n_in - n_out - self.stage_held, ctx.sample_rate))
            }
        }
    }
}

impl Decoder for MicDecoder {
    fn decode(&mut self, data: &[u8], ctx: &DecodeContext<'_>) -> Vec<(Vec<f32>, f64)> {
        let mut joined = std::mem::take(&mut self.frame_tail);
        joined.extend_from_slice(data);
        let (frames, tail) = split_frames(&joined, FrameHeader::Mic);
        self.frame_tail = tail;
        let mut parts: Vec<f32> = Vec::new();
        let mut any = false;
        let mut captured_at = None;
        for frame in frames {
            self.note_adc_time(frame.time, frame.count, ctx);
            let (output, output_at) = self.through_stage(frame.samples, frame.time, frame.wall, ctx);
            if !output.is_empty() {
                parts.extend_from_slice(&output);
                any = true;
                captured_at.get_or_insert(output_at);
            }
        }
        self.note_zeros(ctx);
        match captured_at {
            Some(at) if any => vec![(parts, at)],
            _ => Vec::new(),
        }
    }

    fn on_new_child(&mut self) {
        self.frame_tail.clear();
        self.next_adc_time = None;
    }
}

/// The system audio's frames, handed to the echo stage on the way
/// (`FramedSystemCapture._decode_read`).
pub struct ReferenceDecoder {
    frame_tail: Vec<u8>,
    echo_stage: Option<Arc<dyn EchoSink>>,
}

impl ReferenceDecoder {
    #[must_use]
    pub fn new(echo_stage: Option<Arc<dyn EchoSink>>) -> Self {
        Self { frame_tail: Vec::new(), echo_stage }
    }
}

impl Decoder for ReferenceDecoder {
    fn decode(&mut self, data: &[u8], ctx: &DecodeContext<'_>) -> Vec<(Vec<f32>, f64)> {
        let mut joined = std::mem::take(&mut self.frame_tail);
        joined.extend_from_slice(data);
        let (frames, tail) = split_frames(&joined, FrameHeader::Reference);
        self.frame_tail = tail;
        if frames.is_empty() {
            return Vec::new();
        }
        let captured_at = frames[0].wall;
        let mut parts = Vec::new();
        for frame in frames {
            parts.extend_from_slice(&frame.samples);
            if let Some(stage) = &self.echo_stage
                && let Err(e) = stage.add_reference(&frame.samples, frame.time, frame.echo_possible != 0)
            {
                // Without new reference the stage leaves the microphone raw by itself.
                ctx.log.line(&format!("[{}] Echo cancellation lost its reference: {e}", ctx.name));
                self.echo_stage = None;
            }
        }
        vec![(parts, captured_at)]
    }

    fn on_new_child(&mut self) {
        self.frame_tail.clear();
    }
}

/// `ChildMicCapture`: the microphone worker `argv`, named `mic`.
#[must_use]
pub fn child_mic_capture(argv: Vec<String>, echo_stage: Option<Arc<dyn EchoSink>>) -> CaptureBuilder {
    SubprocessCapture::builder(argv, "mic").decoder(Box::new(MicDecoder::new(echo_stage)))
}

/// `FramedSystemCapture`: `sck_audio_capture --frames`, named `screencapture`.
#[must_use]
pub fn framed_system_capture(argv: Vec<String>, echo_stage: Arc<dyn EchoSink>) -> CaptureBuilder {
    SubprocessCapture::builder(argv, "screencapture").decoder(Box::new(ReferenceDecoder::new(Some(echo_stage))))
}
