//! ASR as the live pipeline calls it: the shapes of `ASRModule`/`WavoASR`'s answers
//! and the errors the callers tell apart (`ASRTemporarilyUnavailable`, `RefineAborted`).

use std::fmt;

/// A word with its `start`/`end` in ms from the clip's start, as
/// `transcribe_with_timestamps` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub start: i64,
    pub end: i64,
}

/// A clip's text and its words.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Transcription {
    pub text: String,
    pub words: Vec<Word>,
}

/// A clip as the callers hold it: float samples in [-1, 1] or Int16 PCM.
#[derive(Debug, Clone, Copy)]
pub enum Clip<'a> {
    F32(&'a [f32]),
    I16(&'a [i16]),
}

impl Clip<'_> {
    /// The samples as float32, Int16 divided by 32768 as `_pcm_bytes` does.
    #[must_use]
    pub fn to_f32(&self) -> Vec<f32> {
        match self {
            Clip::F32(s) => s.to_vec(),
            Clip::I16(s) => s.iter().map(|&v| f32::from(v) / 32768.0).collect(),
        }
    }

    /// The number of samples.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Clip::F32(s) => s.len(),
            Clip::I16(s) => s.len(),
        }
    }

    /// Whether the clip has no samples.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Why an ASR call gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrError {
    /// `ASRTemporarilyUnavailable`: no backend can serve now; the caller keeps its
    /// audio and retries.
    TemporarilyUnavailable,
    /// `RefineAborted`: a refine gave way to live before decoding.
    Aborted,
    /// `ValueError` of the input (wrong sample rate, not mono).
    InvalidInput(String),
    /// Any other failure, with its message.
    Failed(String),
}

impl fmt::Display for AsrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AsrError::TemporarilyUnavailable => f.write_str("ASR is temporarily unavailable"),
            AsrError::Aborted => f.write_str("refine aborted"),
            AsrError::InvalidInput(m) | AsrError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for AsrError {}
