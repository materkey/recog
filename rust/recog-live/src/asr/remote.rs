//! ASR backend "remote": GigaAM on a network gateway with a lazy local fallback, in
//! place of `recog/remote_asr.py` (ADR-0020, and
//! docs/plans/2026-06-02-asr-fallback-buffering-design.md).
//!
//! [`RemoteAsrClient`] speaks to an OpenAI-Whisper-compatible gateway
//! (`POST /v1/audio/transcriptions`, multipart, the same fields as Python's) for the
//! three paths the live process uses: the live chunk, the per-speaker batch (one
//! request per clip, in parallel, the order kept) and the refine window with word
//! timestamps. The offline `transcribe_simple` has no caller in Rust.
//!
//! [`FallbackAsr`] routes each call through one of two independent circuit breakers
//! (live: chunk and batch; background: refine) and falls back to the local model, here
//! [`WavoAsr`], whose weights load in a background thread on the first remote failure.
//! While neither side can serve it answers [`AsrError::TemporarilyUnavailable`] and
//! [`FallbackAsr::available`] is false, so the callers keep their audio buffered.
//!
//! Known differences from Python: the WAV sent has no `PEAK` chunk (libsndfile adds one
//! with a time stamp, which makes Python's bytes differ from run to run anyway); a
//! timeout is a deadline for the whole request rather than httpx's per-phase limit; the
//! error text of a failed request is ureq's, not httpx's; a chunk answer whose `text`
//! is neither a string nor null counts as a failure of the gateway, where Python would
//! hand the value on.

use std::collections::VecDeque;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use recog_core::py;
use recog_core::pyjson::{self, Value};

use super::wavo::WavoAsr;
use super::{AsrError, Clip, Transcription, Word};
use crate::clock::SharedClock;
use crate::log::Logger;

/// How a remote call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteFailure {
    /// `RemoteASRError`: the gateway failed (HTTP, timeout, parse); the breaker counts
    /// it and the call falls back to the local model.
    Remote(String),
    /// Anything Python raises outside the request's `try` (an answer whose words cannot
    /// be read): passed to the caller as is, the breaker untouched.
    Other(AsrError),
}

/// `RemoteASRClient`: a blocking client of the gateway.
pub struct RemoteAsrClient {
    base_url: String,
    model: String,
    timeout: Option<Duration>,
    live_timeout: Option<Duration>,
    max_workers: usize,
    agent: ureq::Agent,
}

/// A timeout in seconds as ureq takes it; None (no limit) for a value it cannot hold.
fn duration(seconds: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(seconds).ok()
}

/// A multipart boundary: 32 random hex digits, as httpx's `os.urandom(16).hex()`.
fn boundary() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let state = RandomState::new();
    let mut text = String::with_capacity(32);
    for _ in 0..2 {
        let mut hasher = state.build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        text.push_str(&format!("{:016x}", hasher.finish()));
    }
    text
}

/// `_wav_bytes`: float32 mono as a WAV file (IEEE float, a `fact` chunk), as
/// soundfile's `subtype="FLOAT"` writes it, without libsndfile's `PEAK` chunk.
#[must_use]
pub fn wav_bytes(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = u32::try_from(samples.len() * 4).unwrap_or(u32::MAX);
    let frames = u32::try_from(samples.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(52 + samples.len() * 4);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(44u32.saturating_add(data_len)).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&frames.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for v in samples {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// The multipart body httpx builds from `data=` then `files=`, and its boundary.
fn multipart(fields: &[(&str, &str)], filename: &str, file: &[u8]) -> (String, Vec<u8>) {
    let boundary = boundary();
    let mut body = Vec::with_capacity(file.len() + 512);
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
             Content-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (boundary, body)
}

/// `float(x)` of a JSON value, None where Python would raise.
fn py_float(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Str(s) => py::parse_float(s).ok(),
        _ => None,
    }
}

/// `int(round(float(x) * 1000))`: seconds to whole ms, ties to even as Python rounds.
fn seconds_to_ms(value: &Value) -> Result<i64, String> {
    let seconds = py_float(value).ok_or_else(|| format!("could not convert {} to float", value.py_repr()))?;
    let ms = (seconds * 1000.0).round_ties_even();
    if !ms.is_finite() {
        return Err(format!("cannot convert float {} to integer", py::float_repr(ms)));
    }
    Ok(ms as i64)
}

/// The words of a `verbose_json` answer, as `transcribe_with_timestamps` reads them.
fn parse_words(payload: &Value) -> Result<Transcription, AsrError> {
    let Value::Object(map) = payload else {
        return Err(AsrError::Failed(format!("'{}' object has no attribute 'get'", payload.type_name())));
    };
    let text = match map.get("text") {
        None | Some(Value::Null) => String::new(),
        Some(Value::Str(s)) => s.clone(),
        Some(other) => return Err(AsrError::Failed(format!("text is {}, not a string", other.type_name()))),
    };
    let mut words = Vec::new();
    match map.get("words") {
        Some(Value::Array(items)) => {
            for item in items {
                let Value::Object(w) = item else {
                    return Err(AsrError::Failed(format!("a word is {}, not an object", item.type_name())));
                };
                let field = |key: &str| w.get(key).ok_or_else(|| AsrError::Failed(format!("KeyError: '{key}'")));
                let Value::Str(word) = field("word")? else {
                    return Err(AsrError::Failed("a word's text is not a string".into()));
                };
                let start = seconds_to_ms(field("start")?).map_err(AsrError::Failed)?;
                let end = seconds_to_ms(field("end")?).map_err(AsrError::Failed)?;
                words.push(Word { text: word.clone(), start, end });
            }
        }
        Some(value) if value.truthy() => {
            return Err(AsrError::Failed(format!("words is {}, not a list", value.type_name())));
        }
        _ => {}
    }
    Ok(Transcription { text, words })
}

/// `_to_float32_mono` of a clip and `_wav_bytes`.
fn clip_wav(clip: Clip<'_>, sample_rate: u32) -> Vec<u8> {
    wav_bytes(&clip.to_f32(), sample_rate)
}

impl RemoteAsrClient {
    /// A client of the gateway at `base_url` (trailing slashes dropped): `model` is the
    /// alias sent, `timeout_s` bounds refine and the health probe, `live_timeout_s`
    /// (`timeout_s` when None) the chunk and batch requests.
    #[must_use]
    pub fn new(base_url: &str, model: &str, timeout_s: f64, live_timeout_s: Option<f64>) -> Self {
        let timeout = duration(timeout_s);
        let agent = ureq::Agent::config_builder().timeout_global(timeout).build().new_agent();
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            model: model.to_owned(),
            timeout,
            live_timeout: duration(live_timeout_s.unwrap_or(timeout_s)),
            max_workers: 8,
            agent,
        }
    }

    /// The gateway's address, without trailing slashes.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `GET /healthz`: true only when the gateway answers and says ok, either flat
    /// (`{"ok": true}`) or per engine (`{"gigaam": {"ok": true}, ...}`: this model's
    /// engine, else any).
    #[must_use]
    pub fn healthz(&self) -> bool {
        let Ok(mut response) = self.agent.get(format!("{}/healthz", self.base_url)).call() else {
            return false;
        };
        let Ok(body) = response.body_mut().read_to_vec() else { return false };
        let Ok(Value::Object(data)) = pyjson::loads_bytes(&body) else { return false };
        if let Some(ok) = data.get("ok") {
            return ok.truthy();
        }
        if let Some(Value::Object(engine)) = data.get(&self.model)
            && let Some(ok) = engine.get("ok")
        {
            return ok.truthy();
        }
        data.values().any(|v| matches!(v, Value::Object(e) if e.get("ok").is_some_and(Value::truthy)))
    }

    /// One `POST /v1/audio/transcriptions`, the parsed JSON answer.
    fn post(
        &self,
        fields: &[(&str, &str)],
        filename: &str,
        wav: &[u8],
        timeout: Option<Duration>,
    ) -> Result<Value, String> {
        let (boundary, body) = multipart(fields, filename, wav);
        let mut response = self
            .agent
            .post(format!("{}/v1/audio/transcriptions", self.base_url))
            .config()
            .timeout_global(timeout)
            .build()
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .send(&body[..])
            .map_err(|e| e.to_string())?;
        let raw = response.body_mut().read_to_vec().map_err(|e| e.to_string())?;
        pyjson::loads_bytes(&raw).map_err(|e| e.to_string())
    }

    /// One clip's text (`response_format=json`); None when the gateway's `text` is null.
    ///
    /// # Errors
    /// [`RemoteFailure::Remote`] for any failure of the request or its answer.
    pub fn transcribe_chunk(&self, clip: Clip<'_>, sample_rate: u32) -> Result<Option<String>, RemoteFailure> {
        let wav = clip_wav(clip, sample_rate);
        let fields = [("model", self.model.as_str()), ("response_format", "json")];
        let failed = |e: String| RemoteFailure::Remote(format!("transcribe_chunk failed: {e}"));
        let payload = self.post(&fields, "chunk.wav", &wav, self.live_timeout).map_err(failed)?;
        let Value::Object(map) = &payload else {
            return Err(failed(format!("'{}' object is not subscriptable", payload.type_name())));
        };
        match map.get("text") {
            Some(Value::Str(text)) => Ok(Some(text.clone())),
            Some(Value::Null) => Ok(None),
            Some(other) => Err(failed(format!("text is {}, not a string", other.type_name()))),
            None => Err(failed("'text'".into())),
        }
    }

    /// One text per clip, in order, the requests in parallel (up to 8 at a time); a null
    /// `text` is an empty one.
    ///
    /// # Errors
    /// The first failure in the clips' order: the caller redoes the whole batch, so no
    /// speaker is left with a hole.
    pub fn transcribe_batch(&self, clips: &[Clip<'_>]) -> Result<Vec<String>, RemoteFailure> {
        let mut results = Vec::with_capacity(clips.len());
        for window in clips.chunks(self.max_workers.max(1)) {
            std::thread::scope(|scope| {
                let handles: Vec<_> = window
                    .iter()
                    .map(|clip| scope.spawn(move || self.transcribe_chunk(*clip, crate::config::SAMPLE_RATE)))
                    .collect();
                for handle in handles {
                    let result = handle.join().unwrap_or_else(|_| {
                        Err(RemoteFailure::Remote("transcribe_batch failed: worker panicked".into()))
                    });
                    results.push(result);
                }
            });
        }
        results.into_iter().map(|r| r.map(Option::unwrap_or_default)).collect()
    }

    /// The refine window's text and words (`verbose_json`, word granularity), `start`
    /// and `end` in whole ms.
    ///
    /// # Errors
    /// [`RemoteFailure::Remote`] for a failure of the request or a body that is not
    /// JSON; [`RemoteFailure::Other`] for an answer whose words cannot be read.
    pub fn transcribe_with_timestamps(&self, clip: Clip<'_>, sample_rate: u32) -> Result<Transcription, RemoteFailure> {
        let wav = clip_wav(clip, sample_rate);
        let fields =
            [("model", self.model.as_str()), ("response_format", "verbose_json"), ("timestamp_granularities", "word")];
        let payload = self
            .post(&fields, "refine.wav", &wav, self.timeout)
            .map_err(|e| RemoteFailure::Remote(format!("transcribe_with_timestamps failed: {e}")))?;
        parse_words(&payload).map_err(RemoteFailure::Other)
    }
}

impl std::fmt::Debug for RemoteAsrClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteAsrClient").field("base_url", &self.base_url).field("model", &self.model).finish()
    }
}

/// `_CircuitBreaker.FAILURE_WINDOW_S`: failures older than this do not count.
pub const FAILURE_WINDOW_S: f64 = 60.0;

/// `FallbackASR.LOCAL_LOAD_RETRY_S`: a failed local load is retried no sooner.
pub const LOCAL_LOAD_RETRY_S: f64 = 300.0;

#[derive(Debug, Default)]
struct BreakerState {
    failure_times: VecDeque<f64>,
    /// None while closed; the end of the open window (or of the reserved probe).
    opened_until: Option<f64>,
    epoch: u64,
}

/// `_CircuitBreaker`: one request class's failure window and recovery probe.
pub struct CircuitBreaker {
    fail_threshold: i64,
    open_seconds: f64,
    scope: &'static str,
    state: Mutex<BreakerState>,
    clock: SharedClock,
    logger: Logger,
}

impl CircuitBreaker {
    #[must_use]
    pub fn new(
        fail_threshold: i64,
        open_seconds: f64,
        scope: &'static str,
        clock: SharedClock,
        logger: Logger,
    ) -> Self {
        Self { fail_threshold, open_seconds, scope, state: Mutex::default(), clock, logger }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, BreakerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `"live"` or `"background"`.
    #[must_use]
    pub fn scope(&self) -> &'static str {
        self.scope
    }

    /// Whether the remote is presumed alive: closed, or its open window is over.
    #[must_use]
    pub fn using_remote(&self) -> bool {
        let state = self.state();
        state.opened_until.is_none_or(|until| self.clock.monotonic() >= until)
    }

    /// `_decide`: whether to try the remote, and the open generation; once the window
    /// is over the first caller reserves the lone probe by extending it.
    pub fn decide(&self) -> (bool, u64) {
        let mut state = self.state();
        let now = self.clock.monotonic();
        match state.opened_until {
            None => (true, state.epoch),
            Some(until) if now >= until => {
                state.opened_until = Some(now + self.open_seconds);
                (true, state.epoch)
            }
            Some(_) => (false, state.epoch),
        }
    }

    /// `_record_success`: closes the circuit unless it reopened since `epoch`; only a
    /// successful probe clears the failures.
    pub fn record_success(&self, epoch: u64) {
        let was_open = {
            let mut state = self.state();
            if epoch != state.epoch {
                return;
            }
            let was_open = state.opened_until.is_some();
            if was_open {
                state.failure_times.clear();
            }
            state.opened_until = None;
            was_open
        };
        if was_open {
            self.logger.line(&format!("[FallbackASR] {} remote recovered — circuit CLOSED", self.scope));
        }
    }

    /// `_record_failure`: counts a failure of generation `epoch`, opening the circuit
    /// at the threshold within [`FAILURE_WINDOW_S`].
    pub fn record_failure(&self, epoch: u64) {
        let mut state = self.state();
        if epoch != state.epoch {
            return;
        }
        let now = self.clock.monotonic();
        while state.failure_times.front().is_some_and(|&t| t <= now - FAILURE_WINDOW_S) {
            state.failure_times.pop_front();
        }
        state.failure_times.push_back(now);
        let failures = state.failure_times.len();
        if i64::try_from(failures).unwrap_or(i64::MAX) >= self.fail_threshold && state.opened_until.is_none() {
            state.opened_until = Some(self.clock.monotonic() + self.open_seconds);
            state.epoch += 1;
            self.logger.line(&format!(
                "[FallbackASR] {} remote circuit OPEN for {}s after {failures} fail(s) in {}s",
                self.scope,
                py::float_repr(self.open_seconds),
                py::float_repr(FAILURE_WINDOW_S)
            ));
        }
    }
}

#[derive(Debug, Default)]
struct LoadState {
    loading: bool,
    failed_at: Option<f64>,
    announced: bool,
}

struct Local {
    asr: Arc<WavoAsr>,
    ready: AtomicBool,
    load: Mutex<LoadState>,
    loaded: Condvar,
    clock: SharedClock,
    logger: Logger,
}

impl Local {
    fn load_state(&self) -> std::sync::MutexGuard<'_, LoadState> {
        self.load.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `_load_local_weights`: the background thread's body.
    fn load_weights(&self) {
        match self.asr.load_model(true) {
            Ok(_) => {
                self.ready.store(true, Ordering::SeqCst);
                self.logger.line("[FallbackASR] local GigaAM ready — fallback active");
                self.load_state().loading = false;
            }
            Err(e) => {
                let name = match e {
                    AsrError::InvalidInput(_) => "ValueError",
                    _ => "RuntimeError",
                };
                self.logger.line(&format!("[FallbackASR] local GigaAM load FAILED: {name}: {e}"));
                let mut state = self.load_state();
                state.failed_at = Some(self.clock.monotonic());
                state.loading = false;
            }
        }
        self.loaded.notify_all();
    }
}

/// Which breaker a call goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Chunk and batch.
    Live,
    /// Refine.
    Background,
}

/// `FallbackASR`: remote first under a breaker, the local model as the reserve.
pub struct FallbackAsr {
    remote: RemoteAsrClient,
    live_circuit: CircuitBreaker,
    background_circuit: CircuitBreaker,
    local: Arc<Local>,
    logger: Logger,
}

impl FallbackAsr {
    /// The router over `remote` with `local` as the reserve (its weights are not loaded
    /// here); each breaker opens after `fail_threshold` failures within a minute for
    /// `open_seconds`.
    #[must_use]
    pub fn new(
        remote: RemoteAsrClient,
        local: Arc<WavoAsr>,
        fail_threshold: i64,
        open_seconds: f64,
        clock: SharedClock,
        logger: Logger,
    ) -> Self {
        let breaker = |scope| CircuitBreaker::new(fail_threshold, open_seconds, scope, clock.clone(), logger.clone());
        Self {
            remote,
            live_circuit: breaker("live"),
            background_circuit: breaker("background"),
            local: Arc::new(Local {
                asr: local,
                ready: AtomicBool::new(false),
                load: Mutex::default(),
                loaded: Condvar::new(),
                clock,
                logger: logger.clone(),
            }),
            logger,
        }
    }

    /// The gateway's client.
    #[must_use]
    pub fn remote(&self) -> &RemoteAsrClient {
        &self.remote
    }

    /// The local model.
    #[must_use]
    pub fn local(&self) -> &WavoAsr {
        &self.local.asr
    }

    /// Whether the local weights have loaded.
    #[must_use]
    pub fn local_ready(&self) -> bool {
        self.local.ready.load(Ordering::SeqCst)
    }

    /// Whether a background load of the local weights is under way.
    #[must_use]
    pub fn local_loading(&self) -> bool {
        self.local.load_state().loading
    }

    /// The live breaker's advisory state (no probe reserved).
    #[must_use]
    pub fn using_remote(&self) -> bool {
        self.live_circuit.using_remote()
    }

    /// Whether a call can be served now: the remote presumed alive or the local model
    /// loaded. False tells the callers to keep their buffers.
    #[must_use]
    pub fn available(&self) -> bool {
        self.local_ready() || self.using_remote()
    }

    /// Waits up to `timeout` for a background load under way; whether the local model
    /// is ready (tests, diagnostics).
    pub fn wait_local_ready(&self, timeout: Duration) -> bool {
        let state = self.local.load_state();
        drop(self.local.loaded.wait_timeout_while(state, timeout, |s| s.loading));
        self.local_ready()
    }

    /// `_start_local_load`: starts the background load once; after a failure no sooner
    /// than [`LOCAL_LOAD_RETRY_S`].
    fn start_local_load(&self) {
        let mut state = self.local.load_state();
        if self.local_ready() || state.loading {
            return;
        }
        if state.failed_at.is_some_and(|at| self.local.clock.monotonic() - at < LOCAL_LOAD_RETRY_S) {
            return;
        }
        if !state.announced {
            self.logger.line("[FallbackASR] remote failed — loading local GigaAM in background");
            state.announced = true;
        }
        state.loading = true;
        let local = self.local.clone();
        let spawned =
            std::thread::Builder::new().name("fallback-asr-local-load".into()).spawn(move || local.load_weights());
        if let Err(e) = spawned {
            state.loading = false;
            state.failed_at = Some(self.local.clock.monotonic());
            self.logger.line(&format!("[FallbackASR] local GigaAM load FAILED: RuntimeError: {e}"));
        }
    }

    /// `load_model`: the health probe while the remote is presumed alive (the local
    /// weights stay unloaded), else the local model.
    ///
    /// # Errors
    /// The local model's load error.
    pub fn load_model(&self) -> Result<(), AsrError> {
        if self.using_remote() {
            // quiet=True, as live loads it: the answer is not logged.
            let _ = self.remote.healthz();
            return Ok(());
        }
        self.local.asr.load_model(true).map(|_| ())
    }

    /// `_run`: the remote under its breaker, the local model only once loaded.
    ///
    /// # Errors
    /// [`AsrError::TemporarilyUnavailable`] when nobody can serve (the remote failed or
    /// its circuit is open, and the local model is not loaded yet); the local call's
    /// error; a [`RemoteFailure::Other`] of the remote call.
    pub fn run<T>(
        &self,
        route: Route,
        remote_call: impl FnOnce(&RemoteAsrClient) -> Result<T, RemoteFailure>,
        local_call: impl FnOnce(&WavoAsr) -> Result<T, AsrError>,
    ) -> Result<T, AsrError> {
        let circuit = match route {
            Route::Live => &self.live_circuit,
            Route::Background => &self.background_circuit,
        };
        let (try_remote, epoch) = circuit.decide();
        if try_remote {
            match remote_call(&self.remote) {
                Ok(out) => {
                    circuit.record_success(epoch);
                    return Ok(out);
                }
                Err(RemoteFailure::Other(e)) => return Err(e),
                Err(RemoteFailure::Remote(message)) => {
                    circuit.record_failure(epoch);
                    let scope = circuit.scope();
                    self.logger.rate_limited(
                        &format!("fallback-remote-fail-{scope}"),
                        &format!("[FallbackASR] {scope} remote call failed: RemoteASRError: {message}"),
                    );
                    self.start_local_load();
                    if self.local_ready() {
                        return local_call(&self.local.asr);
                    }
                    return Err(AsrError::TemporarilyUnavailable);
                }
            }
        }
        if self.local_ready() {
            return local_call(&self.local.asr);
        }
        self.start_local_load();
        Err(AsrError::TemporarilyUnavailable)
    }

    /// The live chunk's text: the gateway's as it gives it, or the local model's.
    ///
    /// # Errors
    /// As [`Self::run`].
    pub fn transcribe_chunk(&self, chunk: &[f32]) -> Result<Option<String>, AsrError> {
        let rate = crate::config::SAMPLE_RATE;
        self.run(
            Route::Live,
            |remote| remote.transcribe_chunk(Clip::F32(chunk), rate),
            |local| Ok(local.transcribe_chunk(Clip::F32(chunk), rate)),
        )
    }

    /// One text per per-speaker clip.
    ///
    /// # Errors
    /// As [`Self::run`].
    pub fn transcribe_batch(&self, clips: &[&[i16]]) -> Result<Vec<String>, AsrError> {
        let clips: Vec<Clip<'_>> = clips.iter().map(|c| Clip::I16(c)).collect();
        self.run(Route::Live, |remote| remote.transcribe_batch(&clips), |local| local.transcribe_batch(&clips))
    }

    /// The refine window: the gateway without the model lock, or the local model under
    /// it with the abort check first (`_refine_transcribe_fn`'s `transcribe_local`).
    ///
    /// # Errors
    /// As [`Self::run`]; `Aborted` when the local path gives way to live.
    pub fn transcribe_with_timestamps(
        &self,
        pcm: &[i16],
        should_abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Transcription, AsrError> {
        self.run(
            Route::Background,
            |remote| remote.transcribe_with_timestamps(Clip::I16(pcm), crate::config::SAMPLE_RATE),
            |local| local.refine(pcm, should_abort),
        )
    }
}

impl std::fmt::Debug for FallbackAsr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackAsr")
            .field("remote", &self.remote)
            .field("local_ready", &self.local_ready())
            .finish_non_exhaustive()
    }
}
