//! The GigaAM HTTP backend, in place of `recog/asr_server/gigaam_backend.py`:
//! `GET /healthz` and an OpenAI-Whisper-compatible `POST /v1/audio/transcriptions`.
//!
//! The upload goes to a temporary file, ffmpeg turns it into 16 kHz mono 16-bit PCM
//! (the same command line), the model transcribes it ([`Transcriber`]) and
//! [`crate::format`] writes the answer in the requested format. The order of the
//! checks and their answers are Python's: 422 for the fields, 503 `model loading`, 400
//! when ffmpeg fails, 500 for a model failure, 503 `diarization pipeline unavailable`
//! after the transcription when diarization was asked for (a `model` containing
//! `diarized` or `response_format=diarized_json`).
//!
//! Speaker diarization does not exist here: pyannote has no Rust counterpart, so the
//! backend always answers as the Python one does when its pipeline failed to load —
//! `/healthz` reports `"diarization": false` and a diarization request gets the 503.
//!
//! Known difference: requests run concurrently (each model call takes its turn inside
//! wavo), where the Python handler blocks its event loop for the whole request, health
//! checks included; ffmpeg's standard input is closed instead of inherited.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header;
use axum::response::Response;
use recog_core::pyjson::Value;

use crate::config::BackendConfig;
use crate::engine::{Longform, Transcriber};
use crate::form::latin1;
use crate::format;
use crate::http::{self, Routed, object};
use crate::log;
use crate::request::{self, Transcription};
use crate::wav;

const ROUTES: [(&str, &str); 2] = [("/healthz", "GET"), ("/v1/audio/transcriptions", "POST")];

/// The backend: its settings, the model once loaded, and where it runs ffmpeg.
pub struct Backend {
    config: BackendConfig,
    engine: Option<Arc<dyn Transcriber>>,
    ffmpeg: PathBuf,
    temp_dir: PathBuf,
}

impl Backend {
    /// A backend whose model is `engine` (`None`: still loading), running `ffmpeg`
    /// from PATH with temporary files in the system's temporary directory.
    #[must_use]
    pub fn new(config: BackendConfig, engine: Option<Arc<dyn Transcriber>>) -> Self {
        Self { config, engine, ffmpeg: PathBuf::from("ffmpeg"), temp_dir: std::env::temp_dir() }
    }

    /// Runs this program instead of `ffmpeg` from PATH.
    #[must_use]
    pub fn with_ffmpeg(mut self, ffmpeg: PathBuf) -> Self {
        self.ffmpeg = ffmpeg;
        self
    }

    /// Writes the temporary files into `dir`.
    #[must_use]
    pub fn with_temp_dir(mut self, dir: PathBuf) -> Self {
        self.temp_dir = dir;
        self
    }
}

/// The axum application.
pub fn app(backend: Arc<Backend>) -> Router {
    Router::new().fallback(dispatch).with_state(backend)
}

/// Serves on a bound listener until `shutdown` resolves.
///
/// # Errors
/// The listener fails.
pub async fn serve(
    listener: tokio::net::TcpListener,
    backend: Arc<Backend>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, app(backend)).with_graceful_shutdown(shutdown).await
}

async fn dispatch(State(backend): State<Arc<Backend>>, request: Request) -> Response {
    let path = http::decode_path(request.uri().path());
    let host = request.headers().get(header::HOST).map(|v| latin1(v.as_bytes()));
    let query = request.uri().query().map(str::to_owned);
    match http::route(&ROUTES, request.method().as_str(), &path, host.as_deref(), query.as_deref()) {
        Routed::Found(0) => healthz(&backend),
        Routed::Found(1) => transcribe(backend, request).await,
        other => http::unrouted(other),
    }
}

fn healthz(backend: &Backend) -> Response {
    http::json(
        200,
        &object([
            ("ok", Value::Bool(backend.engine.is_some())),
            ("variant", Value::from(backend.config.variant.as_str())),
            ("diarization", Value::Bool(false)),
        ]),
    )
}

/// `Path(name).suffix` of a POSIX path.
fn suffix(name: &str) -> String {
    let last = name.split('/').rfind(|part| !part.is_empty() && *part != ".").unwrap_or("");
    match last.rfind('.') {
        Some(i) if i > 0 && i < last.len() - 1 => last[i..].to_owned(),
        _ => String::new(),
    }
}

/// How a transcription ended before formatting.
enum Failure {
    /// An `HTTPException`: status and detail.
    Http(u16, &'static str),
    /// An unhandled exception: the 500.
    Internal(String),
}

/// What the blocking part needs.
struct Job {
    engine: Arc<dyn Transcriber>,
    ffmpeg: PathBuf,
    temp_dir: PathBuf,
    filename: String,
    data: Vec<u8>,
    want_diar: bool,
    want_words: bool,
}

/// The last 500 characters of ffmpeg's stderr; `None` where Python's strict
/// `stderr.decode()` would raise (and turn the 400 into a 500).
fn stderr_tail(stderr: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(stderr).ok()?;
    let count = text.chars().count();
    Some(text.chars().skip(count.saturating_sub(500)).collect())
}

fn run_ffmpeg(job: &Job, src: &Path, wav_path: &Path) -> Result<(), Failure> {
    let output = std::process::Command::new(&job.ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(src)
        .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
        .arg(wav_path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Failure::Internal(format!("ffmpeg: {e}")))?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or_else(|| {
            use std::os::unix::process::ExitStatusExt as _;
            -output.status.signal().unwrap_or(0)
        });
        let tail =
            stderr_tail(&output.stderr).ok_or_else(|| Failure::Internal("ffmpeg's stderr is not UTF-8".to_owned()))?;
        log::error(&format!("ffmpeg failed ({code}): {tail}"));
        return Err(Failure::Http(400, "ffmpeg could not decode the audio"));
    }
    Ok(())
}

fn transcribe_file(job: &Job, src: &Path, wav_path: &Path) -> Result<Longform, Failure> {
    run_ffmpeg(job, src, wav_path)?;
    let raw = std::fs::read(wav_path).map_err(|e| Failure::Internal(e.to_string()))?;
    log::info(&format!(
        "transcoded {} ({} bytes) → wav ({} bytes), diar={}, words={}",
        job.filename,
        job.data.len(),
        raw.len(),
        log::py_bool(job.want_diar),
        log::py_bool(job.want_words)
    ));
    let pcm = wav::read_pcm16_mono(&raw).map_err(Failure::Internal)?;
    let longform = job.engine.transcribe_longform(&pcm, job.want_words).map_err(Failure::Internal)?;
    log::info(&format!("gigaam returned {} segment(s)", longform.segments.len()));
    if job.want_diar {
        return Err(Failure::Http(503, "diarization pipeline unavailable"));
    }
    if job.want_words {
        log::info(&format!("word timestamps: {} word(s)", longform.words.len()));
    }
    Ok(longform)
}

fn run(job: &Job) -> Result<Longform, Failure> {
    let suffix = Some(suffix(if job.filename.is_empty() { "audio.wav" } else { &job.filename }))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".wav".to_owned());
    let mut file = tempfile::Builder::new()
        .prefix("tmp")
        .rand_bytes(8)
        .suffix(&suffix)
        .tempfile_in(&job.temp_dir)
        .map_err(|e| Failure::Internal(e.to_string()))?;
    std::io::Write::write_all(&mut file, &job.data).map_err(|e| Failure::Internal(e.to_string()))?;
    let src = file.into_temp_path().keep().map_err(|e| Failure::Internal(e.to_string()))?;
    let mut wav_path = src.clone().into_os_string();
    wav_path.push(".16k.wav");
    let wav_path = PathBuf::from(wav_path);
    let result = transcribe_file(job, &src, &wav_path);
    for path in [&src, &wav_path] {
        let _ = std::fs::remove_file(path);
    }
    result
}

async fn transcribe(backend: Arc<Backend>, request: Request) -> Response {
    let t: Transcription = match request::read(request, "gigaam").await {
        Ok(t) => t,
        Err(answer) => return *answer,
    };
    let Some(engine) = backend.engine.clone() else {
        return http::detail(503, "model loading");
    };
    let fmt = t.response_format.to_lowercase();
    let want_diar = t.model.to_lowercase().contains("diarized") || fmt == "diarized_json";
    let want_words = t.timestamp_granularities.as_deref().unwrap_or("").to_lowercase().contains("word");
    let job = Job {
        engine,
        ffmpeg: backend.ffmpeg.clone(),
        temp_dir: backend.temp_dir.clone(),
        filename: t.file.filename,
        data: t.file.data,
        want_diar,
        want_words,
    };
    let outcome =
        tokio::task::spawn_blocking(move || run(&job)).await.unwrap_or_else(|e| Err(Failure::Internal(e.to_string())));
    match outcome {
        Ok(longform) => formatted(&fmt, &longform, t.language.as_deref().unwrap_or("ru"), want_words),
        Err(failure) => failure.answer(),
    }
}

impl Failure {
    /// The `HTTPException`'s answer, or the 500 of an unhandled exception.
    fn answer(self) -> Response {
        match self {
            Failure::Http(status, detail) => http::detail(status, detail),
            Failure::Internal(message) => {
                log::error(&format!("Exception in ASGI application: {message}"));
                http::internal_error()
            }
        }
    }
}

/// The transcription in the requested format; a time that is not a finite number is
/// the 500 of the exception Python raises there.
fn formatted(fmt: &str, longform: &Longform, language: &str, want_words: bool) -> Response {
    let words = want_words.then_some(longform.words.as_slice());
    match format::format_response(fmt, &longform.segments, language, words) {
        Some(formatted) => http::text(200, formatted.body, formatted.media_type),
        None => {
            log::error("Exception in ASGI application: a time is not a finite number");
            http::internal_error()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_when_their_suffix_is_taken_then_pathlibs_rule() {
        assert_eq!(suffix("a.mp3"), ".mp3");
        assert_eq!(suffix("dir/a.tar.gz"), ".gz");
        assert_eq!(suffix(".bashrc"), "");
        assert_eq!(suffix("a."), "");
        assert_eq!(suffix("a.wav/."), ".wav");
        assert_eq!(suffix("noext"), "");
    }

    #[test]
    fn non_utf8_ffmpeg_output_when_tailed_then_none_as_pythons_decode_raises() {
        assert_eq!(stderr_tail(b"\xff"), None);
        assert_eq!(stderr_tail("ж".repeat(600).as_bytes()).unwrap().chars().count(), 500);
    }
}
