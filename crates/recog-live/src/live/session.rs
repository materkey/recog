//! `LiveTranscriber.run` and its setup steps: golden recording, the meeting storage,
//! the bridge (in this process, on its own runtime thread) wired to the transcriber,
//! the model, the per-speaker ingest and refine worker, the main loop, and the
//! teardown.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use recog_ai::install::LegacyEnv;
use recog_ai::settings::AiSettingsStore;
use recog_bridge::hooks::{
    GoldenTap, IdentitySource, IncrementGenerator, LiveEvents, MeetingCapture, PcmChunk as BridgePcmChunk, PolicySource,
};
use recog_bridge::{Bridge, BridgeConfig};
use recog_core::meeting_storage::{GeneratedTitle, MeetingStorage, StorageOptions, TitleGenerator, TitleRequest};
use recog_core::pyjson::{Map, Value};

use crate::golden::{GoldenRecorder, remove_old_sessions};
use crate::live::parts::Conference;
use crate::live::transcriber::Live;
use crate::live::{AUDIO_CLEANUP_INTERVAL_S, GOLDEN_KEEP_DAYS};
use crate::per_speaker::{IngestOptions, IngestStats, PcmChunk, PerSpeakerIngest, TranscribeCallback};
use crate::refine::{AbortMode, GenerationGetter, RefineOptions, RefineWorker, new_model_lock};

/// Makes the increment generator of a meetings directory.
pub type IncrementsFactory = Box<dyn Fn(&Path) -> Arc<dyn IncrementGenerator> + Send>;

/// How a session is wired beyond the transcriber.
pub struct SessionConfig {
    pub bridge: BridgeConfig,
    /// The Copilot policy (`AiSettingsPolicy` of the data directory in the service).
    pub policy: Arc<dyn PolicySource>,
    /// The increment generator once the storage exists; None runs without one.
    pub increments: Option<IncrementsFactory>,
    pub storage: StorageOptions,
    /// `[live] golden_record`.
    pub golden_record: bool,
}

/// The bridge's view of the transcriber: its callbacks, the capture switch of
/// `ktalk_only` and the user identity.
pub struct LiveHooks(pub Weak<Live>);

impl LiveEvents for LiveHooks {
    fn speaker_changed(&self, speaker: Option<&str>, is_speaking: bool) {
        if let Some(live) = self.0.upgrade() {
            live.ktalk_speaker_changed(speaker, is_speaking);
        }
    }

    fn before_meeting_switch(&self) {
        if let Some(live) = self.0.upgrade() {
            live.before_meeting_switch();
        }
    }

    fn meeting_changed(&self, meeting_id: Option<&str>, meeting_path: Option<&Path>) -> Result<(), String> {
        let Some(live) = self.0.upgrade() else { return Ok(()) };
        match meeting_path {
            Some(path) => live.on_meeting_change(meeting_id.unwrap_or(""), path, false),
            None => live.close_audio_writers(),
        }
    }

    fn pcm_chunk(&self, chunk: BridgePcmChunk) -> Result<(), String> {
        let Some(ingest) = self.0.upgrade().and_then(|live| live.ingest()) else { return Ok(()) };
        ingest.enqueue(PcmChunk {
            pid: chunk.pid,
            speaker_name: chunk.speaker_name,
            is_local: chunk.is_local,
            seq: i64::try_from(chunk.seq).unwrap_or(i64::MAX),
            chunk_start_ts: chunk.chunk_start_ts,
            chunk_end_ts: chunk.chunk_end_ts,
            sample_rate: u32::try_from(chunk.sample_rate).unwrap_or(0),
            pcm: chunk.pcm,
        });
        Ok(())
    }

    fn wants_pcm(&self) -> bool {
        self.0.upgrade().is_some_and(|live| live.ingest().is_some())
    }

    fn pcm_stats(&self) -> Option<Result<Map, String>> {
        let ingest = self.0.upgrade()?.ingest()?;
        Some(Ok(stats_map(&ingest.get_stats())))
    }
}

impl MeetingCapture for LiveHooks {
    fn set_capture(&self, active: bool) -> Result<(), String> {
        match self.0.upgrade() {
            Some(live) => live.set_conference_capture(active),
            None => Ok(()),
        }
    }

    fn recording_active(&self) -> bool {
        self.0.upgrade().is_some_and(|live| live.is_recording_active())
    }
}

impl IdentitySource for LiveHooks {
    fn read_identity(&self) -> Result<Value, String> {
        self.0.upgrade().ok_or_else(|| "the live process is gone".to_owned())?.read_user_identity()
    }

    fn apply_mic_identity(&self, identity: &Value) -> Result<(), String> {
        if let Some(live) = self.0.upgrade() {
            live.apply_user_identity(identity);
        }
        Ok(())
    }
}

/// The golden recorder's bridge taps.
pub struct GoldenBridgeTap(pub Arc<GoldenRecorder>);

impl GoldenTap for GoldenBridgeTap {
    fn client_connected(&self, client: u64) {
        self.0.client_connected(i64::try_from(client).unwrap_or(i64::MAX));
    }
    fn frame(&self, client: u64, data: &Value) {
        self.0.frame(i64::try_from(client).unwrap_or(i64::MAX), data);
    }
    fn client_disconnected(&self, client: u64) {
        self.0.client_disconnected(i64::try_from(client).unwrap_or(i64::MAX));
    }
}

fn float(x: f64) -> Value {
    Value::Float(x)
}

fn opt_float(x: Option<f64>) -> Value {
    x.map_or(Value::Null, Value::Float)
}

/// `PerSpeakerIngest.get_stats()` as the dict the bridge's debug file holds.
#[must_use]
pub fn stats_map(stats: &IngestStats) -> Map {
    let mut per_pid = Map::new();
    for (pid, s) in &stats.per_pid {
        let mut m = Map::new();
        m.insert("speaker_name", Value::from(s.speaker_name.as_str()));
        m.insert("is_local", Value::Bool(s.is_local));
        m.insert("chunks_seen", Value::Int(i128::from(s.chunks_seen)));
        m.insert("last_seq", s.last_seq.map_or(Value::Null, |v| Value::Int(i128::from(v))));
        m.insert("gaps_count", Value::Int(i128::from(s.gaps_count)));
        m.insert("out_of_order_count", Value::Int(i128::from(s.out_of_order_count)));
        m.insert("first_seen_ts", float(s.first_seen_ts));
        m.insert("last_seen_ts", float(s.last_seen_ts));
        m.insert("age_s", float(s.age_s));
        m.insert("since_last_chunk_s", float(s.since_last_chunk_s));
        m.insert("chunks_per_sec", float(s.chunks_per_sec));
        per_pid.insert(pid.clone(), Value::Object(m));
    }
    let mut sizes = Map::new();
    for (pid, size) in &stats.pending_buffer_sizes {
        sizes.insert(pid.clone(), Value::Int(*size as i128));
    }
    let mut map = Map::new();
    map.insert("pcm_ever_seen", Value::Bool(stats.pcm_ever_seen));
    map.insert("degraded", Value::Bool(stats.degraded));
    map.insert("meeting_active_since", opt_float(stats.meeting_active_since));
    map.insert("last_chunk_ts", opt_float(stats.last_chunk_ts));
    map.insert("now", float(stats.now));
    map.insert("pending_buffer_sizes", Value::Object(sizes));
    map.insert("per_pid", Value::Object(per_pid));
    map
}

/// `_start_golden_recording`: a session directory under `<data dir>/golden`, older
/// sessions removed first.
///
/// # Errors
/// The old sessions or the new directory could not be handled.
pub fn start_golden_recording(live: &Live, enabled: bool) -> Result<Option<Arc<GoldenRecorder>>, String> {
    if !enabled {
        return Ok(None);
    }
    let root = live.env.data_dir.join("golden");
    let removed = remove_old_sessions(&root, GOLDEN_KEEP_DAYS, None).map_err(|e| e.to_string())?;
    for directory in removed {
        let name = directory.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        live.env.log.line(&format!("[golden] Removed {name}, older than {GOLDEN_KEEP_DAYS} days"));
    }
    #[allow(clippy::cast_possible_truncation)]
    let now = recog_core::time::from_timestamp(live.env.clock.wall());
    let wall = now.local_wall(&*live.env.zone);
    let directory = root.join(recog_core::time::format_wall(wall, "%Y-%m-%d_%H%M%S"));
    live.env.log.line(&format!("[golden] Recording the session into {}", directory.display()));
    let recorder = GoldenRecorder::new(&directory).map_err(|e| e.to_string())?;
    Ok(Some(Arc::new(recorder)))
}

/// `_setup_meeting_storage`.
///
/// # Errors
/// The storage could not start (`[FATAL ERROR] MeetingStorage init failed: …`).
pub fn setup_meeting_storage(live: &Live, options: StorageOptions) -> Result<Arc<MeetingStorage>, String> {
    let attempt = MeetingStorage::new(&live.env.data_dir.join("meetings"), options)
        .map_err(|e| e.to_string())
        .map(Arc::new)
        .and_then(|storage| live.start_storage_session(storage.clone()).map(|()| storage));
    attempt.map_err(|e| {
        live.env.log.line(&format!("[FATAL ERROR] MeetingStorage init failed: {e}"));
        e
    })
}

/// `_setup_ktalk_bridge`: the bridge wired to the transcriber, not started yet.
///
/// # Errors
/// A `ktalk_only` installation cannot run without its bridge.
pub fn setup_bridge(live: &Arc<Live>, config: &mut SessionConfig) -> Result<Option<Arc<Bridge>>, String> {
    if !live.use_ktalk() {
        return Ok(None);
    }
    let ktalk_only = live.options().ktalk_only;
    let mut bridge_config = config.bridge.clone();
    bridge_config.ktalk_only = ktalk_only;
    let built = Bridge::new(bridge_config, config.policy.clone())
        .and_then(|bridge| wire_bridge(live, config, bridge, ktalk_only));
    match built {
        Ok(bridge) => {
            live.set_conference(Some(Arc::new(bridge.clone()) as Arc<dyn Conference>));
            Ok(Some(bridge))
        }
        Err(e) if ktalk_only => Err(e),
        Err(e) => {
            live.env.log.line(&format!("Warning: KTalk bridge error: {e}, speaker identification unavailable"));
            live.set_use_ktalk(false);
            Ok(None)
        }
    }
}

/// The bridge's hooks into the transcriber, its storage and the increments.
fn wire_bridge(
    live: &Arc<Live>,
    config: &SessionConfig,
    bridge: Arc<Bridge>,
    ktalk_only: bool,
) -> Result<Arc<Bridge>, String> {
    let hooks = Arc::new(LiveHooks(Arc::downgrade(live)));
    if ktalk_only {
        bridge.set_meeting_capture(hooks.clone());
    }
    bridge.configure_user_profile(&live.user_identity(), hooks.clone())?;
    bridge.set_live_events(Some(hooks));
    if let Some(storage) = live.storage() {
        bridge.set_meeting_storage(storage.clone());
        if let Some(make) = &config.increments {
            bridge.set_increment_generator(make(storage.base_path()));
        }
    }
    Ok(bridge)
}

/// `_setup_per_speaker_ingest` and `_setup_refine_worker`.
pub fn setup_per_speaker_ingest(live: &Arc<Live>) {
    if !(live.use_pcm_transport() && live.use_ktalk() && live.conference().is_some()) {
        return;
    }
    let weak = Arc::downgrade(live);
    let generation: GenerationGetter = {
        let weak = weak.clone();
        Arc::new(move || weak.upgrade().map_or(0, |live| live.generation()))
    };
    let ingest = start_ingest(live, &weak, generation.clone());
    let worker = start_refine_worker(live, &weak, generation);
    if !live.is_recording_active() {
        worker.pause();
    }
    worker.start();
    ingest.set_refine_submit_callback(Some(Arc::new(move |task| {
        if let Some(live) = weak.upgrade() {
            live.submit_refine(task);
        }
        Ok(())
    })));
    live.println("RefineWorker started");
}

/// `_setup_per_speaker_ingest`: the ingest started, its batches into the transcriber.
fn start_ingest(live: &Arc<Live>, weak: &Weak<Live>, generation: GenerationGetter) -> Arc<PerSpeakerIngest> {
    let callback: TranscribeCallback = {
        let weak = weak.clone();
        Arc::new(move |batch| weak.upgrade().map_or(Ok(()), |live| live.on_pcm_batch(batch)))
    };
    let meeting_active = {
        let weak = weak.clone();
        Arc::new(move || {
            weak.upgrade().and_then(|live| live.storage()).is_some_and(|s| s.current_meeting_id().is_some())
        })
    };
    let asr_available = {
        let weak = weak.clone();
        Arc::new(move || weak.upgrade().is_some_and(|live| live.asr().available()))
    };
    let ingest = Arc::new(PerSpeakerIngest::new(
        callback,
        IngestOptions {
            generation_getter: Some(generation),
            meeting_active_getter: Some(meeting_active),
            asr_available_getter: Some(asr_available),
            clock: live.env.clock.clone(),
            logger: live.env.log.clone(),
            ..IngestOptions::default()
        },
    ));
    live.set_ingest(Some(ingest.clone()));
    ingest.start();
    live.println("Per-speaker PCM ingest started");
    ingest
}

/// `_setup_refine_worker`: the worker, not started, re-transcribing through the
/// transcriber and applying there.
fn start_refine_worker(live: &Arc<Live>, weak: &Weak<Live>, generation: GenerationGetter) -> Arc<RefineWorker> {
    let transcribe = {
        let weak = weak.clone();
        Arc::new(move |pcm: &[i16], should_abort: Option<&dyn Fn() -> bool>| match weak.upgrade() {
            Some(live) => live.refine_transcribe(pcm, should_abort),
            None => Err(crate::asr::AsrError::Aborted),
        })
    };
    let apply = {
        let weak = weak.clone();
        Arc::new(move |result: &crate::refine::RefineResult| {
            weak.upgrade().map_or(Ok(()), |live| live.apply_refine(result))
        })
    };
    let worker = Arc::new(RefineWorker::new(
        new_model_lock(),
        transcribe,
        apply,
        RefineOptions {
            abort_mode: AbortMode::Cooperative,
            transcribe_manages_lock: true,
            generation_getter: Some(generation),
            clock: live.env.clock.clone(),
            logger: live.env.log.clone(),
            ..RefineOptions::default()
        },
    ));
    live.set_refine_worker(Some(worker.clone()));
    worker
}

fn banner(live: &Live) {
    let system = live.system_audio_label();
    let mic = &live.options().mic_display;
    live.println(&"=".repeat(60));
    live.println(" LIVE TRANSCRIPTION");
    if live.dual_mode() {
        live.println(&format!(" Mode: DUAL (Mic: {mic} + System: {system})"));
    } else if live.system_capture().is_some() {
        live.println(&format!(" Mic: none | System: {system}"));
    } else {
        live.println(&format!(" Mic: {mic} | System: none"));
    }
    if live.use_ktalk() {
        live.println(" Speaker detection: KTalk (via Chrome extension)");
    } else {
        live.println(" Speaker detection: unavailable without conference data");
    }
    live.println(&"=".repeat(60));
    live.println("");
    live.println("Press Ctrl+C to stop");
    live.println(&"-".repeat(60));
    live.println("");
    if live.options().ktalk_only {
        live.println("Recording policy: KTalk meetings only; idle audio capture is off");
    }
}

fn main_loop(live: &Live) -> Result<(), String> {
    live.start_audio_captures()?;
    let clock = live.env.clock.clone();
    let mut last_audio_cleanup = clock.monotonic();
    while live.running() {
        clock.sleep(0.1);
        if !live.running() {
            break;
        }
        if clock.monotonic() - last_audio_cleanup >= AUDIO_CLEANUP_INTERVAL_S {
            last_audio_cleanup = clock.monotonic();
            cleanup_old_audio(live);
        }
        poll_captures(live)?;
    }
    Ok(())
}

/// The hourly retention pass over the recorded audio.
fn cleanup_old_audio(live: &Live) {
    if let Some(storage) = live.storage()
        && let Err(e) = storage.cleanup_old_audio(24.0)
    {
        live.env.log.line(&format!("Warning: audio cleanup failed: {e}"));
    }
}

/// One iteration's work on the captures: broken devices reopened, a chunk of each
/// source, the system capture's liveness and digital-silence checks.
fn poll_captures(live: &Live) -> Result<(), String> {
    live.reopen_stopped_device_captures();
    if live.mic_capture().is_some() {
        live.process_chunk(true)?;
    }
    if live.system_capture().is_some() {
        live.check_system_capture_alive()?;
        live.restart_silent_system_capture();
        live.process_chunk(false)?;
    }
    Ok(())
}

/// `_teardown_session`: captures and workers stopped, WAVs finalized, the bridge
/// stopped, the meeting ended, the transcript file written.
pub fn teardown(live: &Live, bridge: Option<&Arc<Bridge>>) {
    live.println(&format!("\n{}", "-".repeat(60)));
    live.println("Остановлено.");
    live.set_running(false);
    let log = &live.env.log;
    if live.options().ktalk_only
        && let Err(e) = live.close_conference_capture()
    {
        log.line(&format!("[LiveTranscriber] stopping the capture failed: {e}"));
    }
    live.stop_audio();
    if let Err(e) = live.finalize_audio() {
        log.line(&format!("[LiveTranscriber] finalizing the audio failed: {e}"));
    }
    if let Some(bridge) = bridge {
        bridge.stop();
    }
    end_meeting(live);
    write_transcript_file(live);
}

/// The storage's session ended, the saved meeting named.
fn end_meeting(live: &Live) {
    if let Some(storage) = live.storage() {
        match storage.end_session() {
            Ok(Some(id)) => live.println(&format!("Meeting saved: {id}")),
            Ok(None) => {}
            Err(e) => live.env.log.line(&format!("[LiveTranscriber] ending the meeting failed: {e}")),
        }
    }
}

/// `--output`: the transcript lines written to the file, when there are any.
fn write_transcript_file(live: &Live) {
    let lines = live.transcript_lines();
    if let Some(path) = &live.options().output_file
        && !lines.is_empty()
    {
        match std::fs::write(path, lines.join("\n")) {
            Ok(()) => live.println(&format!("Транскрипт сохранён в {}", path.display())),
            Err(e) => live.env.log.line(&format!("[LiveTranscriber] writing {} failed: {e}", path.display())),
        }
    }
}

/// `LiveTranscriber.run`: setup, the main loop until [`Live::set_running`]`(false)`,
/// teardown. A failure in the loop is printed as Python's `[FATAL ERROR]` and still
/// tears down.
///
/// # Errors
/// A setup step failed before the loop (storage, a `ktalk_only` bridge, the model).
pub fn run(live: &Arc<Live>, mut config: SessionConfig) -> Result<(), String> {
    banner(live);
    let golden = start_golden_recording(live, config.golden_record)?;
    live.set_golden(golden.clone());
    let storage = setup_meeting_storage(live, config.storage.clone())?;
    let bridge = setup_bridge(live, &mut config)?;
    if let (Some(bridge), Some(golden)) = (&bridge, &golden) {
        bridge.set_golden(Some(Arc::new(GoldenBridgeTap(golden.clone()))));
    }
    let _ = storage;
    live.asr().load_model().map_err(|e| e.to_string())?;
    setup_per_speaker_ingest(live);
    if let Some(bridge) = &bridge {
        // A bind failure is logged by the bridge as Python's `Server error`.
        let _ = bridge.start();
        live.println("KTalk bridge started, waiting for Chrome extension...");
        live.env.clock.sleep(1.0);
    }
    live.set_running(true);
    live.reset_source_states();
    if let Err(e) = main_loop(live) {
        live.env.log.line(&format!("\n[FATAL ERROR] {e}"));
    }
    teardown(live, bridge.as_ref());
    Ok(())
}

/// The data directory's `meetings`, the repository's prompts and the installation's
/// models, for the increments.
pub fn ai_increments(prompts_dir: PathBuf, legacy: LegacyEnv) -> IncrementsFactory {
    Box::new(move |meetings: &Path| {
        let mut generator = recog_ai::increment::IncrementGenerator::new(meetings, Some(prompts_dir.clone()))
            .with_legacy_env(legacy.clone());
        generator.set_callback(Arc::new(|meeting_id: &str, version: i64, prompt_id: &str| {
            eprintln!("[increment] ready: {meeting_id} v{version} ({prompt_id})");
        }));
        Arc::new(recog_bridge::ai::AiIncrements(Arc::new(generator))) as Arc<dyn IncrementGenerator>
    })
}

/// Meeting titles from the configured provider (`_generate_title_for_meeting`), run on
/// the storage's title thread.
pub struct AiTitles {
    /// The installation's models, below the ones Settings saved.
    pub legacy: LegacyEnv,
}

impl TitleGenerator for AiTitles {
    fn generate(&self, request: &TitleRequest) -> Result<Option<GeneratedTitle>, String> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        let factory = recog_ai::provider::CliProviderFactory::default();
        let store = AiSettingsStore::new(&request.data_dir, None).with_legacy_env(self.legacy.clone());
        let update = runtime.block_on(recog_ai::title::meeting_title_update(&store, &request.meeting_path, &factory));
        Ok(update.map(|u| GeneratedTitle {
            title: u.title,
            provider: u.title_source,
            model: Value::from(u.title_model),
            settings_revision: Value::from(u.title_settings_revision),
        }))
    }
}

/// Waits for Ctrl+C on a thread of its own and ends the main loop then, as Python's
/// `KeyboardInterrupt` does.
pub fn stop_on_interrupt(live: &Arc<Live>) {
    let weak = Arc::downgrade(live);
    let _ = std::thread::Builder::new().name("interrupt".into()).spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        if runtime.block_on(tokio::signal::ctrl_c()).is_ok()
            && let Some(live) = weak.upgrade()
        {
            live.set_running(false);
        }
    });
}
