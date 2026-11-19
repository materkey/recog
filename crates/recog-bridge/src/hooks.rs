//! What the bridge needs from the rest of recog, as traits: the boundary toward the
//! live process (`recog-live`, not ported yet), the meeting storage, the AI settings,
//! the increment generator and the golden recorder.
//!
//! In Python these are the callbacks `LiveTranscriber` registers on `KTalkBridge`
//! (`recog/live.py`, `_setup_ktalk_bridge` and `_setup_per_speaker_ingest`):
//!
//! | Python | here |
//! |---|---|
//! | `set_speaker_callback` | [`LiveEvents::speaker_changed`] |
//! | `set_segment_callback` | [`LiveEvents::segment_completed`] |
//! | `set_pre_meeting_switch_callback` | [`LiveEvents::before_meeting_switch`] |
//! | `set_meeting_change_callback` | [`LiveEvents::meeting_changed`] |
//! | `set_pcm_chunk_callback` | [`LiveEvents::pcm_chunk`] |
//! | `set_pcm_stats_getter` | [`LiveEvents::pcm_stats`] |
//! | `set_meeting_capture_callback(cb, active_getter=)` | [`MeetingCapture`] |
//! | `configure_user_profile(identity, read, apply)` | [`IdentitySource`] |
//! | `set_meeting_storage` | [`MeetingStore`] (implemented for `recog_core`'s storage) |
//! | `set_increment_generator` | [`IncrementGenerator`] (`crate::ai::AiIncrements` runs `recog-ai`'s) |
//! | `ai_settings_store=` | [`PolicySource`] (`crate::ai::AiSettingsPolicy` reads `recog-ai`'s store) |
//! | `bridge.golden = GoldenRecorder` | [`GoldenTap`] |
//!
//! Everything the live process calls the other way (`send_transcript`, `send_refine`,
//! the speaker queries, `is_conference_active`) is a method of [`crate::Bridge`].
//!
//! The callbacks run on the bridge's own thread, synchronously, with no bridge lock
//! held, as Python calls them from its event loop; an implementation may call back into
//! the bridge's queries.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use recog_core::meeting_storage::MeetingStorage;
use recog_core::pyjson::{Map, Value};

/// A boxed `Send` future, the return of the async hooks.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A finished stretch of one speaker (`SpeakerSegment`).
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerSegment {
    pub speaker_name: String,
    pub speaker_id: String,
    /// Unix seconds.
    pub start_time: f64,
    /// Unix seconds.
    pub end_time: f64,
    pub duration_ms: i64,
    pub is_local: bool,
}

/// One speaker change as kept in the history (`SpeakerEvent`).
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerEvent {
    /// None: nobody speaks.
    pub speaker_name: Option<String>,
    pub speaker_id: String,
    /// Unix seconds.
    pub timestamp: f64,
    pub is_local: bool,
}

/// A conference participant (`Participant`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub is_local: bool,
    pub mic_muted: bool,
}

/// Int16 PCM of one remote participant (`PcmChunk`, the `pcm_chunk` frame), its
/// times already moved onto the bridge's clock by the clock offset.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmChunk {
    pub pid: String,
    pub speaker_name: String,
    /// Always false: PCM comes only for remote participants (D9).
    pub is_local: bool,
    pub seq: i128,
    /// Unix seconds.
    pub chunk_start_ts: f64,
    /// Unix seconds.
    pub chunk_end_ts: f64,
    pub sample_rate: i128,
    /// Int16LE samples.
    pub pcm: Vec<u8>,
}

/// The live process's side of the conference: what Python's `LiveTranscriber`
/// registers with the setters. Every method has the behaviour of a callback that was
/// never registered.
pub trait LiveEvents: Send + Sync {
    /// `_on_speaker_change(speaker_name, is_speaking)` after every `speaker_update`.
    fn speaker_changed(&self, _speaker: Option<&str>, _is_speaking: bool) {}
    /// `_on_segment_complete(segment)`.
    fn segment_completed(&self, _segment: &SpeakerSegment) {}
    /// `_on_pre_meeting_switch()`: before the storage switches meetings, so in-flight
    /// chunks of the old meeting are dropped.
    fn before_meeting_switch(&self) {}
    /// `_on_meeting_change(meeting_id, meeting_path)`: after the switch, to rotate the
    /// audio writers.
    ///
    /// # Errors
    /// What made it fail: Python lets the exception through, except on a
    /// `ktalk_only` join, which answers `capture_unavailable`.
    fn meeting_changed(&self, _meeting_id: Option<&str>, _meeting_path: Option<&Path>) -> Result<(), String> {
        Ok(())
    }
    /// `_on_pcm_chunk(chunk)`; None in Python when no ingest is wired, which
    /// [`LiveEvents::wants_pcm`] says.
    ///
    /// # Errors
    /// What the ingest raised; the bridge logs it and goes on.
    fn pcm_chunk(&self, _chunk: PcmChunk) -> Result<(), String> {
        Ok(())
    }
    /// Whether `pcm_chunk` is wired (`_on_pcm_chunk is not None`).
    fn wants_pcm(&self) -> bool {
        false
    }
    /// `_pcm_stats_getter()` for the debug file: None when not wired.
    fn pcm_stats(&self) -> Option<Result<Map, String>> {
        None
    }
}

/// The capture switch of a `ktalk_only` installation (`set_meeting_capture_callback`).
pub trait MeetingCapture: Send + Sync {
    /// Turns the capture on after an accepted join, off before any leave.
    ///
    /// # Errors
    /// The capture could not start (the join is then refused).
    fn set_capture(&self, active: bool) -> Result<(), String>;
    /// `active_getter()`: whether the capture runs.
    fn recording_active(&self) -> bool;
}

/// The live process's user identity (`configure_user_profile(read, apply)`).
pub trait IdentitySource: Send + Sync {
    /// `read_identity()`: the identity resolved afresh from the profile file.
    ///
    /// # Errors
    /// The profile cannot be read.
    fn read_identity(&self) -> Result<Value, String>;
    /// `apply_mic(identity)`: relabel the microphone.
    ///
    /// # Errors
    /// The live process refused it.
    fn apply_mic_identity(&self, identity: &Value) -> Result<(), String>;
}

/// The Copilot task of the AI settings (`AiSettingsStore.resolve_task("copilot")`):
/// the three fields the bridge passes on.
#[derive(Debug, Clone, PartialEq)]
pub struct CopilotPolicy {
    pub instructions: Value,
    pub revision: Value,
    pub enabled: Value,
}

impl CopilotPolicy {
    /// The `copilot_*` fields frames carry, in Python's key order.
    #[must_use]
    pub fn to_map(&self) -> Map {
        let mut map = Map::new();
        map.insert("copilot_instructions", self.instructions.clone());
        map.insert("copilot_policy_revision", self.revision.clone());
        map.insert("copilot_enabled", self.enabled.clone());
        map
    }

    /// Python's `policy != other`: dict equality.
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        Value::Object(self.to_map()).py_eq(&Value::Object(other.to_map()))
    }
}

/// Where the Copilot policy comes from: read at construction and again on every
/// `ai_settings_refresh`. [`crate::ai::AiSettingsPolicy`] reads `ai-settings.json`
/// through `recog-ai`.
pub trait PolicySource: Send + Sync {
    /// # Errors
    /// The settings cannot be read or are invalid.
    fn read_policy(&self) -> Result<CopilotPolicy, String>;
}

/// A policy that does not change: the shipped default, or a test's.
#[derive(Debug, Clone)]
pub struct FixedPolicy(pub CopilotPolicy);

impl PolicySource for FixedPolicy {
    fn read_policy(&self) -> Result<CopilotPolicy, String> {
        Ok(self.0.clone())
    }
}

/// `IncrementGenerator.generate_all(meeting_id, meeting_path)`, run in the
/// background after a conference meeting ends.
pub trait IncrementGenerator: Send + Sync {
    fn generate_all(&self, meeting_id: String, meeting_path: PathBuf) -> BoxFuture<Result<(), String>>;
}

/// The golden recorder's bridge taps (`recog/golden.py`): clients numbered per
/// session in connection order.
pub trait GoldenTap: Send + Sync {
    fn client_connected(&self, client: u64);
    fn frame(&self, client: u64, data: &Value);
    fn client_disconnected(&self, client: u64);
}

/// The part of `MeetingStorage` the bridge uses. Errors are the text of the Python
/// exception (`str(e)`).
pub trait MeetingStore: Send + Sync {
    fn current_meeting_id(&self) -> Option<String>;
    fn current_meeting_path(&self) -> Option<PathBuf>;
    /// `_load_meta()`.
    ///
    /// # Errors
    /// The file cannot be read or parsed, or there is no current meeting.
    fn load_meta(&self) -> Result<Value, String>;
    /// `is_conference_source(source)` with the installation's aliases.
    fn is_conference_source(&self, source: Option<&str>) -> bool;
    /// Moves the storage to the conference's meeting and answers its id. The bridge
    /// calls it only inside `Bridge::switch_stored_meeting`, so the live side's
    /// writers follow.
    ///
    /// # Errors
    /// As Python raises.
    fn on_meeting_join(&self, url: &str, title: Option<&str>, platform: &str) -> Result<Option<String>, String>;
    /// Ends the meeting, opens an orphan after it and answers the orphan's id. The
    /// bridge calls it only inside `Bridge::switch_stored_meeting`.
    ///
    /// # Errors
    /// As Python raises.
    fn on_ktalk_leave(&self, reason: &str) -> Result<String, String>;
    /// # Errors
    /// As Python raises.
    fn update_title(&self, title: &str) -> Result<(), String>;
    /// # Errors
    /// As Python raises.
    fn get_current_transcript(&self) -> Result<Vec<Value>, String>;
}

impl MeetingStore for MeetingStorage {
    fn current_meeting_id(&self) -> Option<String> {
        MeetingStorage::current_meeting_id(self)
    }
    fn current_meeting_path(&self) -> Option<PathBuf> {
        MeetingStorage::current_meeting_path(self)
    }
    fn load_meta(&self) -> Result<Value, String> {
        MeetingStorage::load_meta(self).map_err(|e| e.to_string())
    }
    fn is_conference_source(&self, source: Option<&str>) -> bool {
        MeetingStorage::is_conference_source(self, source)
    }
    fn on_meeting_join(&self, url: &str, title: Option<&str>, platform: &str) -> Result<Option<String>, String> {
        MeetingStorage::on_meeting_join(self, url, title, platform).map_err(|e| e.to_string())
    }
    fn on_ktalk_leave(&self, reason: &str) -> Result<String, String> {
        MeetingStorage::on_ktalk_leave(self, reason).map_err(|e| e.to_string())
    }
    fn update_title(&self, title: &str) -> Result<(), String> {
        MeetingStorage::update_title(self, title).map_err(|e| e.to_string())
    }
    fn get_current_transcript(&self) -> Result<Vec<Value>, String> {
        MeetingStorage::get_current_transcript(self).map_err(|e| e.to_string())
    }
}

/// Where "now" comes from: `time.time()`, `time.monotonic()` and the local zone of
/// `datetime.now()`, behind a trait so tests and the differential probe fix them.
pub trait Clock: Send + Sync {
    /// `time.time()`.
    fn time(&self) -> f64;
    /// `time.monotonic()`.
    fn monotonic(&self) -> f64;
    /// The local zone's offset from UTC in seconds at an instant (microseconds since
    /// the epoch).
    fn utc_offset(&self, utc_micros: i64) -> i64;
}

/// The system clock and zone.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn time(&self) -> f64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
    }
    fn monotonic(&self) -> f64 {
        use std::sync::OnceLock;
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        // Any fixed origin will do: only differences are used.
        START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() + 1.0e6
    }
    fn utc_offset(&self, utc_micros: i64) -> i64 {
        recog_core::time::Clock::utc_offset(&recog_core::time::SystemClock, utc_micros)
    }
}

/// `datetime.fromtimestamp(t).isoformat()` (`now` is `time.time()`): local naive time.
#[must_use]
pub fn local_isoformat(clock: &dyn Clock, t: f64) -> String {
    let utc = recog_core::time::from_timestamp(t);
    let instant = utc.utc_micros().unwrap_or(utc.wall_micros);
    let wall = instant + clock.utc_offset(instant) * 1_000_000;
    recog_core::time::PyDateTime { wall_micros: wall, offset_micros: None }.isoformat()
}
