//! `LiveTranscriber` of `recog/live.py`: the per-source state, the chunk path (WAV,
//! VAD, ASR, dedup, attribution), the per-speaker batch path and its refine alignment,
//! the markers, meeting changes with the WAV writers, and the capture policy of a
//! `ktalk_only` installation.
//!
//! Threads as in Python: the main loop calls [`Live::process_chunk`] and the
//! once-per-iteration checks; the bridge's thread calls the meeting-switch and capture
//! hooks; the ingest's worker calls [`Live::on_pcm_batch`] and the refine worker
//! [`Live::apply_refine`]. The state they share is behind the same locks Python takes
//! (`state` = `_state_lock`, `wav` = `_wav_lock`, `capture_lock` = `_capture_lock`),
//! never held across a call into the bridge's lock-taking code from the bridge's side.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use parking_lot::ReentrantMutex;
use recog_bridge::{RefineUpdate, TranscriptLine};
use recog_core::meeting_storage::{MeetingStorage, NewSegment, SegmentUpdate};
use recog_core::pyjson::Value;
use recog_core::user_profile::{UserProfileStore, resolve_identity};
use recog_core::wav::IncrementalWavWriter;

use crate::asr::{AsrError, Transcription};
use crate::clock::SharedClock;
use crate::config::{
    LAST_SPEAKER_TTL, MAX_SEGMENT_DURATION, PCM_REFINE_WINDOW_S, PCM_SILENCE_WATCHDOG_S, PCM_WAV_ANCHOR_MAX_AGE_MS,
    RETROACTIVE_MAX_SECONDS, RETROACTIVE_MAX_SEGMENTS, SAMPLE_RATE, SILENCE_SPLIT_TIMEOUT, SPEAKER_TURN_GAP,
    VAD_RMS_THRESHOLD,
};
use crate::golden::{AsrOutput, Audio, GoldenRecorder};
use crate::live::parts::{Conference, LiveAsr, LiveCapture};
use crate::live::text::{deduplicate, last_words, pcm_is_silent, rms_f32};
use crate::live::{SILENCE_CHECK_INTERVAL_S, SYSTEM_AUDIO_SPEAKER};
use crate::log::Logger;
use crate::per_speaker::{BatchedChunk, PerSpeakerIngest};
use crate::refine::{RefineResult, RefineTask, RefineWorker};

/// Where stdout text goes: `sys.stdout.write(text)`, no newline added.
pub type Out = Arc<dyn Fn(&str) + Send + Sync>;

/// The process's stdout, flushed after every write as the transcript lines are.
#[must_use]
pub fn stdout_out() -> Out {
    Arc::new(|text: &str| {
        use std::io::Write as _;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    })
}

/// The arguments of `LiveTranscriber(...)` once the CLI has resolved them.
#[derive(Debug, Clone)]
pub struct LiveOptions {
    /// The resolved backend: `screencapture`, `pulse`, `device:<id>` or `none`.
    pub system_audio: String,
    /// The microphone as the banner shows it (`mic_device`, or `default`).
    pub mic_display: String,
    pub chunk_duration: f64,
    pub output_file: Option<PathBuf>,
    /// Conference signals from the extension (`--ktalk`; implied by `ktalk_only`).
    pub use_ktalk: bool,
    pub recording_policy: recog_core::platforms::RecordingPolicy,
    pub save_audio: bool,
    /// `--mic-speaker-name`.
    pub mic_speaker_name: Option<String>,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            system_audio: "none".into(),
            mic_display: "default".into(),
            chunk_duration: 3.0,
            output_file: None,
            use_ktalk: false,
            recording_policy: Default::default(),
            save_audio: false,
            mic_speaker_name: None,
        }
    }
}

/// What the transcriber reads from its surroundings.
#[derive(Clone)]
pub struct LiveEnv {
    pub data_dir: PathBuf,
    /// `[live] dedup_max_gap_s`.
    pub dedup_max_gap_s: f64,
    /// `MIC_SPEAKER_NAME`: the microphone's name before a profile is saved.
    pub legacy_mic_name: String,
    pub clock: SharedClock,
    /// The local zone for `HH:MM:SS`.
    pub zone: Arc<dyn recog_core::time::Clock>,
    /// stderr.
    pub log: Logger,
    /// stdout.
    pub out: Out,
}

/// The per-source state of the mixed path (`SourceState`).
#[derive(Debug, Clone)]
pub struct SourceState {
    pub last_words: Vec<String>,
    pub last_speaker: Option<String>,
    pub last_speech_time: f64,
    pub segment_start_time: f64,
    pub last_text_time: f64,
    pub audio_offset: i64,
    /// The capture time of the end of the chunk that gave the last segment.
    pub last_words_capture_end: f64,
    /// A chunk ASR could not serve: (samples, capture time, generation).
    pub pending_chunk: Option<(Vec<f32>, f64, i64)>,
}

impl SourceState {
    /// A fresh state, its last text time now.
    #[must_use]
    pub fn new(now: f64) -> Self {
        Self {
            last_words: Vec::new(),
            last_speaker: None,
            last_speech_time: 0.0,
            segment_start_time: 0.0,
            last_text_time: now,
            audio_offset: 0,
            last_words_capture_end: 0.0,
            pending_chunk: None,
        }
    }
}

/// A recent system-audio segment kept for the retroactive correction.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentSegment {
    pub speaker: String,
    pub chunk_time: f64,
    /// Index in the session's transcript lines.
    pub line_idx: usize,
    pub meeting_segment_id: i64,
    pub is_local: bool,
    pub timestamp_display: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy)]
struct PidState {
    last_speech_time: f64,
    segment_start_time: f64,
}

/// What `_state_lock` guards.
#[derive(Default)]
struct State {
    recent_system_segments: VecDeque<RecentSegment>,
    mic: Option<SourceState>,
    sys: Option<SourceState>,
    pcm_prev_words: HashMap<String, Vec<String>>,
    pcm_pid_state: HashMap<String, PidState>,
    /// pid → (segment_id, pid_audio_ms_start, pid_audio_ms_end), oldest first.
    pcm_live_segments: HashMap<String, Vec<(i64, f64, f64)>>,
    pcm_segment_texts: HashMap<i64, String>,
    last_pcm_degraded: bool,
    system_capture_lost: bool,
    system_capture_lost_meeting: Option<PathBuf>,
}

impl State {
    fn source(&mut self, is_mic: bool) -> &mut SourceState {
        let slot = if is_mic { &mut self.mic } else { &mut self.sys };
        slot.get_or_insert_with(|| SourceState::new(0.0))
    }
}

/// What `_wav_lock` guards.
#[derive(Default)]
struct Wav {
    mic: Option<IncrementalWavWriter>,
    system: Option<IncrementalWavWriter>,
    /// (wall ms of the first sample of the last system write, WAV offset ms before it).
    system_anchor: Option<(f64, i64)>,
    system_source_epoch: u64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn storage_error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The live transcriber (`LiveTranscriber`).
pub struct Live {
    pub(crate) opts: LiveOptions,
    pub(crate) env: LiveEnv,
    mic_capture: Option<Arc<dyn LiveCapture>>,
    system_capture: Option<Arc<dyn LiveCapture>>,
    asr: Arc<dyn LiveAsr>,
    profile_store: UserProfileStore,
    identity: Mutex<Value>,
    mic_speaker_name: Mutex<String>,
    use_ktalk: AtomicBool,
    use_pcm_transport: AtomicBool,
    running: AtomicBool,
    transcript_lines: Mutex<Vec<String>>,
    ktalk_current_speaker: Mutex<Option<String>>,
    storage: Mutex<Option<Arc<MeetingStorage>>>,
    conference: Mutex<Option<Arc<dyn Conference>>>,
    ingest: Mutex<Option<Arc<PerSpeakerIngest>>>,
    refine: Mutex<Option<Arc<RefineWorker>>>,
    golden: Mutex<Option<Arc<GoldenRecorder>>>,
    capture_lock: ReentrantMutex<()>,
    captures_ready: AtomicBool,
    recording_allowed: AtomicBool,
    wav: Mutex<Wav>,
    state: Mutex<State>,
    generation: AtomicI64,
    next_silence_check_at: Mutex<f64>,
}

impl Live {
    /// `LiveTranscriber(...)` with its captures and ASR already built.
    ///
    /// # Errors
    /// Nothing to capture, or a user profile that cannot be read.
    pub fn new(
        opts: LiveOptions,
        env: LiveEnv,
        asr: Arc<dyn LiveAsr>,
        mic_capture: Option<Arc<dyn LiveCapture>>,
        system_capture: Option<Arc<dyn LiveCapture>>,
    ) -> Result<Arc<Self>, String> {
        if mic_capture.is_none() && system_capture.is_none() {
            return Err("нечего захватывать: микрофон отключён (--mic-device none) и системный звук выключен \
                        (--system-audio none)"
                .into());
        }
        let mut opts = opts;
        opts.use_ktalk = opts.use_ktalk || opts.recording_policy.gated();
        let profile_store = UserProfileStore::new(&env.data_dir);
        let identity = read_identity(&profile_store, &env.legacy_mic_name, opts.mic_speaker_name.as_deref())?;
        let name = identity_name(&identity);
        let use_ktalk = opts.use_ktalk;
        let recording_allowed = !opts.recording_policy.gated();
        Ok(Arc::new(Self {
            opts,
            env,
            mic_capture,
            system_capture,
            asr,
            profile_store,
            identity: Mutex::new(identity),
            mic_speaker_name: Mutex::new(name),
            use_ktalk: AtomicBool::new(use_ktalk),
            use_pcm_transport: AtomicBool::new(use_ktalk),
            running: AtomicBool::new(false),
            transcript_lines: Mutex::new(Vec::new()),
            ktalk_current_speaker: Mutex::new(None),
            storage: Mutex::new(None),
            conference: Mutex::new(None),
            ingest: Mutex::new(None),
            refine: Mutex::new(None),
            golden: Mutex::new(None),
            capture_lock: ReentrantMutex::new(()),
            captures_ready: AtomicBool::new(false),
            recording_allowed: AtomicBool::new(recording_allowed),
            wav: Mutex::new(Wav::default()),
            state: Mutex::new(State::default()),
            generation: AtomicI64::new(0),
            next_silence_check_at: Mutex::new(0.0),
        }))
    }

    // ---- accessors ----

    #[must_use]
    pub fn options(&self) -> &LiveOptions {
        &self.opts
    }

    #[must_use]
    pub fn mic_capture(&self) -> Option<&Arc<dyn LiveCapture>> {
        self.mic_capture.as_ref()
    }

    #[must_use]
    pub fn system_capture(&self) -> Option<&Arc<dyn LiveCapture>> {
        self.system_capture.as_ref()
    }

    /// Both a microphone and system audio (`dual_mode`).
    #[must_use]
    pub fn dual_mode(&self) -> bool {
        self.mic_capture.is_some() && self.system_capture.is_some()
    }

    #[must_use]
    pub fn asr(&self) -> &Arc<dyn LiveAsr> {
        &self.asr
    }

    #[must_use]
    pub fn use_ktalk(&self) -> bool {
        self.use_ktalk.load(Ordering::SeqCst)
    }

    pub fn set_use_ktalk(&self, on: bool) {
        self.use_ktalk.store(on, Ordering::SeqCst);
    }

    #[must_use]
    pub fn use_pcm_transport(&self) -> bool {
        self.use_pcm_transport.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// `transcriber.running = value`: false ends the main loop after its iteration.
    pub fn set_running(&self, on: bool) {
        self.running.store(on, Ordering::SeqCst);
    }

    #[must_use]
    pub fn generation(&self) -> i64 {
        self.generation.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn mic_speaker_name(&self) -> String {
        lock(&self.mic_speaker_name).clone()
    }

    #[must_use]
    pub fn user_identity(&self) -> Value {
        lock(&self.identity).clone()
    }

    #[must_use]
    pub fn transcript_lines(&self) -> Vec<String> {
        lock(&self.transcript_lines).clone()
    }

    #[must_use]
    pub fn storage(&self) -> Option<Arc<MeetingStorage>> {
        lock(&self.storage).clone()
    }

    pub fn set_storage(&self, storage: Option<Arc<MeetingStorage>>) {
        *lock(&self.storage) = storage;
    }

    #[must_use]
    pub fn conference(&self) -> Option<Arc<dyn Conference>> {
        lock(&self.conference).clone()
    }

    pub fn set_conference(&self, conference: Option<Arc<dyn Conference>>) {
        *lock(&self.conference) = conference;
    }

    #[must_use]
    pub fn ingest(&self) -> Option<Arc<PerSpeakerIngest>> {
        lock(&self.ingest).clone()
    }

    pub fn set_ingest(&self, ingest: Option<Arc<PerSpeakerIngest>>) {
        *lock(&self.ingest) = ingest;
    }

    #[must_use]
    pub fn refine_worker(&self) -> Option<Arc<RefineWorker>> {
        lock(&self.refine).clone()
    }

    pub fn set_refine_worker(&self, worker: Option<Arc<RefineWorker>>) {
        *lock(&self.refine) = worker;
    }

    #[must_use]
    pub fn golden(&self) -> Option<Arc<GoldenRecorder>> {
        lock(&self.golden).clone()
    }

    pub fn set_golden(&self, golden: Option<Arc<GoldenRecorder>>) {
        *lock(&self.golden) = golden;
    }

    /// The source states the main loop starts with (`SourceState()` twice in `run`).
    pub fn reset_source_states(&self) {
        let now = self.env.clock.wall();
        let mut st = lock(&self.state);
        st.mic = Some(SourceState::new(now));
        st.sys = Some(SourceState::new(now));
    }

    /// A copy of one source's state.
    #[must_use]
    pub fn source_state(&self, is_mic: bool) -> Option<SourceState> {
        let st = lock(&self.state);
        if is_mic { st.mic.clone() } else { st.sys.clone() }
    }

    /// Replaces one source's state (tests set up a situation this way).
    pub fn set_source_state(&self, is_mic: bool, state: SourceState) {
        let mut st = lock(&self.state);
        if is_mic {
            st.mic = Some(state);
        } else {
            st.sys = Some(state);
        }
    }

    /// The recent system-audio segments of the retroactive correction.
    #[must_use]
    pub fn recent_system_segments(&self) -> Vec<RecentSegment> {
        lock(&self.state).recent_system_segments.iter().cloned().collect()
    }

    /// The per-pid live-segment registry of one pid.
    #[must_use]
    pub fn pcm_live_segments(&self, pid: &str) -> Vec<(i64, f64, f64)> {
        lock(&self.state).pcm_live_segments.get(pid).cloned().unwrap_or_default()
    }

    /// The cached text of a live PCM segment.
    #[must_use]
    pub fn pcm_segment_text(&self, segment_id: i64) -> Option<String> {
        lock(&self.state).pcm_segment_texts.get(&segment_id).cloned()
    }

    /// The wall↔WAV anchor of the PCM offsets.
    #[must_use]
    pub fn system_wav_anchor(&self) -> Option<(f64, i64)> {
        lock(&self.wav).system_anchor
    }

    pub fn set_system_wav_anchor(&self, anchor: Option<(f64, i64)>) {
        lock(&self.wav).system_anchor = anchor;
    }

    /// The current WAV offsets in ms (mic, system), None for no writer.
    #[must_use]
    pub fn wav_offsets_ms(&self) -> (Option<i64>, Option<i64>) {
        let wav = lock(&self.wav);
        (
            wav.mic.as_ref().map(IncrementalWavWriter::offset_ms),
            wav.system.as_ref().map(IncrementalWavWriter::offset_ms),
        )
    }

    /// The paths of the open WAV writers (mic, system).
    #[must_use]
    pub fn wav_paths(&self) -> (Option<PathBuf>, Option<PathBuf>) {
        let wav = lock(&self.wav);
        (wav.mic.as_ref().map(|w| w.path.clone()), wav.system.as_ref().map(|w| w.path.clone()))
    }

    /// Whether the main loop's previous chunk saw the ingest degraded.
    #[must_use]
    pub fn last_pcm_degraded(&self) -> bool {
        lock(&self.state).last_pcm_degraded
    }

    pub fn set_last_pcm_degraded(&self, degraded: bool) {
        lock(&self.state).last_pcm_degraded = degraded;
    }

    fn print(&self, text: &str) {
        (self.env.out)(&format!("{text}\n"));
    }

    fn write_out(&self, text: &str) {
        (self.env.out)(text);
    }

    fn push_line(&self, line: String) -> usize {
        self.write_out(&line);
        let mut lines = lock(&self.transcript_lines);
        lines.push(line);
        lines.len() - 1
    }

    /// `datetime.fromtimestamp(t).strftime("%H:%M:%S")` in the local zone.
    #[must_use]
    pub fn hms(&self, t: f64) -> String {
        let instant = recog_core::time::from_timestamp(t);
        recog_core::time::hms(instant.local_wall(&*self.env.zone))
    }

    // ---- identity ----

    /// `_read_user_identity`: the identity resolved afresh from the profile file.
    ///
    /// # Errors
    /// The profile cannot be read.
    pub fn read_user_identity(&self) -> Result<Value, String> {
        read_identity(&self.profile_store, &self.env.legacy_mic_name, self.opts.mic_speaker_name.as_deref())
    }

    /// `_apply_user_identity`: the microphone's name from a new identity.
    pub fn apply_user_identity(&self, identity: &Value) {
        *lock(&self.mic_speaker_name) = identity_name(identity);
        *lock(&self.identity) = identity.clone();
    }

    /// `system_audio_label`: the system-audio source for the banner.
    #[must_use]
    pub fn system_audio_label(&self) -> String {
        match self.opts.system_audio.as_str() {
            "screencapture" => "ScreenCaptureKit".into(),
            "pulse" => "PulseAudio/PipeWire (parec)".into(),
            other => other.strip_prefix("device:").map_or_else(|| "none".into(), str::to_owned),
        }
    }

    // ---- recording policy ----

    /// `is_recording_active`.
    #[must_use]
    pub fn is_recording_active(&self) -> bool {
        !self.opts.recording_policy.gated() || self.recording_allowed.load(Ordering::SeqCst)
    }

    fn is_current_recording(&self, generation: i64) -> bool {
        self.generation() == generation && self.is_recording_active()
    }

    #[must_use]
    pub fn captures_ready(&self) -> bool {
        self.captures_ready.load(Ordering::SeqCst)
    }

    // ---- golden ----

    /// `_golden_now`: the recorder while a conference is on.
    #[must_use]
    pub fn golden_now(&self) -> Option<Arc<GoldenRecorder>> {
        let golden = self.golden()?;
        let conference = self.conference()?;
        conference.is_conference_active().then_some(golden)
    }

    // ---- ASR ----

    /// `_transcribe_direct`: the chunk's text, live priority signalled to refine.
    ///
    /// # Errors
    /// Only `TemporarilyUnavailable`: any other failure is logged and gives None.
    pub fn transcribe_direct(&self, chunk: &[f32]) -> Result<Option<String>, AsrError> {
        let refine = self.refine_worker();
        if let Some(worker) = &refine {
            worker.set_live_pending();
        }
        let result = self.asr.transcribe_chunk(chunk);
        if let Some(worker) = &refine {
            worker.clear_live_pending();
        }
        match result {
            Ok(text) => {
                if let Some(golden) = self.golden_now() {
                    let output = text.as_deref().map_or(AsrOutput::None, AsrOutput::Text);
                    golden.asr("transcribe_chunk", &[Audio::F32(chunk)], output);
                }
                Ok(text)
            }
            Err(AsrError::TemporarilyUnavailable) => Err(AsrError::TemporarilyUnavailable),
            Err(e) => {
                self.env.log.rate_limited(
                    "live-transcribe-direct",
                    &format!("[LiveTranscriber] _transcribe_direct failed: {}: {e}", exception_name(&e)),
                );
                Ok(None)
            }
        }
    }

    /// `_refine_transcribe_fn`: the refine window through ASR, recorded when golden.
    ///
    /// # Errors
    /// What ASR failed with.
    pub fn refine_transcribe(
        &self,
        pcm: &[i16],
        should_abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Transcription, AsrError> {
        let result = self.asr.transcribe_with_timestamps(pcm, should_abort)?;
        if let Some(golden) = self.golden_now() {
            golden.asr("transcribe_with_timestamps", &[Audio::I16(pcm)], AsrOutput::Timed(&result));
        }
        Ok(result)
    }

    // ---- conference signals ----

    /// `_ktalk_speaker_callback`.
    pub fn ktalk_speaker_changed(&self, speaker: Option<&str>, is_speaking: bool) {
        *lock(&self.ktalk_current_speaker) = if is_speaking { speaker.map(str::to_owned) } else { None };
    }

    // ---- retroactive correction ----

    /// `_retroactive_correct_unknown_run`: re-attributes the Unknown run before the
    /// segment just identified, from a snapshot taken with the segment's append.
    ///
    /// # Errors
    /// The transcript rewrite failed.
    pub fn retroactive_correct_unknown_run(
        &self,
        identified: &str,
        current_time: f64,
        snapshot: Option<(Vec<RecentSegment>, i64, Option<PathBuf>)>,
    ) -> Result<(), String> {
        let (segments, snapshot_gen, snapshot_path) = match snapshot {
            Some(s) => s,
            None => {
                let st = lock(&self.state);
                let path = self.storage().and_then(|s| s.current_meeting_path());
                (st.recent_system_segments.iter().cloned().collect(), self.generation(), path)
            }
        };
        let to_correct = unknown_run(&segments, current_time);
        if to_correct.is_empty() {
            return Ok(());
        }
        self.relabel_lines(&to_correct, identified);
        let meeting_switched = self.relabel_recent(&to_correct, identified, snapshot_gen);
        self.store_correction(&to_correct, identified, snapshot_path.as_deref())?;
        if let Some(conference) = self.conference()
            && !meeting_switched
        {
            for seg in &to_correct {
                conference.send_transcript(&TranscriptLine {
                    speaker: identified.to_owned(),
                    text: seg.text.clone(),
                    timestamp: seg.timestamp_display.clone(),
                    correction: true,
                    segment_id: seg.meeting_segment_id,
                    ..TranscriptLine::default()
                });
            }
        }
        Ok(())
    }

    /// The printed lines of the corrected run, Unknown replaced by `identified`.
    fn relabel_lines(&self, to_correct: &[RecentSegment], identified: &str) {
        let mut lines = lock(&self.transcript_lines);
        for seg in to_correct {
            if let Some(line) = lines.get_mut(seg.line_idx) {
                *line = line.replacen("**Unknown**", &format!("**{identified}**"), 1);
            }
        }
    }

    /// The recent segments of the corrected run relabelled; whether the meeting
    /// switched since the snapshot.
    fn relabel_recent(&self, to_correct: &[RecentSegment], identified: &str, snapshot_gen: i64) -> bool {
        let corrected: HashSet<usize> = to_correct.iter().map(|s| s.line_idx).collect();
        let mut st = lock(&self.state);
        // Python updates the segment objects themselves, which the deque holds.
        for seg in &mut st.recent_system_segments {
            if corrected.contains(&seg.line_idx) {
                seg.speaker = identified.to_owned();
            }
        }
        self.generation() != snapshot_gen
    }

    /// The stored rows of the corrected run rewritten in the snapshot's meeting.
    fn store_correction(
        &self,
        to_correct: &[RecentSegment],
        identified: &str,
        snapshot_path: Option<&Path>,
    ) -> Result<(), String> {
        let Some(storage) = self.storage() else { return Ok(()) };
        let updates: Vec<SegmentUpdate> = to_correct
            .iter()
            .filter(|s| s.meeting_segment_id > 0)
            .map(|s| SegmentUpdate {
                segment_id: s.meeting_segment_id,
                speaker: Some(identified.to_owned()),
                uncertain: Some(false),
                uncertain_reason: Some(String::new()),
                text: None,
            })
            .collect();
        if !updates.is_empty() {
            storage.update_segments(&updates, snapshot_path).map_err(storage_error)?;
        }
        Ok(())
    }

    // ---- storage and writers ----

    /// `_setup_meeting_storage` after the storage is built: the session and the writers.
    ///
    /// # Errors
    /// The session or a writer could not be opened (`[FATAL ERROR] MeetingStorage init
    /// failed`).
    pub fn start_storage_session(&self, storage: Arc<MeetingStorage>) -> Result<(), String> {
        let mode = recog_core::meeting_storage::SessionMode {
            mic: self.mic_capture.is_some(),
            system_audio: self.system_capture.is_some(),
            ktalk: self.use_ktalk(),
            screencapture: self.opts.system_audio == "screencapture",
            system_audio_backend: self.opts.system_audio.clone(),
            #[allow(clippy::cast_possible_truncation)]
            chunk_duration_ms: (self.opts.chunk_duration * 1000.0) as i64,
            recording_policy: self.opts.recording_policy,
        };
        let meeting_id = storage.start_session(&mode).map_err(storage_error)?;
        self.print(&format!("Meeting started: {meeting_id}"));
        if self.opts.save_audio && !self.opts.recording_policy.gated() {
            let meeting_path = storage.base_path().join(&meeting_id);
            let mut wav = lock(&self.wav);
            if self.mic_capture.is_some() {
                let path = meeting_path.join("audio_mic.wav");
                let mut writer = IncrementalWavWriter::new(&path);
                writer.open(false).map_err(|e| e.to_string())?;
                wav.mic = Some(writer);
                self.print(&format!("Recording mic audio: {}", path.display()));
            }
            if self.system_capture.is_some() {
                let path = meeting_path.join("audio_system.wav");
                let mut writer = IncrementalWavWriter::new(&path);
                writer.open(false).map_err(|e| e.to_string())?;
                wav.system = Some(writer);
                self.print(&format!("Recording system audio: {}", path.display()));
            }
        }
        self.set_storage(Some(storage));
        Ok(())
    }

    /// `_close_audio_writers`: finalize the writers and record their paths in the
    /// meeting they belong to.
    ///
    /// # Errors
    /// Finalizing or writing `meta.json` failed.
    pub fn close_audio_writers(&self) -> Result<(), String> {
        let mut wav = lock(&self.wav);
        let mut mic_path = None;
        let mut system_path = None;
        if let Some(mut writer) = wav.mic.take() {
            writer.finalize().map_err(|e| e.to_string())?;
            let path = writer.path.display().to_string();
            self.print(&format!("Mic audio saved: {path}"));
            mic_path = Some(path);
        }
        if let Some(mut writer) = wav.system.take() {
            writer.finalize().map_err(|e| e.to_string())?;
            let path = writer.path.display().to_string();
            self.print(&format!("System audio saved: {path}"));
            system_path = Some(path);
        }
        wav.system_anchor = None;
        let old_meeting = mic_path.as_ref().or(system_path.as_ref()).and_then(|p| Path::new(p).parent());
        if let (Some(old), Some(storage)) = (old_meeting, self.storage()) {
            storage.set_audio_paths(mic_path.as_deref(), system_path.as_deref(), Some(old)).map_err(storage_error)?;
        }
        Ok(())
    }

    /// `_on_meeting_change`: rotate the writers into the new meeting's directory;
    /// `continuation` means the next chunk still carries the previous one's overlap.
    ///
    /// # Errors
    /// Closing or opening a writer failed.
    pub fn on_meeting_change(&self, _meeting_id: &str, meeting_path: &Path, continuation: bool) -> Result<(), String> {
        if !self.opts.save_audio {
            return Ok(());
        }
        self.close_audio_writers()?;
        if self.opts.recording_policy.gated() && !self.current_meeting_is_allowed()? {
            return Ok(());
        }
        self.open_audio_writers(meeting_path, continuation)
    }

    /// A writer for each capture there is, in the meeting's directory.
    fn open_audio_writers(&self, meeting_path: &Path, continuation: bool) -> Result<(), String> {
        let mut wav = lock(&self.wav);
        if self.mic_capture.is_some() {
            wav.mic = Some(self.open_track_writer(meeting_path, "mic", continuation)?);
        }
        if self.system_capture.is_some() {
            wav.system = Some(self.open_track_writer(meeting_path, "system", continuation)?);
        }
        Ok(())
    }

    /// Whether the current meeting's `meta.json` says it is a KTalk meeting.
    fn current_meeting_is_allowed(&self) -> Result<bool, String> {
        let meta = self.storage().map(|s| s.load_meta().map_err(storage_error)).transpose()?;
        let source = meta.as_ref().and_then(|m| m.as_object()).and_then(|m| m.get("source")).cloned();
        Ok(source.as_ref().and_then(Value::as_str).is_some_and(|s| self.opts.recording_policy.accepts(s)))
    }

    /// `audio_<track>.wav` of the meeting, opened for writing.
    fn open_track_writer(
        &self,
        meeting_path: &Path,
        track: &str,
        continuation: bool,
    ) -> Result<IncrementalWavWriter, String> {
        let path = meeting_path.join(format!("audio_{track}.wav"));
        let mut writer = IncrementalWavWriter::new(&path);
        writer.open(continuation).map_err(|e| e.to_string())?;
        self.print(&format!("Recording {track} audio: {}", path.display()));
        Ok(writer)
    }

    /// `_before_meeting_switch`.
    pub fn before_meeting_switch(&self) {
        if self.opts.recording_policy.gated() {
            if let Err(e) = self.set_conference_capture(false) {
                self.env.log.line(&format!("[LiveTranscriber] stopping the capture failed: {e}"));
            }
        } else {
            self.invalidate_pending_chunks();
        }
    }

    /// `_invalidate_pending_chunks`: a meeting boundary; in-flight work of the old
    /// meeting is dropped and the capture buffers emptied.
    pub fn invalidate_pending_chunks(&self) {
        {
            let mut st = lock(&self.state);
            self.generation.fetch_add(1, Ordering::SeqCst);
            st.recent_system_segments.clear();
            if st.mic.is_some() {
                for is_mic in [true, false] {
                    let s = st.source(is_mic);
                    s.last_speaker = None;
                    s.last_words.clear();
                    s.segment_start_time = 0.0;
                    s.pending_chunk = None;
                }
            }
            st.pcm_prev_words.clear();
            st.pcm_pid_state.clear();
            st.pcm_live_segments.clear();
            st.pcm_segment_texts.clear();
            st.last_pcm_degraded = false;
            if let Some(worker) = self.refine_worker() {
                worker.pause();
                if self.is_recording_active() {
                    worker.resume();
                }
            }
        }
        if let Some(ingest) = self.ingest() {
            ingest.drain();
        }
        if let Some(capture) = &self.mic_capture {
            capture.clear_buffer();
        }
        if let Some(capture) = &self.system_capture {
            capture.clear_buffer();
        }
    }

    // ---- capture policy ----

    fn open_audio_captures(&self) -> Result<(), String> {
        for capture in [&self.mic_capture, &self.system_capture].into_iter().flatten() {
            if !capture.running() {
                capture.start()?;
            }
        }
        Ok(())
    }

    /// `_start_audio_captures`: devices start after initialization (in `ktalk_only`
    /// mode only on an allowed meeting join).
    ///
    /// # Errors
    /// A capture could not start.
    pub fn start_audio_captures(&self) -> Result<(), String> {
        let _guard = self.capture_lock.lock();
        self.captures_ready.store(true, Ordering::SeqCst);
        if self.opts.recording_policy.gated() {
            return Ok(());
        }
        self.open_audio_captures()
    }

    /// `_set_conference_capture`: the bridge's capture switch of `ktalk_only`.
    ///
    /// # Errors
    /// The capture is not ready or could not start; a failed stop.
    pub fn set_conference_capture(&self, active: bool) -> Result<(), String> {
        let _guard = self.capture_lock.lock();
        if active {
            if !self.captures_ready() {
                return Err("Audio capture is not ready".into());
            }
            if let Err(e) = self.open_conference_capture() {
                self.set_conference_capture(false)?;
                return Err(e);
            }
        } else {
            self.shut_conference_capture()?;
        }
        self.print(match (self.opts.recording_policy, active) {
            (recog_core::platforms::RecordingPolicy::KTalkOnly, true) => "KTalk recording active",
            (recog_core::platforms::RecordingPolicy::KTalkOnly, false) => {
                "Waiting for a KTalk meeting (audio capture off)"
            }
            (_, true) => "Conference recording active",
            (_, false) => "Waiting for a conference (audio capture off)",
        });
        Ok(())
    }

    /// The captures started, the meeting marked recording, recording allowed, refine
    /// resumed.
    fn open_conference_capture(&self) -> Result<(), String> {
        self.open_audio_captures()?;
        if let Some(storage) = self.storage() {
            storage.set_recording_active(true).map_err(storage_error)?;
        }
        self.recording_allowed.store(true, Ordering::SeqCst);
        if let Some(worker) = self.refine_worker() {
            worker.resume();
        }
        Ok(())
    }

    /// Recording stopped: pending chunks dropped, captures stopped and emptied, the
    /// writers closed, the meeting marked not recording (a failure to say so is only
    /// logged).
    fn shut_conference_capture(&self) -> Result<(), String> {
        self.recording_allowed.store(false, Ordering::SeqCst);
        self.invalidate_pending_chunks();
        for capture in [&self.mic_capture, &self.system_capture].into_iter().flatten() {
            capture.stop();
            capture.clear_buffer();
        }
        let closed = self.close_audio_writers();
        if let Some(storage) = self.storage()
            && let Err(e) = storage.set_recording_active(false)
        {
            self.env.log.line(&format!("Warning: recording status could not be saved: {e}"));
        }
        closed
    }

    /// The `ktalk_only` part of `_teardown_session`: captures no longer ready, off.
    ///
    /// # Errors
    /// As [`Live::set_conference_capture`].
    pub fn close_conference_capture(&self) -> Result<(), String> {
        let _guard = self.capture_lock.lock();
        self.captures_ready.store(false, Ordering::SeqCst);
        self.set_conference_capture(false)
    }

    // ---- markers ----

    /// `_emit_pcm_transition_marker`: a gap marker on a PCM ↔ mixed flip, refine
    /// paused while on the mixed fallback.
    ///
    /// # Errors
    /// The marker could not be appended.
    pub fn emit_pcm_transition_marker(&self, degraded: bool, marker_gen: i64) -> Result<(), String> {
        #[allow(clippy::cast_possible_truncation)]
        let msg = if degraded {
            format!("[PCM silence >{}s — switched to mixed fallback]", PCM_SILENCE_WATCHDOG_S as i64)
        } else {
            "[PCM recovered — back to per-speaker]".to_owned()
        };
        let timestamp = self.hms(self.env.clock.wall());
        self.push_line(format!("\n*{msg}* *[{timestamp}]*\n"));
        let _st = lock(&self.state);
        if self.generation() != marker_gen {
            return Ok(());
        }
        if let Some(worker) = self.refine_worker() {
            if degraded {
                worker.pause();
            } else {
                worker.resume();
            }
        }
        if let Some(storage) = self.storage() {
            storage.append_segment(&marker(&msg, "pcm_watchdog_transition")).map_err(storage_error)?;
        }
        Ok(())
    }

    /// `_emit_capture_loss_marker`.
    ///
    /// # Errors
    /// The marker could not be appended.
    pub fn emit_capture_loss_marker(&self, marker_gen: i64, recovered: bool) -> Result<(), String> {
        let (msg, reason) = if recovered {
            ("[Системный звук восстановлен]", "system_audio_capture_recovered")
        } else {
            ("[Системный звук потерян — захват перезапускается раз в минуту]", "system_audio_capture_lost")
        };
        let timestamp = self.hms(self.env.clock.wall());
        self.push_line(format!("\n*{msg}* *[{timestamp}]*\n"));
        let _st = lock(&self.state);
        if self.generation() != marker_gen {
            return Ok(());
        }
        if let Some(storage) = self.storage() {
            storage.append_segment(&marker(msg, reason)).map_err(storage_error)?;
        }
        Ok(())
    }

    fn current_meeting_key(&self) -> Option<PathBuf> {
        self.storage().and_then(|s| s.current_meeting_path())
    }

    /// `_check_system_capture_alive`: one marker when the system capture is lost, one
    /// when it delivers again.
    ///
    /// # Errors
    /// A marker could not be appended.
    pub fn check_system_capture_alive(&self) -> Result<(), String> {
        if !self.is_recording_active() {
            return Ok(());
        }
        let Some(capture) = &self.system_capture else { return Ok(()) };
        if !capture.dead() {
            let marker_gen = {
                let mut st = lock(&self.state);
                if !st.system_capture_lost {
                    return Ok(());
                }
                st.system_capture_lost = false;
                st.system_capture_lost_meeting = None;
                self.generation()
            };
            return self.emit_capture_loss_marker(marker_gen, true);
        }
        let marker_gen = {
            let mut st = lock(&self.state);
            let key = self.current_meeting_key();
            if st.system_capture_lost && st.system_capture_lost_meeting == key {
                return Ok(());
            }
            st.system_capture_lost = true;
            st.system_capture_lost_meeting = key;
            self.generation()
        };
        self.emit_capture_loss_marker(marker_gen, false)
    }

    /// `_restart_silent_system_capture`: a planned restart after a minute of exact
    /// zeros during a conference.
    pub fn restart_silent_system_capture(&self) {
        let now = self.env.clock.monotonic();
        {
            let mut next = lock(&self.next_silence_check_at);
            if now < *next {
                return;
            }
            *next = now + SILENCE_CHECK_INTERVAL_S;
        }
        if !self.is_recording_active() {
            return;
        }
        let Some(capture) = &self.system_capture else { return };
        let Some(silent_for) = capture.digital_silence_s() else { return };
        if silent_for < crate::capture::subprocess::DIGITAL_SILENCE_RESTART_S {
            return;
        }
        if !self.conference().is_some_and(|c| c.is_conference_active()) {
            return;
        }
        capture.request_restart(&format!("Only digital silence for {silent_for:.0}s during a conference"));
    }

    /// `_reopen_stopped_device_captures`.
    pub fn reopen_stopped_device_captures(&self) {
        if !self.is_recording_active() {
            return;
        }
        for capture in [&self.mic_capture, &self.system_capture].into_iter().flatten() {
            capture.reopen_if_broken();
        }
    }

    // ---- the mixed path ----

    /// `_check_silence_split`: ten minutes without text split an orphan meeting.
    ///
    /// # Errors
    /// The split or the writer rotation failed.
    pub fn check_silence_split(&self, is_mic: bool, current_time: f64, chunk: Option<&[f32]>) -> Result<(), String> {
        let Some(storage) = self.storage() else { return Ok(()) };
        let last_text_time = lock(&self.state).source(is_mic).last_text_time;
        if current_time - last_text_time >= SILENCE_SPLIT_TIMEOUT {
            if let Some(new_id) = storage.split_on_silence_timeout().map_err(storage_error)? {
                self.on_meeting_change(&new_id, &storage.base_path().join(&new_id), true)?;
                if let Some(chunk) = chunk {
                    self.write_split_chunk(is_mic, chunk)?;
                }
            }
            let mut st = lock(&self.state);
            let s = st.source(is_mic);
            s.last_speaker = None;
            s.last_words.clear();
        }
        lock(&self.state).source(is_mic).last_text_time = current_time;
        Ok(())
    }

    /// The chunk that triggered a split into the new meeting's own WAV, so its first
    /// segment points into that file.
    fn write_split_chunk(&self, is_mic: bool, chunk: &[f32]) -> Result<(), String> {
        let offset = {
            let mut wav = lock(&self.wav);
            let writer = if is_mic { wav.mic.as_mut() } else { wav.system.as_mut() };
            match writer {
                Some(writer) => {
                    let offset = writer.offset_ms();
                    writer.write_chunk(chunk).map_err(|e| e.to_string())?;
                    Some(offset)
                }
                None => None,
            }
        };
        if let Some(offset) = offset {
            lock(&self.state).source(is_mic).audio_offset = offset;
        }
        Ok(())
    }

    /// `_handle_mic_chunk`: the user's own speech, under the profile's name.
    ///
    /// # Errors
    /// The segment could not be appended.
    pub fn handle_mic_chunk(&self, text: &str, timestamp: &str, chunk_time: f64, chunk_gen: i64) -> Result<(), String> {
        let name = self.mic_speaker_name();
        let (is_continuation, audio_offset) = {
            let mut st = lock(&self.state);
            let s = st.source(true);
            let gap = chunk_time - s.last_speech_time;
            let duration = chunk_time - s.segment_start_time;
            let continuation = s.last_speaker.as_deref() == Some(name.as_str())
                && gap < SPEAKER_TURN_GAP
                && duration < MAX_SEGMENT_DURATION;
            s.last_speaker = Some(name.clone());
            s.last_speech_time = chunk_time;
            if !continuation {
                s.segment_start_time = chunk_time;
            }
            (continuation, s.audio_offset)
        };
        self.push_line(format!("\n**{name}** *[{timestamp}]*: {text}\n"));
        let mut segment_id = 0;
        if let Some(storage) = self.storage()
            && self.generation() == chunk_gen
        {
            segment_id = storage
                .append_segment(&NewSegment {
                    speaker: name.clone(),
                    speaker_id: "local".into(),
                    text: text.to_owned(),
                    is_local: true,
                    is_continuation,
                    uncertain: false,
                    uncertain_reason: String::new(),
                    audio_offset_ms: audio_offset,
                    captured_at: Some(chunk_time),
                })
                .map_err(storage_error)?;
        }
        if let Some(conference) = self.conference() {
            conference.send_transcript(&TranscriptLine {
                speaker: name,
                text: text.to_owned(),
                timestamp: timestamp.to_owned(),
                is_continuation,
                is_local: true,
                segment_id,
                ..TranscriptLine::default()
            });
        }
        Ok(())
    }

    /// `_handle_ktalk_chunk`: system audio attributed from the conference signals
    /// (dominant speaker, current speaker, the last speaker within its TTL, the sole
    /// unmuted remote participant, "Системный звук" with nobody remote, Unknown).
    ///
    /// # Errors
    /// The segment or the correction could not be written.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_ktalk_chunk(
        &self,
        text: &str,
        timestamp: &str,
        chunk_start_time: f64,
        chunk_end_time: f64,
        chunk_time: f64,
        chunk_gen: i64,
        fallback_mixed: bool,
    ) -> Result<(), String> {
        let conference = self.conference();
        let ktalk_speaker = self.ktalk_speaker(conference.as_ref(), chunk_start_time, chunk_end_time);
        let attribution = self.attribute_system_chunk(conference.as_ref(), ktalk_speaker, chunk_time);
        let speaker = attribution.speaker.clone();
        let (is_continuation, audio_offset) = self.system_segment_state(&attribution, chunk_time);
        let line_idx = self.push_line(format!("\n**{speaker}** *[{timestamp}]*: {text}\n"));
        let Some(conference) = conference else { return Ok(()) };
        let (uncertain, uncertain_reason) =
            uncertainty(attribution.no_speaker_yet, attribution.sole_unmuted_fallback, conference.are_all_mics_muted());
        let mut meeting_seg_id = 0;
        let storage = self.storage();
        if let Some(storage) = &storage
            && self.generation() == chunk_gen
        {
            meeting_seg_id = storage
                .append_segment(&NewSegment {
                    speaker: speaker.clone(),
                    speaker_id: speaker.clone(),
                    text: text.to_owned(),
                    is_local: false,
                    is_continuation,
                    uncertain,
                    uncertain_reason: persisted_reason(fallback_mixed, uncertain_reason),
                    audio_offset_ms: audio_offset,
                    captured_at: Some(chunk_time),
                })
                .map_err(storage_error)?;
        }
        conference.send_transcript(&TranscriptLine {
            speaker: speaker.clone(),
            text: text.to_owned(),
            timestamp: timestamp.to_owned(),
            is_continuation,
            is_local: false,
            uncertain,
            uncertain_reason: uncertain_reason.to_owned(),
            segment_id: meeting_seg_id,
            ..TranscriptLine::default()
        });
        let recent = RecentSegment {
            speaker: speaker.clone(),
            chunk_time,
            line_idx,
            meeting_segment_id: meeting_seg_id,
            is_local: false,
            timestamp_display: timestamp.to_owned(),
            text: text.to_owned(),
        };
        if let Some(snapshot) = self.remember_system_segment(recent, chunk_gen, storage.as_deref()) {
            self.retroactive_correct_unknown_run(&speaker, chunk_time, Some(snapshot))?;
        }
        Ok(())
    }

    /// The conference's speaker of the chunk's period (else its current speaker), or
    /// the last speaker the extension reported without a conference.
    fn ktalk_speaker(&self, conference: Option<&Arc<dyn Conference>>, start: f64, end: f64) -> Option<String> {
        match conference {
            Some(c) => c
                .get_dominant_speaker_for_period(start, end)
                .filter(|s| !s.is_empty())
                .or_else(|| c.get_current_speaker()),
            None => lock(&self.ktalk_current_speaker).clone(),
        }
    }

    /// The attribution chain of a system chunk: the conference's speaker, the last
    /// speaker within its TTL and unmuted, the sole unmuted remote participant,
    /// "Системный звук" with nobody remote, else Unknown.
    fn attribute_system_chunk(
        &self,
        conference: Option<&Arc<dyn Conference>>,
        ktalk_speaker: Option<String>,
        chunk_time: f64,
    ) -> Attribution {
        let found = |speaker: String| Attribution { speaker, no_speaker_yet: false, sole_unmuted_fallback: false };
        if let Some(speaker) = ktalk_speaker.filter(|s| !s.is_empty()) {
            return found(speaker);
        }
        if let Some(last) = self.recent_unmuted_last_speaker(conference, chunk_time) {
            return found(last);
        }
        if let Some(sole) = conference.and_then(|c| c.get_sole_unmuted_remote_participant()).filter(|s| !s.is_empty()) {
            return Attribution { speaker: sole, no_speaker_yet: false, sole_unmuted_fallback: true };
        }
        if conference.is_some_and(|c| !c.has_remote_participants()) {
            return found(SYSTEM_AUDIO_SPEAKER.to_owned());
        }
        Attribution { speaker: "Unknown".to_owned(), no_speaker_yet: true, sole_unmuted_fallback: false }
    }

    /// The system source's last speaker while within `LAST_SPEAKER_TTL` and not muted.
    fn recent_unmuted_last_speaker(&self, conference: Option<&Arc<dyn Conference>>, chunk_time: f64) -> Option<String> {
        let (last_speaker, last_speech_time) = {
            let mut st = lock(&self.state);
            let s = st.source(false);
            (s.last_speaker.clone(), s.last_speech_time)
        };
        last_speaker.filter(|l| {
            !l.is_empty()
                && chunk_time - last_speech_time < LAST_SPEAKER_TTL
                && !conference.is_some_and(|c| c.is_speaker_muted(l))
        })
    }

    /// Whether the chunk continues the system source's segment (same speaker, within
    /// the turn gap and the segment's length), the state moved on, and the audio offset.
    fn system_segment_state(&self, attribution: &Attribution, chunk_time: f64) -> (bool, i64) {
        let speaker = &attribution.speaker;
        let mut st = lock(&self.state);
        let s = st.source(false);
        let gap = chunk_time - s.last_speech_time;
        let duration = chunk_time - s.segment_start_time;
        let continuation = s.last_speaker.as_deref() == Some(speaker.as_str())
            && gap < SPEAKER_TURN_GAP
            && duration < MAX_SEGMENT_DURATION;
        if speaker != "Unknown" && speaker != SYSTEM_AUDIO_SPEAKER && !attribution.sole_unmuted_fallback {
            s.last_speaker = Some(speaker.clone());
        }
        s.last_speech_time = chunk_time;
        if !continuation {
            s.segment_start_time = chunk_time;
        }
        (continuation, s.audio_offset)
    }

    /// The segment kept for the retroactive correction (of this recording only); the
    /// snapshot to correct from when it identifies a speaker right after an Unknown.
    fn remember_system_segment(
        &self,
        recent: RecentSegment,
        chunk_gen: i64,
        storage: Option<&MeetingStorage>,
    ) -> Option<(Vec<RecentSegment>, i64, Option<PathBuf>)> {
        let mut st = lock(&self.state);
        if self.generation() != chunk_gen {
            return None;
        }
        let speaker = recent.speaker.clone();
        st.recent_system_segments.push_back(recent);
        while st.recent_system_segments.len() > RETROACTIVE_MAX_SEGMENTS {
            st.recent_system_segments.pop_front();
        }
        let len = st.recent_system_segments.len();
        let needs = speaker != "Unknown"
            && speaker != SYSTEM_AUDIO_SPEAKER
            && len >= 2
            && st.recent_system_segments[len - 2].speaker == "Unknown";
        needs.then(|| {
            (
                st.recent_system_segments.iter().cloned().collect::<Vec<_>>(),
                self.generation(),
                storage.and_then(MeetingStorage::current_meeting_path),
            )
        })
    }

    /// `_handle_unattributed_chunk`: system speech without conference identity.
    ///
    /// # Errors
    /// The segment could not be appended.
    pub fn handle_unattributed_chunk(
        &self,
        text: &str,
        timestamp: &str,
        chunk_time: f64,
        chunk_gen: i64,
    ) -> Result<(), String> {
        let (is_continuation, audio_offset) = {
            let mut st = lock(&self.state);
            let s = st.source(false);
            let gap = chunk_time - s.last_speech_time;
            let duration = chunk_time - s.segment_start_time;
            let continuation = s.last_speaker.as_deref() == Some("speaker")
                && gap < SPEAKER_TURN_GAP
                && duration < MAX_SEGMENT_DURATION;
            s.last_speaker = Some("speaker".into());
            s.last_speech_time = chunk_time;
            if !continuation {
                s.segment_start_time = chunk_time;
            }
            (continuation, s.audio_offset)
        };
        self.push_line(format!("\n**speaker** *[{timestamp}]*: {text}\n"));
        if let Some(storage) = self.storage()
            && self.generation() == chunk_gen
        {
            storage
                .append_segment(&NewSegment {
                    speaker: "speaker".into(),
                    speaker_id: "speaker".into(),
                    text: text.to_owned(),
                    is_local: false,
                    is_continuation,
                    uncertain: true,
                    uncertain_reason: "no_conference_identity".into(),
                    audio_offset_ms: audio_offset,
                    captured_at: Some(chunk_time),
                })
                .map_err(storage_error)?;
        }
        Ok(())
    }

    /// `_process_chunk`: one chunk of a source through WAV, the PCM-primary gate, the
    /// mute and VAD gates, ASR, dedup, the silence split and its handler.
    ///
    /// # Errors
    /// A WAV or storage write failed (fatal in Python).
    pub fn process_chunk(&self, is_mic: bool) -> Result<(), String> {
        let Some(capture) = self.capture_of(is_mic) else { return Ok(()) };
        if !self.asr.available() || !self.is_recording_active() {
            return Ok(());
        }
        let chunk_gen = self.generation();
        let Some(next) = self.next_chunk(capture, is_mic, chunk_gen) else { return Ok(()) };
        let rms = rms_f32(&next.chunk);
        let ingest = self.ingest();
        if !self.write_chunk_audio(is_mic, &next, rms, ingest.as_ref(), chunk_gen)? {
            return Ok(());
        }
        let Some(fallback_mixed) = self.pcm_transport_gate(is_mic, ingest.as_ref())? else { return Ok(()) };
        if !self.speech_wanted(is_mic, rms) {
            return Ok(());
        }
        let Some(text) = self.chunk_text(is_mic, &next, chunk_gen) else { return Ok(()) };
        self.handle_chunk_text(is_mic, &text, &next, chunk_gen, fallback_mixed)
    }

    fn capture_of(&self, is_mic: bool) -> Option<&Arc<dyn LiveCapture>> {
        if is_mic { self.mic_capture.as_ref() } else { self.system_capture.as_ref() }
    }

    /// The chunk ASR could not serve before (of this recording), else the capture's
    /// next one of at least 1000 samples, which the golden recorder records.
    fn next_chunk(&self, capture: &Arc<dyn LiveCapture>, is_mic: bool, chunk_gen: i64) -> Option<NextChunk> {
        let retry = lock(&self.state).source(is_mic).pending_chunk.take().filter(|(_, _, g)| *g == chunk_gen);
        if let Some((chunk, start_ts, _)) = retry {
            return Some(NextChunk { chunk, start_ts, epoch: lock(&self.wav).system_source_epoch, is_retry: true });
        }
        let (chunk, start_ts) = capture.get_chunk(self.opts.chunk_duration)?;
        if chunk.len() < 1000 {
            return None;
        }
        if let Some(golden) = self.golden_now() {
            golden.capture(if is_mic { "mic" } else { "system" }, Audio::F32(&chunk), start_ts);
        }
        Some(NextChunk { chunk, start_ts, epoch: capture.chunk_epoch(), is_retry: false })
    }

    /// The chunk into its WAV (unless it is a retry, or silent while the system audio
    /// is not the PCM path's timeline), its offset kept for the segment; false when the
    /// recording changed meanwhile.
    fn write_chunk_audio(
        &self,
        is_mic: bool,
        next: &NextChunk,
        rms: f64,
        ingest: Option<&Arc<PerSpeakerIngest>>,
        chunk_gen: i64,
    ) -> Result<bool, String> {
        let skip_wav_write = rms < VAD_RMS_THRESHOLD && !pcm_primary(is_mic, ingest);
        let mut wav = lock(&self.wav);
        if !self.is_current_recording(chunk_gen) {
            return Ok(false);
        }
        // A retried chunk is in the WAV already (with its offset and anchor); a
        // silent one makes no segment, so nothing reads its offset.
        let write = !(next.is_retry || skip_wav_write);
        let offset = if write { write_wav(&mut wav, is_mic, next)? } else { None };
        if let Some(offset) = offset {
            lock(&self.state).source(is_mic).audio_offset = offset;
        }
        Ok(true)
    }

    /// The per-speaker path's say over a system chunk: the transition marker when PCM
    /// degraded or recovered; None while PCM is primary (the chunk is not
    /// transcribed), else whether the chunk is the fallback mixed path.
    fn pcm_transport_gate(&self, is_mic: bool, ingest: Option<&Arc<PerSpeakerIngest>>) -> Result<Option<bool>, String> {
        if is_mic {
            return Ok(Some(false));
        }
        let Some(ingest) = ingest.filter(|i| i.pcm_ever_seen()) else { return Ok(Some(false)) };
        let degraded_now = ingest.degraded();
        let (should_emit, marker_gen) = {
            let mut st = lock(&self.state);
            let emit = degraded_now != st.last_pcm_degraded;
            if emit {
                st.last_pcm_degraded = degraded_now;
            }
            (emit, self.generation())
        };
        if should_emit {
            self.emit_pcm_transition_marker(degraded_now, marker_gen)?;
        }
        Ok(degraded_now.then_some(true))
    }

    /// Whether the chunk goes to ASR: not the muted microphone, not below the VAD
    /// threshold, recording still allowed.
    fn speech_wanted(&self, is_mic: bool, rms: f64) -> bool {
        if is_mic && self.conference().is_some_and(|c| c.is_local_mic_muted()) {
            return false;
        }
        if rms < VAD_RMS_THRESHOLD {
            return false;
        }
        self.is_recording_active()
    }

    /// The chunk's text after dedup against the source's last words, which it then
    /// replaces; None for no text or a changed recording. A chunk ASR could not serve
    /// is held for a retry.
    fn chunk_text(&self, is_mic: bool, next: &NextChunk, chunk_gen: i64) -> Option<String> {
        let text = match self.transcribe_direct(&next.chunk) {
            Ok(text) => text?,
            Err(_) => {
                self.hold_for_retry(is_mic, next, chunk_gen);
                return None;
            }
        };
        if text.is_empty() {
            return None;
        }
        let text = self.deduplicated(is_mic, &text, next.start_ts).filter(|t| !t.is_empty())?;
        if !self.is_current_recording(chunk_gen) {
            return None;
        }
        let mut st = lock(&self.state);
        let s = st.source(is_mic);
        s.last_words = last_words(&text, 5);
        s.last_words_capture_end = next.start_ts + self.opts.chunk_duration;
        Some(text)
    }

    fn hold_for_retry(&self, is_mic: bool, next: &NextChunk, chunk_gen: i64) {
        let mut st = lock(&self.state);
        if self.is_current_recording(chunk_gen) {
            st.source(is_mic).pending_chunk = Some((next.chunk.clone(), next.start_ts, chunk_gen));
        }
    }

    /// `text` without the words the source's last segment ended with, those words
    /// forgotten after a gap longer than `dedup_max_gap_s`.
    fn deduplicated(&self, is_mic: bool, text: &str, chunk_start_time: f64) -> Option<String> {
        let mut st = lock(&self.state);
        let s = st.source(is_mic);
        if chunk_start_time - s.last_words_capture_end > self.env.dedup_max_gap_s {
            s.last_words.clear();
        }
        deduplicate(text, &s.last_words)
    }

    /// A chunk's text through the silence split to its source's handler.
    fn handle_chunk_text(
        &self,
        is_mic: bool,
        text: &str,
        next: &NextChunk,
        chunk_gen: i64,
        fallback_mixed: bool,
    ) -> Result<(), String> {
        let chunk_start_time = next.start_ts;
        let chunk_end_time = next.start_ts + self.opts.chunk_duration;
        let current_time = chunk_end_time;
        self.check_silence_split(is_mic, current_time, Some(&next.chunk))?;
        let timestamp = self.hms(chunk_end_time);
        let chunk_time = current_time;
        if !lock(&self.transcript_lines).is_empty() {
            self.write_out("\n");
        }
        if is_mic {
            self.handle_mic_chunk(text, &timestamp, chunk_time, chunk_gen)
        } else if self.use_ktalk() {
            self.handle_ktalk_chunk(
                text,
                &timestamp,
                chunk_start_time,
                chunk_end_time,
                chunk_time,
                chunk_gen,
                fallback_mixed,
            )
        } else {
            self.handle_unattributed_chunk(text, &timestamp, chunk_time, chunk_gen)
        }
    }

    // ---- the per-speaker path ----

    /// `_on_pcm_batch`: one batched ASR call on the participants' PCM, the segments
    /// attributed by the chunk's own speaker.
    ///
    /// # Errors
    /// `TemporarilyUnavailable` for the ingest to hold the batch; anything else is
    /// logged here.
    pub fn on_pcm_batch(&self, batch: &[BatchedChunk]) -> Result<(), AsrError> {
        if batch.is_empty() {
            return Ok(());
        }
        let ingest = self.ingest();
        let chunk_gen = ingest.as_ref().and_then(|i| i.last_drain_generation()).unwrap_or_else(|| self.generation());
        if !self.is_current_recording(chunk_gen) {
            return Ok(());
        }
        let batch: Vec<&BatchedChunk> = batch.iter().filter(|item| !pcm_is_silent(&item.pcm)).collect();
        if batch.is_empty() {
            return Ok(());
        }
        let Some(texts) = self.batch_texts(&batch)? else { return Ok(()) };
        let current_time = self.env.clock.wall();
        let (anchor, wav_offset_now_ms) = {
            let wav = lock(&self.wav);
            (wav.system_anchor, wav.system.as_ref().map_or(0, IncrementalWavWriter::offset_ms))
        };
        if texts.len() != batch.len() {
            self.env.log.line(&format!(
                "[PerSpeaker] transcribe_batch returned {} text(s) for {} input(s) — dropping batch to avoid \
                 misattribution",
                texts.len(),
                batch.len()
            ));
            return Ok(());
        }
        let timeline = PcmTimeline { batch_now_ms: current_time * 1000.0, anchor, wav_offset_now_ms };
        for (item, raw_text) in batch.iter().zip(&texts) {
            self.emit_pcm_segment(item, raw_text, chunk_gen, current_time, &timeline, ingest.is_some())?;
        }
        Ok(())
    }

    /// One batched ASR call with refine's live priority raised around it, recorded by
    /// the golden recorder; None when it failed otherwise than temporarily (logged).
    fn batch_texts(&self, batch: &[&BatchedChunk]) -> Result<Option<Vec<String>>, AsrError> {
        let clips: Vec<&[i16]> = batch.iter().map(|item| item.pcm.as_slice()).collect();
        let refine = self.refine_worker();
        if let Some(worker) = &refine {
            worker.set_live_pending();
        }
        let result = self.asr.transcribe_batch(&clips);
        if let Some(worker) = &refine {
            worker.clear_live_pending();
        }
        match result {
            Ok(texts) => {
                if let Some(golden) = self.golden_now() {
                    let audio: Vec<Audio<'_>> = clips.iter().map(|c| Audio::I16(c)).collect();
                    golden.asr("transcribe_batch", &audio, AsrOutput::Texts(&texts));
                }
                Ok(Some(texts))
            }
            Err(AsrError::TemporarilyUnavailable) => Err(AsrError::TemporarilyUnavailable),
            Err(e) => {
                self.env.log.line(&format!("[PerSpeaker] transcribe_batch failed: {e}"));
                Ok(None)
            }
        }
    }

    /// One participant's text of a batch: deduplicated against its last words,
    /// attributed by its own name, stored and sent; nothing for an empty text or a
    /// changed recording.
    fn emit_pcm_segment(
        &self,
        item: &BatchedChunk,
        raw_text: &str,
        chunk_gen: i64,
        current_time: f64,
        timeline: &PcmTimeline,
        has_ingest: bool,
    ) -> Result<(), AsrError> {
        let text = recog_core::py::strip(raw_text);
        if text.is_empty() {
            return Ok(());
        }
        let mut st = lock(&self.state);
        if self.generation() != chunk_gen {
            return Ok(());
        }
        let prev_words = st.pcm_prev_words.get(&item.pid).cloned().unwrap_or_default();
        let Some(deduped) = deduplicate(text, &prev_words).filter(|t| !t.is_empty()) else { return Ok(()) };
        let speaker_label = pcm_speaker_label(&item.speaker_name, &item.pid);
        st.pcm_prev_words.insert(item.pid.clone(), last_words(&deduped, 5));
        let is_continuation = pid_continues(&mut st, &item.pid, current_time);
        let timestamp = self.hms(item.chunk_end_ms / 1000.0);
        let audio_offset_ms = timeline.audio_offset_ms(item.chunk_start_ms);
        let line = format!("\n**{speaker_label}** *[{timestamp}]*: {deduped}\n");
        self.push_line(line);
        let mut segment_id = 0;
        if let Some(storage) = self.storage() {
            segment_id = storage
                .append_segment(&NewSegment {
                    speaker: speaker_label.clone(),
                    speaker_id: item.pid.clone(),
                    text: deduped.clone(),
                    is_local: item.is_local,
                    is_continuation,
                    uncertain: false,
                    uncertain_reason: String::new(),
                    audio_offset_ms,
                    captured_at: Some(item.chunk_end_ms / 1000.0),
                })
                .map_err(|e| AsrError::Failed(e.to_string()))?;
            register_pcm_live_segment(
                &mut st,
                has_ingest,
                &item.pid,
                segment_id,
                item.pid_audio_ms_start,
                item.pid_audio_ms_end,
            );
            st.pcm_segment_texts.insert(segment_id, deduped.clone());
        }
        if let Some(conference) = self.conference() {
            conference.send_transcript(&TranscriptLine {
                speaker: speaker_label,
                text: deduped,
                timestamp,
                is_continuation,
                is_local: item.is_local,
                segment_id,
                source_id: item.pid.clone(),
                ..TranscriptLine::default()
            });
        }
        Ok(())
    }

    /// `_register_pcm_live_segment` (the caller holds the state lock in Python; here
    /// the method takes it).
    pub fn register_pcm_live_segment(&self, pid: &str, segment_id: i64, start_ms: f64, end_ms: f64) {
        let has_ingest = self.ingest().is_some();
        let mut st = lock(&self.state);
        register_pcm_live_segment(&mut st, has_ingest, pid, segment_id, start_ms, end_ms);
    }

    /// `_submit_refine`: a refine window goes to the worker only when it holds a live
    /// segment of its pid.
    pub fn submit_refine(&self, task: RefineTask) {
        let task_end_ms = task.offset_base_ms + PCM_REFINE_WINDOW_S * 1000.0;
        let has_live_segment = lock(&self.state)
            .pcm_live_segments
            .get(&task.pid)
            .is_some_and(|segs| segs.iter().any(|&(_, start, end)| end >= task.offset_base_ms && start <= task_end_ms));
        if has_live_segment && let Some(worker) = self.refine_worker() {
            worker.submit(task);
        }
    }

    /// `_apply_refine`: the refined words aligned to the live segments by their start,
    /// the transcript rewritten, the refined text published.
    ///
    /// # Errors
    /// The transcript rewrite failed.
    pub fn apply_refine(&self, result: &RefineResult) -> Result<(), String> {
        let task = &result.task;
        let rebased = rebased_words(result);
        let Some((target_meeting_path, updates)) = self.refine_updates(task, &rebased) else { return Ok(()) };
        if updates.is_empty() {
            return Ok(());
        }
        let Some(storage) = self.storage() else { return Ok(()) };
        let rows: Vec<SegmentUpdate> = updates
            .iter()
            .map(|(id, text)| SegmentUpdate { segment_id: *id, text: Some(text.clone()), ..SegmentUpdate::default() })
            .collect();
        storage.update_segments(&rows, Some(&target_meeting_path)).map_err(storage_error)?;
        if let Some(conference) = self.conference() {
            let refined: Vec<RefineUpdate> =
                updates.iter().map(|(id, text)| RefineUpdate { segment_id: *id, text: text.clone() }).collect();
            let meeting_id = target_meeting_path.file_name().map(|n| n.to_string_lossy().into_owned());
            conference.send_refine(&refined, meeting_id.as_deref());
        }
        self.remember_refined_texts(task, updates);
        Ok(())
    }

    /// The meeting the refine belongs to and the new text of each of its pid's live
    /// segments the window overlaps; None when the recording changed, refine is
    /// paused, or there is no meeting.
    fn refine_updates(&self, task: &RefineTask, rebased: &[(f64, String)]) -> Option<(PathBuf, Vec<(i64, String)>)> {
        let st = lock(&self.state);
        if task.meeting_generation != self.generation() {
            return None;
        }
        if self.refine_worker().is_some_and(|w| w.is_paused()) {
            return None;
        }
        let path = self.storage()?.current_meeting_path()?;
        let task_start_ms = task.offset_base_ms;
        let task_end_ms = task.offset_base_ms + PCM_REFINE_WINDOW_S * 1000.0;
        let mut updates = Vec::new();
        for &(segment_id, seg_start, seg_end) in st.pcm_live_segments.get(&task.pid).into_iter().flatten() {
            if seg_end < task_start_ms || seg_start > task_end_ms {
                continue;
            }
            let Some(new_text) = refined_text(rebased, seg_start, seg_end) else { continue };
            if st.pcm_segment_texts.get(&segment_id) == Some(&new_text) {
                continue;
            }
            updates.push((segment_id, new_text));
        }
        Some((path, updates))
    }

    /// The refined texts kept for the identical-text skip, for segments still live.
    fn remember_refined_texts(&self, task: &RefineTask, updates: Vec<(i64, String)>) {
        let mut st = lock(&self.state);
        if task.meeting_generation != self.generation() {
            return;
        }
        let live_ids: HashSet<i64> =
            st.pcm_live_segments.get(&task.pid).into_iter().flatten().map(|&(id, _, _)| id).collect();
        for (id, text) in updates {
            if live_ids.contains(&id) {
                st.pcm_segment_texts.insert(id, text);
            }
        }
    }

    /// `_teardown_session` without the bridge and storage ends, which
    /// [`crate::live::session`] runs in their place: captures, golden, refine, ingest,
    /// the WAV writers and their paths.
    pub fn stop_audio(&self) {
        if let Some(capture) = &self.mic_capture {
            capture.stop();
        }
        if let Some(capture) = &self.system_capture {
            capture.stop();
        }
        if let Some(golden) = self.golden() {
            golden.close();
        }
        if let Some(worker) = lock(&self.refine).take() {
            worker.stop(std::time::Duration::from_secs(2));
        }
        if let Some(ingest) = self.ingest() {
            ingest.stop(std::time::Duration::from_secs(2));
        }
    }

    /// The WAV part of `_teardown_session`: writers finalized, their paths stored.
    ///
    /// # Errors
    /// Finalizing or storing the paths failed.
    pub fn finalize_audio(&self) -> Result<(), String> {
        let (mic, system) = self.finalize_writers()?;
        if let Some(path) = &mic {
            self.print(&format!("Mic audio saved: {path}"));
        }
        if let Some(path) = &system {
            self.print(&format!("System audio saved: {path}"));
        }
        if let Some(storage) = self.storage()
            && (mic.is_some() || system.is_some())
        {
            storage.set_audio_paths(mic.as_deref(), system.as_deref(), None).map_err(storage_error)?;
        }
        Ok(())
    }

    /// Both writers finalized, the anchor dropped; the paths of the finalized WAVs.
    fn finalize_writers(&self) -> Result<(Option<String>, Option<String>), String> {
        let mut wav = lock(&self.wav);
        let mut mic = None;
        let mut system = None;
        if let Some(writer) = wav.mic.as_mut() {
            writer.finalize().map_err(|e| e.to_string())?;
            mic = Some(writer.path.display().to_string());
        }
        if let Some(writer) = wav.system.as_mut() {
            writer.finalize().map_err(|e| e.to_string())?;
            system = Some(writer.path.display().to_string());
        }
        wav.system_anchor = None;
        Ok((mic, system))
    }

    /// Prints a line on stdout (`print`).
    pub fn println(&self, text: &str) {
        self.print(text);
    }
}

fn register_pcm_live_segment(st: &mut State, has_ingest: bool, pid: &str, segment_id: i64, start_ms: f64, end_ms: f64) {
    if !has_ingest {
        return;
    }
    let entries = st.pcm_live_segments.entry(pid.to_owned()).or_default();
    entries.push((segment_id, start_ms, end_ms));
    let prune_floor = end_ms - (PCM_REFINE_WINDOW_S + 5.0) * 1000.0;
    let mut pruned = Vec::new();
    while entries.first().is_some_and(|e| e.2 < prune_floor) {
        pruned.push(entries.remove(0).0);
    }
    for id in pruned {
        st.pcm_segment_texts.remove(&id);
    }
}

/// Where a batch's segments land in the system WAV.
struct PcmTimeline {
    batch_now_ms: f64,
    /// (wall ms of the last system write's first sample, its WAV offset ms).
    anchor: Option<(f64, i64)>,
    wav_offset_now_ms: i64,
}

impl PcmTimeline {
    /// The WAV offset of a chunk captured at `chunk_start_ms`: from the anchor while
    /// it is fresh, else back from the WAV's end by the chunk's age.
    #[allow(clippy::cast_possible_truncation)]
    fn audio_offset_ms(&self, chunk_start_ms: f64) -> i64 {
        match self.anchor {
            Some((wall, wav_ms)) if self.batch_now_ms - wall <= PCM_WAV_ANCHOR_MAX_AGE_MS => {
                // int() truncates toward zero, as Python's does.
                ((wav_ms as f64) + (chunk_start_ms - wall)).trunc().max(0.0) as i64
            }
            _ => {
                let age = (self.batch_now_ms - chunk_start_ms).trunc() as i64;
                (self.wav_offset_now_ms - age).max(0)
            }
        }
    }
}

/// The speaker of a participant's segment: its name on one line, else
/// `Участник <pid prefix>`.
fn pcm_speaker_label(speaker_name: &str, pid: &str) -> String {
    let safe_name = speaker_name.replace(['\n', '\r'], " ");
    let safe_name = recog_core::py::strip(&safe_name);
    if safe_name.is_empty() {
        format!("Участник {}", pid.chars().take(6).collect::<String>())
    } else {
        safe_name.to_owned()
    }
}

/// Whether a participant's segment continues its last one (within the turn gap and
/// the segment's length); its state moved on to `current_time`.
fn pid_continues(st: &mut State, pid: &str, current_time: f64) -> bool {
    let pid_state = st
        .pcm_pid_state
        .entry(pid.to_owned())
        .or_insert(PidState { last_speech_time: 0.0, segment_start_time: current_time });
    let gap = current_time - pid_state.last_speech_time;
    let duration = current_time - pid_state.segment_start_time;
    let is_continuation = gap < SPEAKER_TURN_GAP && duration < MAX_SEGMENT_DURATION;
    pid_state.last_speech_time = current_time;
    if !is_continuation {
        pid_state.segment_start_time = current_time;
    }
    is_continuation
}

/// A refine's non-empty words, their starts moved onto the pid's audio timeline.
fn rebased_words(result: &RefineResult) -> Vec<(f64, String)> {
    result
        .words
        .iter()
        .filter_map(|w| {
            let text = recog_core::py::strip(&w.text);
            #[allow(clippy::cast_precision_loss)]
            (!text.is_empty()).then(|| (result.task.offset_base_ms + w.start as f64, text.to_owned()))
        })
        .collect()
}

/// The words starting inside a live segment, joined; None when there are none.
fn refined_text(rebased: &[(f64, String)], seg_start: f64, seg_end: f64) -> Option<String> {
    let collected: Vec<&str> = rebased
        .iter()
        .filter(|(w_start, _)| seg_start <= *w_start && *w_start < seg_end)
        .map(|(_, t)| t.as_str())
        .collect();
    if collected.is_empty() {
        return None;
    }
    let new_text = recog_core::py::strip(&collected.join(" ")).to_owned();
    (!new_text.is_empty()).then_some(new_text)
}

/// Who a system chunk is attributed to, and how sure.
struct Attribution {
    speaker: String,
    /// Nobody could be named: Unknown.
    no_speaker_yet: bool,
    /// The sole unmuted remote participant, a guess.
    sole_unmuted_fallback: bool,
}

/// `uncertain` and its reason for a conference's system segment.
fn uncertainty(no_speaker_yet: bool, sole_unmuted_fallback: bool, all_mics_muted: bool) -> (bool, &'static str) {
    if no_speaker_yet {
        (true, "no_speaker")
    } else if sole_unmuted_fallback {
        (true, "sole_unmuted_fallback")
    } else if all_mics_muted {
        (true, "mics_off")
    } else {
        (false, "")
    }
}

/// The stored reason: the fallback mixed path prefixes it.
fn persisted_reason(fallback_mixed: bool, uncertain_reason: &str) -> String {
    if !fallback_mixed {
        return uncertain_reason.to_owned();
    }
    if uncertain_reason.is_empty() { "fallback_mixed".to_owned() } else { format!("fallback_mixed/{uncertain_reason}") }
}

/// A chunk on its way through `_process_chunk`.
struct NextChunk {
    chunk: Vec<f32>,
    start_ts: f64,
    /// The capture's source epoch (the WAV's, for a retry).
    epoch: u64,
    /// The chunk ASR could not serve before: already in the WAV.
    is_retry: bool,
}

/// Whether the per-speaker PCM is the system audio's path, so its WAV must keep a
/// real-time timeline.
fn pcm_primary(is_mic: bool, ingest: Option<&Arc<PerSpeakerIngest>>) -> bool {
    !is_mic && ingest.is_some_and(|i| i.pcm_ever_seen() && !i.degraded())
}

/// The chunk into its source's WAV: the offset it starts at, None without a writer.
fn write_wav(wav: &mut Wav, is_mic: bool, next: &NextChunk) -> Result<Option<i64>, String> {
    if is_mic { write_mic_wav(wav, &next.chunk) } else { write_system_wav(wav, next) }
}

fn write_mic_wav(wav: &mut Wav, chunk: &[f32]) -> Result<Option<i64>, String> {
    let Some(writer) = wav.mic.as_mut() else { return Ok(None) };
    let offset = writer.offset_ms();
    writer.write_chunk(chunk).map_err(|e| e.to_string())?;
    Ok(Some(offset))
}

/// A system chunk into its WAV: a discontinuity marked when the capture's source
/// changed, the wall-clock anchor moved to its first written sample.
fn write_system_wav(wav: &mut Wav, next: &NextChunk) -> Result<Option<i64>, String> {
    if wav.system.is_none() {
        return Ok(None);
    }
    if next.epoch != wav.system_source_epoch {
        wav.system_source_epoch = next.epoch;
        if let Some(writer) = wav.system.as_mut() {
            writer.mark_discontinuity();
        }
    }
    let Some(writer) = wav.system.as_mut() else { return Ok(None) };
    let before = writer.offset_ms();
    let skipped = writer.write_chunk(&next.chunk).map_err(|e| e.to_string())?;
    if let Some(skipped) = skipped {
        #[allow(clippy::cast_precision_loss)]
        let first = (next.start_ts + skipped as f64 / f64::from(SAMPLE_RATE)) * 1000.0;
        wav.system_anchor = Some((first, before));
    }
    Ok(Some(before))
}

/// The Unknown run right before the last segment, newest first: remote segments
/// within `RETROACTIVE_MAX_SECONDS` of `current_time`.
fn unknown_run(segments: &[RecentSegment], current_time: f64) -> Vec<RecentSegment> {
    let mut run = Vec::new();
    let Some((_, before)) = segments.split_last() else { return run };
    for seg in before.iter().rev() {
        if seg.speaker != "Unknown" || seg.is_local || current_time - seg.chunk_time > RETROACTIVE_MAX_SECONDS {
            break;
        }
        run.push(seg.clone());
    }
    run
}

fn marker(msg: &str, reason: &str) -> NewSegment {
    NewSegment {
        speaker: "Система".into(),
        speaker_id: "__system__".into(),
        text: msg.to_owned(),
        is_local: false,
        is_continuation: false,
        uncertain: false,
        uncertain_reason: reason.to_owned(),
        audio_offset_ms: 0,
        captured_at: None,
    }
}

fn exception_name(e: &AsrError) -> &'static str {
    match e {
        AsrError::InvalidInput(_) => "ValueError",
        AsrError::TemporarilyUnavailable => "ASRTemporarilyUnavailable",
        AsrError::Aborted => "RefineAborted",
        AsrError::Failed(_) => "RuntimeError",
    }
}

fn read_identity(store: &UserProfileStore, legacy: &str, override_name: Option<&str>) -> Result<Value, String> {
    let profile = store.read().map_err(|e| e.to_string())?;
    Ok(resolve_identity(&profile, Some(legacy), override_name))
}

fn identity_name(identity: &Value) -> String {
    identity.as_object().and_then(|m| m.get("local_user_name")).and_then(Value::as_str).unwrap_or("Me").to_owned()
}
