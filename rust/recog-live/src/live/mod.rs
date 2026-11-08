//! The main loop of the live process, in place of `recog/live.py`: the transcriber
//! ([`transcriber::Live`]), its session with the bridge in the same process
//! ([`session`]), the captures it builds ([`build`]), the devices it names
//! ([`devices`]) and the golden replay ([`replay`]).

pub mod build;
pub mod devices;
pub mod parts;
pub mod replay;
pub mod session;
pub mod text;
pub mod transcriber;

pub use parts::{Conference, LiveAsr, LiveCapture};
pub use transcriber::{Live, LiveEnv, LiveOptions, Out, RecentSegment, SourceState};

/// How often the main loop deletes meeting audio older than 24 h (seconds).
pub const AUDIO_CLEANUP_INTERVAL_S: f64 = 3600.0;

/// How often the main loop checks the system capture for digital silence (seconds).
pub const SILENCE_CHECK_INTERVAL_S: f64 = 5.0;

/// Golden sessions older than this many days are deleted when the next one starts.
pub const GOLDEN_KEEP_DAYS: f64 = 14.0;

/// `--mic-device none`: no microphone (unlike "the default input").
pub const NO_MIC: &str = "none";

/// The system-audio spec without a source.
pub const NO_SYSTEM_AUDIO: &str = "none";

/// The speaker of system audio when nobody else is in the call, or there is no call.
pub const SYSTEM_AUDIO_SPEAKER: &str = "Системный звук";
