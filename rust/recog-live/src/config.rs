//! The constants of `recog/config.py` the live modules use, with the same values.

/// The sample rate of every capture, the PCM transport and ASR.
pub const SAMPLE_RATE: u32 = 16_000;

/// RMS below which a chunk counts as silence.
pub const VAD_RMS_THRESHOLD: f64 = 0.005;

/// A pause longer than this (seconds) starts a new speaker turn; the refine gap trigger.
pub const SPEAKER_TURN_GAP: f64 = 3.0;

/// The worklet's chunk stride (seconds).
pub const PCM_CHUNK_DURATION_S: f64 = 3.0;

/// The worklet's chunk payload: stride plus the 600 ms overlap (seconds).
pub const PCM_CHUNK_PAYLOAD_S: f64 = 3.6;

/// The per-speaker batch window (ms).
pub const PCM_BATCH_INTERVAL_MS: u64 = 200;

/// The largest per-speaker batch.
pub const PCM_MAX_BATCH_SIZE: usize = 8;

/// The overlap of consecutive worklet chunks (ms).
pub const PCM_CHUNK_OVERLAP_MS: u64 = 600;

/// Without PCM for this long during a meeting the ingest is degraded (seconds).
pub const PCM_SILENCE_WATCHDOG_S: f64 = 30.0;

/// The trailing refine window (seconds).
pub const PCM_REFINE_WINDOW_S: f64 = 20.0;

/// Continuous speech without a gap that still triggers a refine (seconds).
pub const PCM_REFINE_MONOLOG_CAP_S: f64 = 15.0;

/// While a capture buffer holds no more than this (seconds), its start time is
/// re-anchored to the wall clock on every block.
pub const CAPTURE_RESYNC_MAX_S: f64 = 6.0;

/// Reuse the last named speaker of system audio only within this many seconds.
pub const LAST_SPEAKER_TTL: f64 = 10.0;

/// Without recognised text for this long (seconds), an orphan meeting is split.
pub const SILENCE_SPLIT_TIMEOUT: f64 = 10.0 * 60.0;

/// Retroactive correction scans at most this many recent system-audio segments.
pub const RETROACTIVE_MAX_SEGMENTS: usize = 20;

/// Retroactive correction reaches back at most this many seconds.
pub const RETROACTIVE_MAX_SECONDS: f64 = 60.0;

/// A segment of one speaker breaks after this many seconds of continuous speech.
pub const MAX_SEGMENT_DURATION: f64 = 20.0;

/// The wall-clock↔WAV anchor of PCM offsets is used only while younger than this (ms).
pub const PCM_WAV_ANCHOR_MAX_AGE_MS: f64 = 30_000.0;

/// `MIC_SPEAKER_NAME`'s default: the microphone's name without a profile.
pub const DEFAULT_MIC_SPEAKER_NAME: &str = "Me";
