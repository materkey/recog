//! Fakes of the main loop's parts for tests/live*.rs: captures that hand out given
//! chunks, an ASR that answers from a script, a conference whose signals a test sets,
//! and a transcriber in a temp data directory with its stdout and stderr captured.
#![allow(dead_code)]

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use recog_bridge::{RefineUpdate, TranscriptLine};
use recog_core::meeting_storage::{MeetingStorage, StorageOptions};
use recog_core::pyjson::Value;
use recog_live::asr::{AsrError, Transcription};
use recog_live::clock::ManualClock;
use recog_live::live::{Conference, Live, LiveAsr, LiveCapture, LiveEnv, LiveOptions, Out};
use recog_live::log::{Captured, Logger};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
pub struct FakeCapture {
    pub chunks: Mutex<VecDeque<(Vec<f32>, f64)>>,
    pub running: AtomicBool,
    pub epoch: AtomicU64,
    pub reads: AtomicUsize,
    pub clears: AtomicUsize,
    pub starts: AtomicUsize,
    pub stops: AtomicUsize,
    pub fail_start: AtomicBool,
    pub dead: AtomicBool,
    pub silence: Mutex<Option<f64>>,
    pub restarts: Mutex<Vec<String>>,
    pub reopens: AtomicUsize,
}

impl FakeCapture {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn push(&self, chunk: Vec<f32>, start_ts: f64) {
        lock(&self.chunks).push_back((chunk, start_ts));
    }
}

impl LiveCapture for FakeCapture {
    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    fn start(&self) -> Result<(), String> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        if self.fail_start.load(Ordering::SeqCst) {
            return Err("device busy".into());
        }
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
    }
    fn get_chunk(&self, _duration: f64) -> Option<(Vec<f32>, f64)> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        lock(&self.chunks).pop_front()
    }
    fn clear_buffer(&self) {
        self.clears.fetch_add(1, Ordering::SeqCst);
        lock(&self.chunks).clear();
    }
    fn chunk_epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
    fn dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }
    fn digital_silence_s(&self) -> Option<f64> {
        *lock(&self.silence)
    }
    fn request_restart(&self, reason: &str) {
        lock(&self.restarts).push(reason.to_owned());
    }
    fn reopen_if_broken(&self) -> bool {
        self.reopens.fetch_add(1, Ordering::SeqCst);
        false
    }
}

/// ASR answering from scripts; an empty script answers with `default_text`.
pub struct FakeAsr {
    pub chunk_answers: Mutex<VecDeque<Result<Option<String>, AsrError>>>,
    pub default_text: Mutex<Option<String>>,
    pub batch_answers: Mutex<VecDeque<Result<Vec<String>, AsrError>>>,
    pub refine_answer: Mutex<Transcription>,
    pub available: AtomicBool,
    pub chunk_calls: AtomicUsize,
    pub batch_calls: Mutex<Vec<usize>>,
    pub refine_calls: AtomicUsize,
    /// What a callback saw of refine's live priority flag while ASR ran.
    pub on_call: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl FakeAsr {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            chunk_answers: Mutex::new(VecDeque::new()),
            default_text: Mutex::new(None),
            batch_answers: Mutex::new(VecDeque::new()),
            refine_answer: Mutex::new(Transcription::default()),
            available: AtomicBool::new(true),
            chunk_calls: AtomicUsize::new(0),
            batch_calls: Mutex::new(Vec::new()),
            refine_calls: AtomicUsize::new(0),
            on_call: Mutex::new(None),
        })
    }

    pub fn says(&self, text: &str) {
        lock(&self.chunk_answers).push_back(Ok(Some(text.to_owned())));
    }

    pub fn fails(&self, error: AsrError) {
        lock(&self.chunk_answers).push_back(Err(error));
    }

    pub fn batch(&self, texts: &[&str]) {
        lock(&self.batch_answers).push_back(Ok(texts.iter().map(|t| (*t).to_owned()).collect()));
    }
}

impl LiveAsr for FakeAsr {
    fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }
    fn load_model(&self) -> Result<(), AsrError> {
        Ok(())
    }
    fn transcribe_chunk(&self, _chunk: &[f32]) -> Result<Option<String>, AsrError> {
        self.chunk_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(f) = lock(&self.on_call).as_ref() {
            f();
        }
        lock(&self.chunk_answers).pop_front().unwrap_or_else(|| Ok(lock(&self.default_text).clone()))
    }
    fn transcribe_batch(&self, clips: &[&[i16]]) -> Result<Vec<String>, AsrError> {
        lock(&self.batch_calls).push(clips.len());
        if let Some(f) = lock(&self.on_call).as_ref() {
            f();
        }
        lock(&self.batch_answers).pop_front().unwrap_or_else(|| Ok(vec!["текст".into(); clips.len()]))
    }
    fn transcribe_with_timestamps(
        &self,
        _pcm: &[i16],
        _should_abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Transcription, AsrError> {
        self.refine_calls.fetch_add(1, Ordering::SeqCst);
        Ok(lock(&self.refine_answer).clone())
    }
}

/// A conference whose answers the test sets, recording what it is sent.
#[derive(Default)]
pub struct FakeConference {
    pub dominant: Mutex<Option<String>>,
    pub current: Mutex<Option<String>>,
    pub muted: Mutex<HashSet<String>>,
    pub sole_unmuted: Mutex<Option<String>>,
    pub has_remote: AtomicBool,
    pub all_muted: AtomicBool,
    pub local_muted: AtomicBool,
    pub active: AtomicBool,
    pub sent: Mutex<Vec<TranscriptLine>>,
    pub refines: Mutex<Vec<(Vec<RefineUpdate>, Option<String>)>>,
    pub dominant_queries: AtomicUsize,
}

impl FakeConference {
    pub fn new() -> Arc<Self> {
        let c = Self::default();
        c.has_remote.store(true, Ordering::SeqCst);
        Arc::new(c)
    }

    pub fn sent(&self) -> Vec<TranscriptLine> {
        lock(&self.sent).clone()
    }
}

impl Conference for FakeConference {
    fn get_dominant_speaker_for_period(&self, _start: f64, _end: f64) -> Option<String> {
        self.dominant_queries.fetch_add(1, Ordering::SeqCst);
        lock(&self.dominant).clone()
    }
    fn get_current_speaker(&self) -> Option<String> {
        lock(&self.current).clone()
    }
    fn is_speaker_muted(&self, name: &str) -> bool {
        lock(&self.muted).contains(name)
    }
    fn get_sole_unmuted_remote_participant(&self) -> Option<String> {
        lock(&self.sole_unmuted).clone()
    }
    fn has_remote_participants(&self) -> bool {
        self.has_remote.load(Ordering::SeqCst)
    }
    fn are_all_mics_muted(&self) -> bool {
        self.all_muted.load(Ordering::SeqCst)
    }
    fn is_local_mic_muted(&self) -> bool {
        self.local_muted.load(Ordering::SeqCst)
    }
    fn is_conference_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
    fn send_transcript(&self, line: &TranscriptLine) {
        lock(&self.sent).push(line.clone());
    }
    fn send_refine(&self, updates: &[RefineUpdate], meeting_id: Option<&str>) {
        lock(&self.refines).push((updates.to_vec(), meeting_id.map(str::to_owned)));
    }
}

/// The UTC zone, so `HH:MM:SS` is the same on every host.
pub struct Utc;

impl recog_core::time::Clock for Utc {
    fn now_micros(&self) -> i64 {
        0
    }
    fn utc_offset(&self, _utc_micros: i64) -> i64 {
        0
    }
}

/// A transcriber over fakes in a temp data directory.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub live: Arc<Live>,
    pub asr: Arc<FakeAsr>,
    pub mic: Option<Arc<FakeCapture>>,
    pub system: Option<Arc<FakeCapture>>,
    pub clock: ManualClock,
    pub stdout: Arc<Mutex<String>>,
    pub stderr: Captured,
}

pub const T0: f64 = 1_790_000_000.0;

impl Rig {
    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    pub fn stdout(&self) -> String {
        lock(&self.stdout).clone()
    }

    /// The storage, its first session started.
    pub fn storage(&self) -> Arc<MeetingStorage> {
        if let Some(storage) = self.live.storage() {
            return storage;
        }
        let mut options = StorageOptions::new(self.dir.path());
        options.log = Arc::new(|_, _| {});
        let storage = Arc::new(MeetingStorage::new(&self.data_dir().join("meetings"), options).unwrap());
        self.live.start_storage_session(storage.clone()).unwrap();
        storage
    }

    pub fn rows(&self) -> Vec<Value> {
        self.storage().get_current_transcript().unwrap()
    }

    pub fn conference(&self) -> Arc<FakeConference> {
        let conference = FakeConference::new();
        self.live.set_conference(Some(conference.clone() as Arc<dyn Conference>));
        conference
    }
}

pub struct RigOptions {
    pub mic: bool,
    pub system: bool,
    pub options: LiveOptions,
    pub legacy_name: String,
}

impl Default for RigOptions {
    fn default() -> Self {
        Self { mic: true, system: true, options: LiveOptions::default(), legacy_name: "Me".into() }
    }
}

pub fn rig(opts: RigOptions) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(T0);
    let (log, stderr) = Logger::capture(clock.shared());
    let stdout = Arc::new(Mutex::new(String::new()));
    let sink = stdout.clone();
    let out: Out = Arc::new(move |text: &str| lock(&sink).push_str(text));
    let env = LiveEnv {
        data_dir: dir.path().join("data"),
        dedup_max_gap_s: 2.0,
        legacy_mic_name: opts.legacy_name.clone(),
        clock: clock.shared(),
        zone: Arc::new(Utc),
        log,
        out,
    };
    let asr = FakeAsr::new();
    let mic = opts.mic.then(FakeCapture::new);
    let system = opts.system.then(FakeCapture::new);
    let live = Live::new(
        opts.options,
        env,
        asr.clone(),
        mic.clone().map(|c| c as Arc<dyn LiveCapture>),
        system.clone().map(|c| c as Arc<dyn LiveCapture>),
    )
    .unwrap();
    live.reset_source_states();
    Rig { dir, live, asr, mic, system, clock, stdout, stderr }
}

/// Speech-loud float samples (RMS well above the VAD threshold).
pub fn speech(seconds: f64) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (seconds * 16000.0) as usize;
    (0..n).map(|i| if i % 2 == 0 { 0.1 } else { -0.1 }).collect()
}

/// Silence.
pub fn silence(seconds: f64) -> Vec<f32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (seconds * 16000.0) as usize;
    vec![0.0; n]
}

pub fn field<'a>(row: &'a Value, key: &str) -> &'a Value {
    row.as_object().and_then(|m| m.get(key)).unwrap_or(&Value::Null)
}

pub fn text(row: &Value, key: &str) -> String {
    field(row, key).as_str().unwrap_or("").to_owned()
}
