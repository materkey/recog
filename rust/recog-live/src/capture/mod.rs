//! Audio capture, in place of `recog/audio_capture.py`, `recog/system_audio.py` and
//! `recog/mic_capture.py`.
//!
//! - [`buffer::BufferedAudioCapture`]: the buffer, its capture times and overlap.
//! - [`subprocess::SubprocessCapture`]: a child's stdout into the buffer, with the
//!   restart policy ([`subprocess::RestartPolicy`]) and a pluggable decoder.
//! - [`pulse::PulseAudioCapture`]: `parec` following the default sink (Linux).
//! - [`sck::find_or_build_sck_utility`]: the ScreenCaptureKit helper (macOS).
//! - [`framed`]: the microphone worker's and the reference's frames, through the echo
//!   stage.
//! - [`backend`]: choosing and building the system capture.
//!
//! Every part builds and is tested on Linux and macOS alike: only running the helpers
//! needs their system.

pub mod backend;
pub mod buffer;
pub mod child;
pub mod framed;
pub mod pulse;
pub mod sck;
pub mod subprocess;

#[cfg(test)]
mod tests;

pub use backend::{SystemCapture, create_system_capture, resolve_system_audio_backend};
pub use buffer::BufferedAudioCapture;
pub use framed::{
    MicDecoder, ReferenceDecoder, child_mic_capture, framed_system_capture, mic_worker_argv, split_frames,
};
pub use pulse::{PulseAudioCapture, parec_argv, resolve_pulse_monitor_source};
pub use subprocess::{RestartPolicy, SubprocessCapture};
