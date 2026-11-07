//! Errors of the public API.

use std::fmt;

/// Why a configuration or a call was refused. speexdsp itself accepts all of these
/// and then divides by zero, aborts the process, overflows its 16-bit constants or
/// reads past the caller's buffers.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The frame size cannot be processed.
    InvalidFrameSize {
        /// The size asked for.
        frame_size: usize,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The filter length cannot be processed.
    InvalidFilterLength {
        /// The length asked for.
        filter_length: usize,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The fixed-point constants cannot represent this rate for this frame size.
    UnsupportedSamplingRate {
        /// The rate asked for.
        sampling_rate: u32,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A buffer passed to [`EchoCanceller::cancel`](crate::EchoCanceller::cancel) is
    /// not one frame long.
    FrameLength {
        /// The frame size.
        expected: usize,
        /// Length of the microphone buffer.
        mic: usize,
        /// Length of the far-end buffer.
        far_end: usize,
        /// Length of the output buffer.
        out: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFrameSize { frame_size, reason } => write!(f, "frame size {frame_size} {reason}"),
            Self::InvalidFilterLength { filter_length, reason } => write!(f, "filter length {filter_length} {reason}"),
            Self::UnsupportedSamplingRate { sampling_rate, reason } => {
                write!(f, "sampling rate {sampling_rate} Hz {reason}")
            }
            Self::FrameLength { expected, mic, far_end, out } => write!(
                f,
                "every buffer must be one frame of {expected} samples (microphone {mic}, far end {far_end}, output {out})"
            ),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_value_and_the_reason() {
        let cases = [
            (Error::InvalidFrameSize { frame_size: 7, reason: "is odd" }, "frame size 7 is odd"),
            (Error::InvalidFilterLength { filter_length: 0, reason: "is zero" }, "filter length 0 is zero"),
            (Error::UnsupportedSamplingRate { sampling_rate: 48_000, reason: "is too high" }, "sampling rate 48000 Hz is too high"),
            (
                Error::FrameLength { expected: 160, mic: 80, far_end: 160, out: 160 },
                "every buffer must be one frame of 160 samples (microphone 80, far end 160, output 160)",
            ),
        ];
        for (error, message) in cases {
            assert_eq!(error.to_string(), message);
        }
    }
}
