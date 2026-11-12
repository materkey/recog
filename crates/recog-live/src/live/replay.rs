//! Replay of a golden session into the Rust live process, as `recog/golden_replay.py`
//! replays one into the Python one (docs/plans/2026-11-07-golden-sessions-design.md).
//!
//! The real transcriber runs with its real bridge, ingest, refine worker and storage,
//! in a data directory of its own (`<out>/data`) and on a free port, while
//!
//! - the microphone and system captures hand out the recorded chunks at their recorded
//!   times, their capture times moved to now;
//! - ASR answers from the recording, looked up by the exact input, and counts misses;
//! - the extension's frames reach the bridge over WebSockets at their recorded times,
//!   one connection per recorded client, so its clock sync moves them to now as well;
//! - nothing calls an LLM: no titles, the increments do nothing.
//!
//! It replays in real time, as the Python player does: the live process sleeps and
//! polls on the wall clock, and a faster clock would change what the bridge's and the
//! ingest's timers see. `<out>/replay.json` holds the summary Python writes.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use recog_bridge::BridgeConfig;
use recog_bridge::hooks::{BoxFuture, IncrementGenerator, PolicySource};
use recog_core::install_config::InstallConfig;
use recog_core::meeting_storage::{GeneratedTitle, StorageOptions, TitleGenerator, TitleRequest};
use recog_core::pyjson::{self, Map, Value};

use crate::asr::{AsrError, Transcription, Word};
use crate::clock::SharedClock;
use crate::golden::{Audio, AudioData, Event, audio_key, read_session};
use crate::live::parts::{LiveAsr, LiveCapture};
use crate::live::session::{self, SessionConfig};
use crate::live::transcriber::{Live, LiveEnv, LiveOptions, Out};
use crate::log::Logger;

/// How long the live process runs on after the last event, so the last chunk, batch
/// and refine get through.
pub const TAIL_S: f64 = 8.0;

/// A capture that hands out the recorded chunks, one per `get_chunk`.
///
/// `clear_buffer` leaves the queue alone, as the Python player's capture does: it
/// clears the buffer of `BufferedAudioCapture`, which the replay never fills.
#[derive(Default)]
pub struct ReplayCapture {
    chunks: Mutex<VecDeque<(Vec<f32>, f64)>>,
    running: AtomicBool,
}

impl ReplayCapture {
    pub fn feed(&self, chunk: Vec<f32>, start_ts: f64) {
        self.chunks.lock().unwrap_or_else(PoisonError::into_inner).push_back((chunk, start_ts));
    }
}

impl LiveCapture for ReplayCapture {
    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    fn start(&self) -> Result<(), String> {
        self.running.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
    fn get_chunk(&self, _duration: f64) -> Option<(Vec<f32>, f64)> {
        self.chunks.lock().unwrap_or_else(PoisonError::into_inner).pop_front()
    }
    fn clear_buffer(&self) {}
    fn chunk_epoch(&self) -> u64 {
        0
    }
}

/// Answers each ASR call from the recording by the exact input; counts misses.
pub struct ReplayAsr {
    answers: HashMap<String, Value>,
    misses: Mutex<Vec<String>>,
    model_lock: Mutex<()>,
}

impl ReplayAsr {
    /// The answers of a session's `asr` events.
    #[must_use]
    pub fn from_events(events: &[Event]) -> Self {
        let mut answers = HashMap::new();
        for event in events.iter().filter(|e| e.get("kind").and_then(Value::as_str) == Some("asr")) {
            let inputs: Vec<&str> = match event.get("inputs") {
                Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            let output = event.get("output").cloned().unwrap_or(Value::Null);
            if event.get("call").and_then(Value::as_str) == Some("transcribe_batch") {
                if let Value::Array(texts) = output {
                    for (key, text) in inputs.iter().zip(texts) {
                        answers.insert((*key).to_owned(), text);
                    }
                }
            } else if let Some(key) = inputs.first() {
                answers.insert((*key).to_owned(), output);
            }
        }
        Self { answers, misses: Mutex::new(Vec::new()), model_lock: Mutex::new(()) }
    }

    /// The calls no recorded answer matched, in order.
    #[must_use]
    pub fn misses(&self) -> Vec<String> {
        self.misses.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn answer(&self, audio: Audio<'_>, call: &str) -> Option<&Value> {
        let found = self.answers.get(&audio_key(audio));
        if found.is_none() {
            self.misses.lock().unwrap_or_else(PoisonError::into_inner).push(call.to_owned());
        }
        found
    }
}

fn text_of(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or("").to_owned()
}

impl LiveAsr for ReplayAsr {
    fn load_model(&self) -> Result<(), AsrError> {
        Ok(())
    }

    fn transcribe_chunk(&self, chunk: &[f32]) -> Result<Option<String>, AsrError> {
        let text = text_of(self.answer(Audio::F32(chunk), "transcribe_chunk"));
        Ok((!text.is_empty()).then_some(text))
    }

    fn transcribe_batch(&self, clips: &[&[i16]]) -> Result<Vec<String>, AsrError> {
        Ok(clips.iter().map(|clip| text_of(self.answer(Audio::I16(clip), "transcribe_batch"))).collect())
    }

    fn transcribe_with_timestamps(
        &self,
        pcm: &[i16],
        should_abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Transcription, AsrError> {
        let _guard = self.model_lock.lock().unwrap_or_else(PoisonError::into_inner);
        if should_abort.is_some_and(|abort| abort()) {
            return Err(AsrError::Aborted);
        }
        let Some(Value::Object(answer)) = self.answer(Audio::I16(pcm), "transcribe_with_timestamps") else {
            return Ok(Transcription::default());
        };
        Ok(Transcription { text: text_of(answer.get("text")), words: timed_words(answer.get("words")) })
    }
}

/// The recorded `words` of a refine: each `[text, start, end]`, others left out.
fn timed_words(words: Option<&Value>) -> Vec<Word> {
    match words {
        Some(Value::Array(words)) => words.iter().filter_map(timed_word).collect(),
        _ => Vec::new(),
    }
}

fn timed_word(word: &Value) -> Option<Word> {
    match word {
        Value::Array(parts) if parts.len() == 3 => {
            Some(Word { text: parts[0].as_str()?.to_owned(), start: whole_ms(&parts[1])?, end: whole_ms(&parts[2])? })
        }
        _ => None,
    }
}

/// A recorded time in ms as an integer; int() truncates a float toward zero.
fn whole_ms(value: &Value) -> Option<i64> {
    match value {
        Value::Int(n) => i64::try_from(*n).ok(),
        #[allow(clippy::cast_possible_truncation)]
        Value::Float(x) => Some(*x as i64),
        _ => None,
    }
}

/// The increments of a replay: switched off, as the Python player patches them.
struct NoIncrements;

impl IncrementGenerator for NoIncrements {
    fn generate_all(&self, _meeting_id: String, _meeting_path: PathBuf) -> BoxFuture<Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// The titles of a replay: none, silently, as the Python player patches them (with no
/// generator at all the storage would say it has none).
struct NoTitles;

impl TitleGenerator for NoTitles {
    fn generate(&self, _request: &TitleRequest) -> Result<Option<GeneratedTitle>, String> {
        Ok(None)
    }
}

/// What a replay needs besides the session.
pub struct ReplaySettings {
    /// The installation settings (`config.toml` and the environment); golden
    /// recording and echo cancellation are switched off for the replay.
    pub config: InstallConfig,
    /// `MIC_SPEAKER_NAME`.
    pub legacy_mic_name: String,
    /// A `user-profile.json` to replay with.
    pub profile: Option<PathBuf>,
    pub tail_s: f64,
    pub clock: SharedClock,
    pub zone: Arc<dyn recog_core::time::Clock>,
    pub log: Logger,
    pub out: Out,
    /// The Copilot policy; None reads the replay's own data directory.
    pub policy: Option<Arc<dyn PolicySource>>,
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn number(event: &Event, key: &str) -> f64 {
    match event.get(key) {
        Some(Value::Float(x)) => *x,
        #[allow(clippy::cast_precision_loss)]
        Some(Value::Int(n)) => *n as f64,
        _ => 0.0,
    }
}

fn client_of(event: &Event) -> i128 {
    match event.get("client") {
        Some(Value::Int(n)) => *n,
        _ => -1,
    }
}

type Sink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Message,
>;

struct Connection {
    sink: Sink,
    reader: tokio::task::JoinHandle<()>,
}

async fn connect(url: &str) -> Result<Connection, tokio_tungstenite::tungstenite::Error> {
    let (stream, _) = tokio_tungstenite::connect_async(url).await?;
    let (sink, mut incoming) = stream.split();
    // The bridge's broadcasts are read and dropped, so its sends never back up.
    let reader = tokio::spawn(async move { while let Some(Ok(_)) = incoming.next().await {} });
    Ok(Connection { sink, reader })
}

async fn close(mut connection: Connection) {
    let _ = connection.sink.close().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), &mut connection.reader).await;
    connection.reader.abort();
}

/// Until the replayed bridge accepts a WebSocket (a bare TCP probe would make it log a
/// failed handshake).
async fn wait_for_bridge(url: &str, timeout: Duration) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout(Duration::from_secs(1), tokio_tungstenite::connect_async(url)).await {
            Ok(Ok((mut stream, _))) => {
                let _ = stream.close(None).await;
                return Ok(());
            }
            _ if tokio::time::Instant::now() > deadline => return Err("the replayed bridge did not start".into()),
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

/// `_drive`: the recording into the running live process at its recorded times.
async fn drive(
    events: Vec<Event>,
    captures: HashMap<&'static str, Arc<ReplayCapture>>,
    port: u16,
    start: f64,
    tail_s: f64,
    clock: SharedClock,
) -> Result<(), String> {
    let url = format!("ws://127.0.0.1:{port}");
    let first_wall = number(&events[0], "wall");
    let first_t = number(&events[0], "t");
    let shift = start - first_wall;
    let mut clients: HashMap<i128, Connection> = HashMap::new();
    wait_for_bridge(&url, Duration::from_secs(30)).await?;
    for event in events {
        let delay = start + number(&event, "t") - first_t - clock.wall();
        if delay > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(delay)).await;
        }
        play(&event, &mut clients, &captures, &url, shift).await?;
    }
    tokio::time::sleep(Duration::from_secs_f64(tail_s.max(0.0))).await;
    for (_, connection) in clients.drain() {
        close(connection).await;
    }
    Ok(())
}

/// One recorded event: a client connects or leaves, sends its frame, or a capture
/// gets its chunk (its capture time moved by `shift`).
async fn play(
    event: &Event,
    clients: &mut HashMap<i128, Connection>,
    captures: &HashMap<&'static str, Arc<ReplayCapture>>,
    url: &str,
    shift: f64,
) -> Result<(), String> {
    match event.get("kind").and_then(Value::as_str) {
        Some("connect") => {
            let connection = connect(url).await.map_err(|e| e.to_string())?;
            clients.insert(client_of(event), connection);
        }
        Some("disconnect") => {
            if let Some(connection) = clients.remove(&client_of(event)) {
                close(connection).await;
            }
        }
        Some("frame") => send_frame(event, clients).await,
        Some("capture") => feed_capture(event, captures, shift),
        _ => {}
    }
    Ok(())
}

async fn send_frame(event: &Event, clients: &mut HashMap<i128, Connection>) {
    if let Some(connection) = clients.get_mut(&client_of(event)) {
        let data = event.get("data").cloned().unwrap_or(Value::Null);
        let text = pyjson::dumps(&data, None);
        let _ = connection.sink.send(tokio_tungstenite::tungstenite::Message::text(text)).await;
    }
}

fn feed_capture(event: &Event, captures: &HashMap<&'static str, Arc<ReplayCapture>>, shift: f64) {
    let source = event.get("source").and_then(Value::as_str).unwrap_or("");
    if let (Some(capture), Some(AudioData::F32(samples))) = (captures.get(source), event.audio.clone()) {
        capture.feed(samples, number(event, "start_ts") + shift);
    }
}

/// `replay(session, out, profile)`: the session into a live process whose data goes
/// to `<out>/data`; returns the summary written to `<out>/replay.json`.
///
/// # Errors
/// The session is unreadable or empty, the directories cannot be made, or the live
/// process could not start.
pub fn replay(session: &Path, out: &Path, settings: ReplaySettings) -> Result<Value, String> {
    let events = read_session(session).map_err(|e| format!("{}: {e}", session.display()))?;
    if events.is_empty() {
        return Err(format!("{} holds no events", session.display()));
    }
    let data_dir = prepare_data_dir(out, settings.profile.as_deref())?;
    let mic = Arc::new(ReplayCapture::default());
    let system = Arc::new(ReplayCapture::default());
    let asr = Arc::new(ReplayAsr::from_events(&events));
    let port = free_port().map_err(|e| e.to_string())?;
    let live = replay_live(&settings.config, &data_dir, &settings, asr.clone(), &mic, &system)?;
    let session_config = replay_session_config(&settings.config, &data_dir, out, port, &settings);
    let start = settings.clock.wall();
    let recorded_start_wall = number(&events[0], "wall");
    let event_count = events.len();
    let captures: HashMap<&'static str, Arc<ReplayCapture>> = [("mic", mic), ("system", system)].into();
    let driver = spawn_driver(events, captures, port, start, &live, &settings)?;
    session::run(&live, session_config)?;
    let _ = driver.join();
    let mut summary = Map::new();
    summary.insert("session", Value::from(session.display().to_string()));
    summary.insert("start_wall", Value::Float(start));
    summary.insert("recorded_start_wall", Value::Float(recorded_start_wall));
    summary.insert("events", Value::Int(event_count as i128));
    summary.insert("asr_misses", Value::Int(asr.misses().len() as i128));
    let summary = Value::Object(summary);
    std::fs::write(out.join("replay.json"), pyjson::dumps(&summary, Some(1))).map_err(|e| e.to_string())?;
    Ok(summary)
}

/// `<out>/data`, with the profile to replay with copied in when it exists.
fn prepare_data_dir(out: &Path, profile: Option<&Path>) -> Result<PathBuf, String> {
    let data_dir = out.join("data");
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    if let Some(profile) = profile.filter(|p| p.exists()) {
        let name = profile.file_name().unwrap_or_default();
        std::fs::copy(profile, data_dir.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(data_dir)
}

/// The transcriber of a replay: the recorded captures and ASR, the conference on, no
/// audio saved.
fn replay_live(
    config: &InstallConfig,
    data_dir: &Path,
    settings: &ReplaySettings,
    asr: Arc<ReplayAsr>,
    mic: &Arc<ReplayCapture>,
    system: &Arc<ReplayCapture>,
) -> Result<Arc<Live>, String> {
    let backend = if crate::capture::backend::current_platform() == "darwin" { "screencapture" } else { "pulse" };
    let env = LiveEnv {
        data_dir: data_dir.to_path_buf(),
        dedup_max_gap_s: config.live.dedup_max_gap_s,
        legacy_mic_name: settings.legacy_mic_name.clone(),
        clock: settings.clock.clone(),
        zone: settings.zone.clone(),
        log: settings.log.clone(),
        out: settings.out.clone(),
    };
    let options = LiveOptions {
        system_audio: backend.into(),
        mic_display: "replay".into(),
        use_ktalk: true,
        save_audio: false,
        ..LiveOptions::default()
    };
    Live::new(
        options,
        env,
        asr,
        Some(mic.clone() as Arc<dyn LiveCapture>),
        Some(system.clone() as Arc<dyn LiveCapture>),
    )
}

/// The session of a replay: the bridge on `port` with its debug file in `out`, no
/// titles, no increments, no golden recording.
fn replay_session_config(
    config: &InstallConfig,
    data_dir: &Path,
    out: &Path,
    port: u16,
    settings: &ReplaySettings,
) -> SessionConfig {
    let mut storage = StorageOptions::new(&home_dir());
    storage.source_aliases = config.source_aliases.clone();
    storage.title_generator = Some(Arc::new(NoTitles));
    SessionConfig {
        bridge: BridgeConfig {
            port,
            extra_origins: recog_core::ws_origin::extra_origins(config),
            debug_file: Some(out.join("ktalk_debug.json")),
            legacy_mic_name: settings.legacy_mic_name.clone(),
            ..BridgeConfig::default()
        },
        policy: settings
            .policy
            .clone()
            .unwrap_or_else(|| Arc::new(recog_bridge::ai::AiSettingsPolicy::new(data_dir)) as Arc<dyn PolicySource>),
        increments: Some(Box::new(|_: &Path| Arc::new(NoIncrements) as Arc<dyn IncrementGenerator>)),
        storage,
        golden_record: false,
    }
}

/// The thread that drives the recording into the live process and ends its main loop
/// once the recording and the tail are over.
fn spawn_driver(
    events: Vec<Event>,
    captures: HashMap<&'static str, Arc<ReplayCapture>>,
    port: u16,
    start: f64,
    live: &Arc<Live>,
    settings: &ReplaySettings,
) -> Result<std::thread::JoinHandle<()>, String> {
    let driver_live = Arc::downgrade(live);
    let clock = settings.clock.clone();
    let log = settings.log.clone();
    let tail = settings.tail_s;
    std::thread::Builder::new()
        .name("golden-replay".into())
        .spawn(move || {
            if let Err(e) = run_driver(events, captures, port, start, tail, clock) {
                log.line(&format!("[golden] replay driver failed: {e}"));
            }
            if let Some(live) = driver_live.upgrade() {
                live.set_running(false);
            }
        })
        .map_err(|e| e.to_string())
}

fn run_driver(
    events: Vec<Event>,
    captures: HashMap<&'static str, Arc<ReplayCapture>>,
    port: u16,
    start: f64,
    tail: f64,
    clock: SharedClock,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    runtime.block_on(drive(events, captures, port, start, tail, clock))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}
