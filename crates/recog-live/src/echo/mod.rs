//! Echo cancellation on the microphone, in place of `recog/echo_cancel.py`: what the
//! speakers play leaves the transcript.
//!
//! The chain, its numbers and why each part is there:
//! docs/plans/2026-10-30-mic-echo-cancel-design.md. A high-pass and the inverse of
//! Speex's low cut, then Speex (the `speexdsp-aec` crate, called directly), then a
//! suppressor of what Speex leaves behind, and raw audio whenever there is no echo to
//! remove. Both streams are placed by their time stamps on the host uptime clock.
//!
//! Against the Python module (`tests/echo_parity.rs` runs both on the same streams):
//! - exact: the placement, the reference ring, the delay line and every raw sample,
//!   the high-pass, the per-block decisions (`active`, the echo path, the quiet frames,
//!   all from numpy's own pairwise sums), the noise the inverse filter is measured with;
//! - within rounding: the FFTs (realfft for numpy's pocketfft) and the inverse FIR's
//!   convolution (numpy's goes through BLAS), whose float64 results differ in the last
//!   bits. On the test streams that leaves under 0.03% of the cancelled samples one
//!   float32 ulp off Python's and none at the int16 Speex sees, so Speex adapts the same;
//!   an input that puts a sample on an int16 boundary could still feed Speex one LSB
//!   apart, after which the two filters drift apart by rounding;
//! - the measured inverse filter: realfft and Levinson's recursion for pocketfft and
//!   LAPACK's LU, the same taps to 2e-14.
//!
//! Inherited from Python as is: `ReferenceRing::write` of a block longer than the ring
//! keeps its last four seconds but writes them at the block's start position (the
//! Python code adds `len(samples) - n` after cutting `samples` to `n`).

pub mod api;
pub mod detector;
pub mod filters;
pub mod inverse;
pub mod numpy;
pub mod placer;
pub mod random;
pub mod stage;
pub mod suppressor;
#[rustfmt::skip]
mod ziggurat_tables;

pub use api::EchoSink;
pub use detector::{EchoPathDetector, peak_correlation};
pub use inverse::{inverse_low_cut_fir, shared_inverse_fir};
pub use placer::{ReferenceRing, TimelinePlacer};
pub use stage::EchoStage;

/// The sample rate of both streams.
pub const SR: u32 = 16_000;
/// Speex's frame.
pub const FRAME: usize = 160;
/// The suppressor's hop.
pub const HOP: usize = 256;
/// A whole number of both: decisions are made per block.
pub const BLOCK: usize = 1280;

/// The reference leads the microphone by about 50 ms by their stamps. Speex's usable
/// range is about 5-250 ms, so the microphone is paired 75 ms later to sit in the
/// middle of it, which also covers the batches the system audio arrives in.
pub const REFERENCE_LEAD: i64 = 1200;
/// The suppressor's reference offset follows the lead.
pub const SUPPRESSOR_REFERENCE_DELAY: usize = 1950;
/// Speex's filter length.
pub const TAIL: usize = 4096;
pub const HIGH_PASS_HZ: f64 = 60.0;
/// How much microphone to hold for a late reference before going on without it.
pub const REFERENCE_WAIT: usize = 4800;

/// Raw audio while the reference has been this quiet for this long: Speex thins a
/// voice even when it has nothing to cancel.
pub const QUIET_RMS: f64 = 0.002;
pub const QUIET_HOLD_S: f64 = 0.3;

/// Echo path: peak correlation of the microphone and the reference.
pub const PATH_WINDOW_S: f64 = 3.0;
pub const PATH_HOP_S: f64 = 0.5;
pub const PATH_MAX_LEAD_S: f64 = 0.3;
pub const PATH_BAND_HZ: (f64, f64) = (200.0, 3500.0);
pub const PATH_ENTER: f64 = 0.2;
pub const PATH_LEAVE: f64 = 0.13;
pub const PATH_LEAVE_WINDOWS: usize = 10;
