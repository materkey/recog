//! Meeting storage — the port of `recog/meeting_storage.py` (ADR-0011).
//!
//! One directory per meeting under `<data dir>/meetings`: `meta.json` (written
//! atomically through a unique temp file, its mode kept), `transcript.jsonl` (one
//! segment per line, appended; `update_segments` rewrites it through
//! `transcript.jsonl.tmp` and a rename), the WAVs, and `index.json` listing ended
//! meetings. The JSON is the bytes Python's `json.dumps` writes (see `pyjson`).
//!
//! Lifecycle: `start_session` closes the stale sessions of earlier runs, deletes
//! audio older than the retention and opens the first meeting; a conference join
//! opens, reopens (the same room left within ten minutes the same day) or retitles a
//! meeting; leaving or a silence timeout ends it and opens an orphan; an ended meeting
//! without segments is deleted.
//!
//! Title generation (an LLM call in Python) is not here: [`TitleGenerator`] is the
//! hook, run on a background thread like Python's executor, and the storage persists
//! what it returns as Python does.
//!
//! Errors are Python's exceptions ([`StorageError`]): where Python catches some kinds
//! (a malformed `meta.json` skipped by a scan) the port catches the same kinds, and
//! messages printed for them have Python's text.
//!
//! Known differences from Python:
//! - the OS error text inside printed warnings is Rust's (`[Errno 13] Permission
//!   denied: '<path>'` is built the same way but strerror wording may differ);
//! - JSON limits of `pyjson` apply (integers beyond i128, lone surrogates);
//! - `shutil.copy2` copies extended attributes, the port only the permissions and the
//!   access and modification times;
//! - an external index cache that is not UTF-8 or not a JSON object is rebuilt
//!   (Python raises on the first and returns garbage for the second);
//! - `update_segments` takes typed rows: Python would also accept, say, a non-bool
//!   `uncertain` and write it as given;
//! - with the title thread, which Python runs on an executor too, the lines a title
//!   prints may come before or after the operation's own lines.

mod pyops;

use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use md5::{Digest as _, Md5};

use crate::paths;
use crate::py;
use crate::pyjson::{self, Map, Value};
use crate::time::{self, Clock, PyDateTime, SystemClock, TimeError};
use crate::url::urlparse;

use pyops::{add_int, compare_int, get, get_or_empty, item, item_mut, items, set, sort_desc};

/// Rejoining a room reopens its ended meeting only within this window: it covers
/// reconnects and conference flaps, while a permanent room hosts unrelated meetings
/// on the same day that must stay separate.
pub const SAME_ROOM_REOPEN_WINDOW_MICROS: i64 = 10 * 60 * 1_000_000;

/// Sources of extension-driven conference meetings; "orphan" and "local" are plain
/// recordings.
pub const CONFERENCE_SOURCES: [&str; 6] = ["ktalk", "vas3k", "jitsi", "matrix", "gmeet", "zoom"];

/// A Python exception the storage raised, by kind, with `str(exception)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// `json.JSONDecodeError` (a `ValueError`).
    Json(String),
    /// `UnicodeDecodeError` (a `ValueError`).
    Unicode(String),
    Value(String),
    /// `KeyError`: the key's repr.
    Key(String),
    Type(String),
    Attribute(String),
    /// `OSError`.
    Os(String),
}

impl StorageError {
    /// Caught by `except ValueError`.
    #[must_use]
    pub fn is_value_error(&self) -> bool {
        matches!(self, Self::Json(_) | Self::Unicode(_) | Self::Value(_))
    }

    fn os(error: &io::Error, path: &Path) -> Self {
        Self::Os(paths::os_error_message(error, path))
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(m)
            | Self::Unicode(m)
            | Self::Value(m)
            | Self::Key(m)
            | Self::Type(m)
            | Self::Attribute(m)
            | Self::Os(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<TimeError> for StorageError {
    fn from(error: TimeError) -> Self {
        match error {
            TimeError::Value(m) => Self::Value(m),
            TimeError::Type(m) => Self::Type(m),
        }
    }
}

type Result<T, E = StorageError> = std::result::Result<T, E>;

/// Which stream a message goes to: Python prints most to stdout, warnings of the
/// external scan and the title thread to stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// What the title hook gets: the transcript as text and the prompt variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleRequest {
    /// The data root (`base_path.parent`), where the AI settings live.
    pub data_dir: PathBuf,
    pub meeting_path: PathBuf,
    /// `[HH:MM:SS] Speaker: text` lines, system events left out.
    pub transcript: String,
    /// The local participants' names, comma-separated.
    pub user_name: String,
    /// Every participant's name, comma-separated.
    pub participants: String,
    /// The meeting's title so far, or empty.
    pub meeting_title: String,
}

/// A generated title and its provenance, merged into `meta.json` as `title`,
/// `title_source`, `title_model`, `title_settings_revision`.
#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedTitle {
    pub title: String,
    pub provider: String,
    pub model: Value,
    pub settings_revision: Value,
}

/// Generates the title of an ended untitled meeting (the later `recog-ai` crate).
pub trait TitleGenerator: Send + Sync {
    /// A title, None for none, or the failure's text (printed as
    /// `Warning: Title generation failed: …`).
    ///
    /// # Errors
    /// Whatever made generation fail.
    fn generate(&self, request: &TitleRequest) -> Result<Option<GeneratedTitle>, String>;
}

/// The capture configuration a session records in `meta.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMode {
    pub mic: bool,
    pub system_audio: bool,
    pub ktalk: bool,
    pub screencapture: bool,
    pub system_audio_backend: String,
    pub chunk_duration_ms: i64,
    pub ktalk_only: bool,
}

impl Default for SessionMode {
    fn default() -> Self {
        Self {
            mic: false,
            system_audio: false,
            ktalk: false,
            screencapture: false,
            system_audio_backend: "none".into(),
            chunk_duration_ms: 3000,
            ktalk_only: false,
        }
    }
}

/// One row of `update_segments`: the keys present are applied.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SegmentUpdate {
    pub segment_id: i64,
    /// Sets both `speaker` and `speaker_id`.
    pub speaker: Option<String>,
    pub uncertain: Option<bool>,
    pub uncertain_reason: Option<String>,
    /// Sets `text` and `refined: true`, and moves the words count.
    pub text: Option<String>,
}

/// A new transcript segment, `append_segment`'s arguments.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewSegment {
    pub speaker: String,
    pub speaker_id: String,
    pub text: String,
    pub is_local: bool,
    pub is_continuation: bool,
    pub uncertain: bool,
    pub uncertain_reason: String,
    pub audio_offset_ms: i64,
    /// Speech time as Unix seconds; None for events emitted now.
    pub captured_at: Option<f64>,
}

type Logger = Arc<dyn Fn(Stream, &str) + Send + Sync>;

/// How a storage is set up beyond its directory.
#[derive(Clone)]
pub struct StorageOptions {
    /// Read-only meeting directories of other sources and the source they report.
    pub external_paths: Vec<(PathBuf, Option<String>)>,
    /// `[sources.aliases]`: legacy source names and their platform.
    pub source_aliases: Vec<(String, String)>,
    /// Where the indexes of the external directories are cached
    /// (`~/.cache/recog/external_indexes`).
    pub external_index_cache_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    /// The eight characters after the time in a meeting id (`uuid4().hex[:8]`).
    pub meeting_suffix: Arc<dyn Fn() -> String + Send + Sync>,
    pub title_generator: Option<Arc<dyn TitleGenerator>>,
    /// Where Python's `print` lines go.
    pub log: Logger,
}

impl StorageOptions {
    /// The defaults of the Python module: the system clock, random ids, no title
    /// hook, the cache under `$HOME/.cache`, messages printed.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            external_paths: Vec::new(),
            source_aliases: Vec::new(),
            external_index_cache_dir: home.join(".cache/recog/external_indexes"),
            clock: Arc::new(SystemClock),
            meeting_suffix: Arc::new(|| uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()),
            title_generator: None,
            log: Arc::new(|stream, line| match stream {
                Stream::Stdout => println!("{line}"),
                Stream::Stderr => eprintln!("{line}"),
            }),
        }
    }
}

/// `get_external_paths()`: entries `path:source` or `path` (the source then is the
/// parent directory's name).
#[must_use]
pub fn external_paths(entries: &[String]) -> Vec<(PathBuf, Option<String>)> {
    entries
        .iter()
        .map(|entry| match entry.rsplit_once(':') {
            Some((path, source)) => (paths::normalize(py::strip(path)), Some(py::strip(source).to_owned())),
            None => {
                let path = paths::normalize(entry);
                let text = path.to_string_lossy().into_owned();
                let parts: Vec<&str> = text.split('/').filter(|p| !p.is_empty()).collect();
                let parent =
                    if parts.len() >= 2 && text != "." { Some(parts[parts.len() - 2].to_owned()) } else { None };
                (path, parent)
            }
        })
        .collect()
}

struct Inner {
    current_meeting_id: Option<String>,
    current_meeting_path: Option<PathBuf>,
    segment_counter: i64,
    mode: Map,
    chunk_duration_ms: Option<i64>,
    titles: Option<TitleWorker>,
}

struct TitleWorker {
    jobs: mpsc::Sender<TitleJob>,
    thread: JoinHandle<()>,
}

enum TitleJob {
    Meeting(PathBuf),
    Barrier(mpsc::Sender<()>),
}

struct Shared {
    base_path: PathBuf,
    options: StorageOptions,
    inner: Mutex<Inner>,
}

/// `MeetingStorage(base_path, external_paths)`.
pub struct MeetingStorage {
    shared: Arc<Shared>,
}

fn read_text(path: &Path) -> Result<String> {
    let raw = fs::read(path).map_err(|e| StorageError::os(&e, path))?;
    let text = std::str::from_utf8(&raw).map_err(|e| StorageError::Unicode(pyjson::utf8_error_message(&raw, &e)))?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn loads(text: &str) -> Result<Value> {
    pyjson::loads(text).map_err(|e| StorageError::Json(e.0))
}

fn read_json(path: &Path) -> Result<Value> {
    loads(&read_text(path)?)
}

/// `_save_json`: a unique temp file beside the target, created with the target's
/// mode (so a restrictive mode also protects the temp while it is written), renamed
/// over it.
fn save_json(path: &Path, data: &Value) -> Result<()> {
    let mode = match fs::metadata(path) {
        Ok(meta) => Some(meta.permissions().mode() & 0o7777),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(StorageError::os(&e, path)),
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = paths::create_unique(dir, &format!("{name}."), ".tmp", mode.unwrap_or(0o666))
        .map_err(|e| StorageError::os(&e, dir))?;
    paths::replace_with(path, temp, pyjson::dumps(data, Some(2)).as_bytes(), false, mode)
        .map_err(|e| StorageError::os(&e, path))
}

fn is_dir_entries(path: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(path).map_err(|e| StorageError::os(&e, path))?;
    Ok(entries.filter_map(std::result::Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect())
}

fn dir_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn mtime_seconds(meta: &fs::Metadata) -> Option<f64> {
    let since = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    #[allow(clippy::cast_precision_loss)]
    Some(since.as_secs() as f64 + f64::from(since.subsec_nanos()) * 1e-9)
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn print(&self, line: &str) {
        (self.options.log)(Stream::Stdout, line);
    }

    fn eprint(&self, line: &str) {
        (self.options.log)(Stream::Stderr, line);
    }

    fn now(&self) -> PyDateTime {
        time::now_utc(self.options.clock.as_ref())
    }

    fn now_iso(&self) -> Value {
        Value::from(self.now().isoformat())
    }

    fn canonical(&self, source: &Value) -> Value {
        match source {
            Value::Str(s) => Value::from(
                self.options.source_aliases.iter().find(|(name, _)| name == s).map_or(s.as_str(), |(_, p)| p.as_str()),
            ),
            other => other.clone(),
        }
    }

    fn canonical_str<'a>(&'a self, source: &'a str) -> &'a str {
        self.options.source_aliases.iter().find(|(name, _)| name == source).map_or(source, |(_, p)| p.as_str())
    }

    fn is_conference(&self, source: Option<&Value>) -> bool {
        matches!(self.canonical(source.unwrap_or(&Value::Null)), Value::Str(s) if CONFERENCE_SOURCES.contains(&s.as_str()))
    }

    fn meta_path(inner: &Inner) -> Result<PathBuf> {
        inner
            .current_meeting_path
            .as_ref()
            .map(|p| p.join("meta.json"))
            .ok_or_else(|| StorageError::Type("unsupported operand type(s) for /: 'NoneType' and 'str'".into()))
    }

    fn load_meta(inner: &Inner) -> Result<Value> {
        read_json(&Self::meta_path(inner)?)
    }

    fn save_meta(inner: &Inner, meta: &Value) -> Result<()> {
        save_json(&Self::meta_path(inner)?, meta)
    }

    fn wants_generated_title(&self, meta: &Value) -> Result<bool> {
        if get(meta, "title")?.is_some_and(Value::truthy)
            || compare_int(item(item(meta, "stats")?, "segment_count")?, 0, "<=")?.is_le()
        {
            return Ok(false);
        }
        let source = get(meta, "source")?;
        Ok(source.and_then(Value::as_str) == Some("orphan") || self.is_conference(source))
    }

    // ---- lifecycle ----

    fn close_stale_sessions(&self) -> Result<()> {
        let mut closed = 0;
        for meeting_dir in is_dir_entries(&self.base_path)? {
            let meta_path = meeting_dir.join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let attempt = (|| -> Result<bool> {
                let mut meta = read_json(&meta_path)?;
                if get(&meta, "ended_at")?.is_some_and(|v| !v.is_null()) {
                    return Ok(false);
                }
                let now = self.now();
                let created = parse_iso(item(&meta, "created_at")?)?;
                let ended = match get(&meta, "updated_at")? {
                    Some(v) if v.truthy() => v.clone(),
                    _ => Value::from(now.isoformat()),
                };
                set(&mut meta, "ended_at", ended)?;
                set(&mut meta, "end_reason", Value::from("stale_session"))?;
                let duration = parse_iso(item(&meta, "ended_at")?)?.minus(&created)?;
                set(item_mut(&mut meta, "stats")?, "duration_seconds", Value::from(duration / 1_000_000))?;
                save_json(&meta_path, &meta)?;
                self.update_index(&meta)?;
                Ok(true)
            })();
            match attempt {
                Ok(true) => closed += 1,
                Ok(false) => {}
                Err(e) if e.is_value_error() || matches!(e, StorageError::Key(_)) => {
                    self.print(&format!("Warning: Could not close stale session {}: {e}", dir_name(&meeting_dir)));
                }
                Err(e) => return Err(e),
            }
        }
        if closed > 0 {
            self.print(&format!("Closed {closed} stale session(s) from previous runs"));
        }
        Ok(())
    }

    fn create_meeting(&self, inner: &mut Inner, source: &str, prev_meeting_id: Option<&str>) -> Result<String> {
        let now = self.now();
        let meeting_id =
            format!("{}_{}", time::format_wall(now.wall_micros, "%Y-%m-%d_%H%M%S"), (self.options.meeting_suffix)());
        let meeting_path = self.base_path.join(&meeting_id);
        fs::create_dir(&meeting_path).map_err(|e| StorageError::os(&e, &meeting_path))?;
        let null = Value::Null;
        let mut meta = Map::new();
        meta.insert("id", Value::from(meeting_id.as_str()));
        meta.insert("version", Value::Int(1));
        meta.insert("created_at", Value::from(now.isoformat()));
        meta.insert("ended_at", null.clone());
        meta.insert("updated_at", Value::from(now.isoformat()));
        meta.insert("source", Value::from(self.canonical_str(source)));
        meta.insert("end_reason", null.clone());
        meta.insert("mode", Value::Object(inner.mode.clone()));
        meta.insert("chunk_duration_ms", Value::from(inner.chunk_duration_ms.unwrap_or(3000)));
        let conference: Map = ["live_url", "live_id", "host"].iter().map(|k| ((*k).to_owned(), null.clone())).collect();
        meta.insert("conference", Value::Object(conference));
        meta.insert("title", null.clone());
        meta.insert("title_source", null.clone());
        meta.insert("prev_meeting_id", Value::from(prev_meeting_id));
        meta.insert("next_meeting_id", null);
        meta.insert("participants", Value::Object(Map::new()));
        let stats: Map = ["segment_count", "duration_seconds", "words_count"]
            .iter()
            .map(|k| ((*k).to_owned(), Value::Int(0)))
            .collect();
        meta.insert("stats", Value::Object(stats));
        save_json(&meeting_path.join("meta.json"), &Value::Object(meta))?;
        let transcript = meeting_path.join("transcript.jsonl");
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript)
            .map_err(|e| StorageError::os(&e, &transcript))?;
        if let Some(prev) = prev_meeting_id.filter(|p| !p.is_empty()) {
            let prev_path = self.base_path.join(prev).join("meta.json");
            if prev_path.exists() {
                let mut prev_meta = read_json(&prev_path)?;
                set(&mut prev_meta, "next_meeting_id", Value::from(meeting_id.as_str()))?;
                save_json(&prev_path, &prev_meta)?;
            }
        }
        inner.current_meeting_id = Some(meeting_id.clone());
        inner.current_meeting_path = Some(meeting_path);
        inner.segment_counter = 0;
        Ok(meeting_id)
    }

    fn stored_host(&self, conference: &Value) -> Result<Value> {
        match get(conference, "host")? {
            Some(host) if host.truthy() => Ok(host.clone()),
            _ => {
                let url = match get(conference, "live_url")? {
                    Some(Value::Str(s)) if !s.is_empty() => s.clone(),
                    Some(v) if v.truthy() => {
                        return Err(StorageError::Type(format!(
                            "Cannot mix str and non-str arguments ({})",
                            v.type_name()
                        )));
                    }
                    _ => String::new(),
                };
                Ok(Value::from(urlparse(&url).map_err(StorageError::Value)?.0.netloc))
            }
        }
    }

    fn find_same_day_meeting(&self, live_id: &str, platform: &str, host: &str) -> Result<Option<String>> {
        if live_id.is_empty() {
            return Ok(None);
        }
        let clock = self.options.clock.as_ref();
        let now = self.now();
        let today = time::wall_date(now.local_wall(clock));
        let empty = Value::Object(Map::new());
        for meeting_dir in is_dir_entries(&self.base_path)? {
            let meta_path = meeting_dir.join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let attempt = (|| -> Result<Option<String>> {
                let meta = read_json(&meta_path)?;
                if get(&meta, "ended_at")?.is_none_or(Value::is_null) {
                    return Ok(None);
                }
                let source = self.canonical(get(&meta, "source")?.unwrap_or(&Value::Null));
                if source != Value::from(self.canonical_str(platform)) {
                    return Ok(None);
                }
                if get(get_or_empty(&meta, "conference", &empty)?, "live_id")?
                    .is_none_or(|v| *v != Value::from(live_id))
                {
                    return Ok(None);
                }
                let conference = get_or_empty(&meta, "conference", &empty)?;
                if self.stored_host(conference)? != Value::from(host) {
                    return Ok(None);
                }
                let ended_at = parse_iso(item(&meta, "ended_at")?)?;
                if now.minus(&ended_at)? > SAME_ROOM_REOPEN_WINDOW_MICROS {
                    return Ok(None);
                }
                let created_at = parse_iso(item(&meta, "created_at")?)?;
                if time::wall_date(created_at.local_wall(clock)) == today {
                    return Ok(Some(item(&meta, "id")?.py_str()));
                }
                Ok(None)
            })();
            match attempt {
                Ok(Some(id)) => return Ok(Some(id)),
                Ok(None) => {}
                Err(e) if e.is_value_error() || matches!(e, StorageError::Key(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    fn reopen_meeting(&self, inner: &mut Inner, meeting_id: &str) -> Result<bool> {
        let meeting_path = self.base_path.join(meeting_id);
        let meta_path = meeting_path.join("meta.json");
        if !meta_path.exists() {
            return Ok(false);
        }
        let mut meta = read_json(&meta_path)?;
        set(&mut meta, "ended_at", Value::Null)?;
        set(&mut meta, "end_reason", Value::Null)?;
        set(&mut meta, "updated_at", self.now_iso())?;
        // Keep the original chunk duration so old segments stay aligned.
        if let Some(chunk) = inner.chunk_duration_ms
            && !pyops::contains(&meta, "chunk_duration_ms")?
        {
            set(&mut meta, "chunk_duration_ms", Value::from(chunk))?;
        }
        save_json(&meta_path, &meta)?;
        let index_path = self.base_path.join("index.json");
        if index_path.exists() {
            let mut index = read_json(&index_path)?;
            let kept = match item(&index, "meetings")? {
                Value::Array(meetings) => meetings
                    .iter()
                    .map(|m| item(m, "id").map(|id| (m, *id != Value::from(meeting_id))))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|(_, keep)| *keep)
                    .map(|(m, _)| m.clone())
                    .collect(),
                other => return Err(StorageError::Type(format!("'{}' object is not iterable", other.type_name()))),
            };
            set(&mut index, "meetings", Value::Array(kept))?;
            save_json(&index_path, &index)?;
        }
        let transcript = meeting_path.join("transcript.jsonl");
        inner.current_meeting_id = Some(meeting_id.to_owned());
        inner.current_meeting_path = Some(meeting_path);
        inner.segment_counter = if transcript.exists() {
            let content = read_text(&transcript)?;
            let content = py::strip(&content);
            if content.is_empty() { 0 } else { i64::try_from(content.split('\n').count()).unwrap_or(i64::MAX) }
        } else {
            0
        };
        self.append_system_event(inner, "session_resume", "Сессия возобновлена")?;
        Ok(true)
    }

    fn end_current_meeting(self: &Arc<Self>, inner: &mut Inner, end_reason: &str) -> Result<Option<String>> {
        let Some(meeting_path) = inner.current_meeting_path.clone() else { return Ok(None) };
        let mut meta = Self::load_meta(inner)?;
        if self.is_conference(get(&meta, "source")?) && end_reason != "session_end" {
            self.append_system_event(inner, "session_pause", "Сессия приостановлена")?;
        }
        let now = self.now();
        set(&mut meta, "ended_at", Value::from(now.isoformat()))?;
        set(&mut meta, "end_reason", Value::from(end_reason))?;
        let created = parse_iso(item(&meta, "created_at")?)?;
        let duration = now.minus(&created)? / 1_000_000;
        set(item_mut(&mut meta, "stats")?, "duration_seconds", Value::from(duration))?;
        Self::save_meta(inner, &meta)?;
        self.update_index(&meta)?;
        // AI work must not delay a lifecycle transition; pin the closed recording.
        if self.wants_generated_title(&meta)? {
            self.submit_title(inner, meeting_path);
        }
        Ok(inner.current_meeting_id.clone())
    }

    fn submit_title(self: &Arc<Self>, inner: &mut Inner, meeting_path: PathBuf) {
        let worker = inner.titles.get_or_insert_with(|| {
            let (jobs, queue) = mpsc::channel::<TitleJob>();
            let shared = Arc::clone(self);
            let thread = std::thread::Builder::new()
                .name("recog-title".into())
                .spawn(move || {
                    for job in queue {
                        match job {
                            TitleJob::Meeting(path) => shared.finish_meeting_title(&path),
                            TitleJob::Barrier(done) => {
                                let _ = done.send(());
                            }
                        }
                    }
                })
                .unwrap_or_else(|e| panic!("cannot start the title thread: {e}"));
            TitleWorker { jobs, thread }
        });
        let _ = worker.jobs.send(TitleJob::Meeting(meeting_path));
    }

    fn finish_meeting_title(&self, meeting_path: &Path) {
        let result = (|| -> Result<()> {
            let Some(title) = self.generate_title_for_meeting(meeting_path) else { return Ok(()) };
            let _inner = self.lock();
            let meta_path = meeting_path.join("meta.json");
            let mut meta = read_json(&meta_path)?;
            if !get(&meta, "ended_at")?.is_some_and(Value::truthy) || get(&meta, "title")?.is_some_and(Value::truthy) {
                return Ok(());
            }
            set(&mut meta, "title", Value::from(title.title))?;
            set(&mut meta, "title_source", Value::from(title.provider))?;
            set(&mut meta, "title_model", title.model)?;
            set(&mut meta, "title_settings_revision", title.settings_revision)?;
            save_json(&meta_path, &meta)?;
            self.update_index(&meta)
        })();
        if let Err(e) = result {
            self.eprint(&format!("Warning: Title persistence failed: {e}"));
        }
    }

    fn generate_title_for_meeting(&self, meeting_path: &Path) -> Option<GeneratedTitle> {
        let Some(generator) = self.options.title_generator.clone() else {
            self.print("Warning: title_generator not available");
            return None;
        };
        let attempt = (|| -> Result<Option<GeneratedTitle>, String> {
            let transcript = transcript_as_text(meeting_path).map_err(|e| e.to_string())?;
            if transcript.is_empty() {
                return Ok(None);
            }
            let meta = read_json(&meeting_path.join("meta.json")).map_err(|e| e.to_string())?;
            let empty = Value::Object(Map::new());
            let participants = get_or_empty(&meta, "participants", &empty).map_err(|e| e.to_string())?;
            let mut user_names = Vec::new();
            let mut names = Vec::new();
            for (pid, info) in items(participants, "items").map_err(|e| e.to_string())?.iter() {
                let name =
                    get(info, "name").map_err(|e| e.to_string())?.cloned().unwrap_or_else(|| Value::from(pid.as_str()));
                if get(info, "is_local").map_err(|e| e.to_string())?.is_some_and(Value::truthy) {
                    user_names.push(name.clone());
                }
                names.push(name);
            }
            let request = TitleRequest {
                data_dir: self.base_path.parent().unwrap_or(Path::new("")).to_path_buf(),
                meeting_path: meeting_path.to_path_buf(),
                transcript,
                user_name: pyops::join_strings(&user_names).map_err(|e| e.to_string())?,
                participants: pyops::join_strings(&names).map_err(|e| e.to_string())?,
                meeting_title: match get(&meta, "title").map_err(|e| e.to_string())? {
                    Some(title) if title.truthy() => title.py_str(),
                    _ => String::new(),
                },
            };
            let title = generator.generate(&request)?;
            Ok(title.filter(|t| !t.title.is_empty()))
        })();
        match attempt {
            Ok(Some(title)) => {
                self.print(&format!("Generated title: {}", title.title));
                Some(title)
            }
            Ok(None) => None,
            Err(e) => {
                self.print(&format!("Warning: Title generation failed: {e}"));
                None
            }
        }
    }

    fn append_system_event(&self, inner: &mut Inner, event_type: &str, message: &str) -> Result<()> {
        let Some(meeting_path) = inner.current_meeting_path.clone() else { return Ok(()) };
        inner.segment_counter += 1;
        let segment_id = inner.segment_counter;
        let now = self.now();
        let mut event = Map::new();
        event.insert("id", Value::from(segment_id));
        event.insert("ts", Value::from(now.isoformat()));
        event.insert("ts_display", Value::from(time::hms(now.local_wall(self.options.clock.as_ref()))));
        event.insert("speaker", Value::from("Система"));
        event.insert("speaker_id", Value::from("__system__"));
        event.insert("text", Value::from(message));
        event.insert("is_local", Value::Bool(false));
        event.insert("cont", Value::Bool(false));
        event.insert("uncertain", Value::Bool(false));
        event.insert("uncertain_reason", Value::from(""));
        event.insert("event_type", Value::from(event_type));
        append_line(&meeting_path.join("transcript.jsonl"), &pyjson::dumps(&Value::Object(event), None))?;
        let meta_path = meeting_path.join("meta.json");
        let mut meta = read_json(&meta_path)?;
        set(item_mut(&mut meta, "stats")?, "segment_count", Value::from(segment_id))?;
        set(&mut meta, "updated_at", Value::from(now.isoformat()))?;
        save_json(&meta_path, &meta)
    }

    fn update_index(&self, meta: &Value) -> Result<()> {
        let index_path = self.base_path.join("index.json");
        let mut index = if index_path.exists() {
            read_json(&index_path)?
        } else {
            pyjson::loads(r#"{"meetings": []}"#).unwrap_or(Value::Null)
        };
        let id = item(meta, "id")?;
        let mut meetings = match item(&index, "meetings")? {
            Value::Array(list) => {
                let mut kept = Vec::new();
                for m in list {
                    if !item(m, "id")?.py_eq(id) {
                        kept.push(m.clone());
                    }
                }
                kept
            }
            other => return Err(StorageError::Type(format!("'{}' object is not iterable", other.type_name()))),
        };
        let stats = item(meta, "stats")?;
        if pyops::equals_int(item(stats, "segment_count")?, 0) {
            set(&mut index, "meetings", Value::Array(meetings))?;
            save_json(&index_path, &index)?;
            let meeting_path = self.base_path.join(id.py_str());
            if meeting_path.exists() {
                let _ = fs::remove_dir_all(&meeting_path);
            }
            return Ok(());
        }
        let empty = Value::Object(Map::new());
        let conference = match get(meta, "conference")? {
            Some(c) => c,
            None => get_or_empty(meta, "ktalk", &empty)?,
        };
        let names = items(item(meta, "participants")?, "values")?
            .values()
            .map(|p| item(p, "name").cloned())
            .collect::<Result<Vec<_>>>()?;
        let mut entry = Map::new();
        entry.insert("id", id.clone());
        entry.insert("created_at", item(meta, "created_at")?.clone());
        entry.insert("ended_at", item(meta, "ended_at")?.clone());
        entry.insert("duration_seconds", item(stats, "duration_seconds")?.clone());
        entry.insert("title", item(meta, "title")?.clone());
        entry.insert("source", item(meta, "source")?.clone());
        entry.insert("end_reason", item(meta, "end_reason")?.clone());
        entry.insert("ktalk_live_url", get(conference, "live_url")?.cloned().unwrap_or(Value::Null));
        entry.insert("segment_count", item(stats, "segment_count")?.clone());
        entry.insert("participants", Value::Array(names));
        meetings.push(Value::Object(entry));
        sort_desc(&mut meetings, |m| item(m, "created_at").cloned())?;
        set(&mut index, "meetings", Value::Array(meetings))?;
        save_json(&index_path, &index)
    }
}

fn parse_iso(value: &Value) -> Result<PyDateTime> {
    match value {
        Value::Str(s) => Ok(time::fromisoformat(s)?),
        other => Err(StorageError::Type(format!("fromisoformat: argument must be str, not {}", other.type_name()))),
    }
}

fn append_line(path: &Path, line: &str) -> Result<()> {
    let mut file =
        fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| StorageError::os(&e, path))?;
    file.write_all(format!("{line}\n").as_bytes()).map_err(|e| StorageError::os(&e, path))
}

/// `_get_transcript_as_text(meeting_path)`.
fn transcript_as_text(meeting_path: &Path) -> Result<String> {
    let path = meeting_path.join("transcript.jsonl");
    if !path.exists() {
        return Ok(String::new());
    }
    let content = read_text(&path)?;
    let mut lines = Vec::new();
    for line in py::strip(&content).split('\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(segment) = pyjson::loads(line) else { continue };
        if get(&segment, "speaker_id")?.is_some_and(|v| *v == Value::from("__system__")) {
            continue;
        }
        let field = |key: &str, default: &str| -> Result<String> {
            Ok(get(&segment, key)?.map_or_else(|| default.to_owned(), Value::py_str))
        };
        lines.push(format!("[{}] {}: {}", field("ts_display", "")?, field("speaker", "Unknown")?, field("text", "")?));
    }
    Ok(lines.join("\n"))
}

/// The transcript's rows, as `get_current_transcript` and `get_meeting` read them.
fn read_rows(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = read_text(path)?;
    let content = py::strip(&content);
    if content.is_empty() {
        return Ok(Vec::new());
    }
    content.split('\n').filter(|l| !l.is_empty()).map(loads).collect()
}

fn copy2(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).map_err(|e| StorageError::os(&e, from))?;
    let meta = fs::metadata(from).map_err(|e| StorageError::os(&e, from))?;
    let times = fs::FileTimes::new()
        .set_accessed(meta.accessed().map_err(|e| StorageError::os(&e, from))?)
        .set_modified(meta.modified().map_err(|e| StorageError::os(&e, from))?);
    let file = fs::OpenOptions::new().write(true).open(to).map_err(|e| StorageError::os(&e, to))?;
    file.set_times(times).map_err(|e| StorageError::os(&e, to))
}

impl MeetingStorage {
    /// `MeetingStorage(base_path, external_paths)`: the directory is created.
    ///
    /// # Errors
    /// The directory cannot be created.
    pub fn new(base_path: &Path, options: StorageOptions) -> Result<Self> {
        fs::create_dir_all(base_path).map_err(|e| StorageError::os(&e, base_path))?;
        let inner = Inner {
            current_meeting_id: None,
            current_meeting_path: None,
            segment_counter: 0,
            mode: Map::new(),
            chunk_duration_ms: None,
            titles: None,
        };
        Ok(Self { shared: Arc::new(Shared { base_path: base_path.to_path_buf(), options, inner: Mutex::new(inner) }) })
    }

    #[must_use]
    pub fn base_path(&self) -> &Path {
        &self.shared.base_path
    }

    #[must_use]
    pub fn current_meeting_id(&self) -> Option<String> {
        self.shared.lock().current_meeting_id.clone()
    }

    #[must_use]
    pub fn current_meeting_path(&self) -> Option<PathBuf> {
        self.shared.lock().current_meeting_path.clone()
    }

    /// `is_conference_source(source)`.
    #[must_use]
    pub fn is_conference_source(&self, source: Option<&str>) -> bool {
        self.shared.is_conference(source.map(Value::from).as_ref())
    }

    /// `start_session(...)`: close stale sessions, delete old audio, open the first
    /// meeting (an orphan awaiting a conference when `ktalk`).
    ///
    /// # Errors
    /// As Python raises.
    pub fn start_session(&self, mode: &SessionMode) -> Result<String> {
        self.shared.close_stale_sessions()?;
        self.cleanup_old_audio(24.0)?;
        let mut inner = self.shared.lock();
        let mut map = Map::new();
        map.insert("mic", Value::Bool(mode.mic));
        map.insert("system_audio", Value::Bool(mode.system_audio));
        map.insert("ktalk", Value::Bool(mode.ktalk));
        map.insert("screencapture", Value::Bool(mode.screencapture));
        map.insert("system_audio_backend", Value::from(mode.system_audio_backend.as_str()));
        if mode.ktalk_only {
            map.insert("ktalk_only", Value::Bool(true));
        }
        inner.mode = map;
        inner.chunk_duration_ms = Some(mode.chunk_duration_ms);
        let source = if mode.ktalk {
            "orphan"
        } else if mode.system_audio && !mode.mic {
            "diarized"
        } else {
            "local"
        };
        self.shared.create_meeting(&mut inner, source, None)
    }

    /// `on_meeting_join(url, title, platform)`: the extension entered a conference.
    ///
    /// # Errors
    /// As Python raises.
    pub fn on_meeting_join(&self, url: &str, title: Option<&str>, platform: &str) -> Result<Option<String>> {
        let shared = &self.shared;
        let mut guard = shared.lock();
        let inner = &mut *guard;
        let (parsed, _) = urlparse(url).map_err(StorageError::Value)?;
        let mut live_id = parsed.path.trim_matches('/').rsplit('/').next().unwrap_or("").to_owned();
        if live_id.is_empty() {
            // re.search(r"#/room/([^/?]+)", url): the first occurrence with a room.
            if let Some(room) = url
                .match_indices("#/room/")
                .map(|(at, _)| url[at + 7..].chars().take_while(|c| *c != '/' && *c != '?').collect::<String>())
                .find(|room| !room.is_empty())
            {
                live_id = room;
            }
        }
        let host = parsed.netloc.clone();
        let empty = Value::Object(Map::new());
        let mut need_new_or_reopen = false;
        if inner.current_meeting_path.is_none() {
            need_new_or_reopen = true;
        } else {
            let mut meta = Shared::load_meta(inner)?;
            let current_live_id =
                get(get_or_empty(&meta, "conference", &empty)?, "live_id")?.cloned().unwrap_or(Value::Null);
            let current_host = shared.stored_host(get_or_empty(&meta, "conference", &empty)?)?;
            let changed = current_live_id.truthy()
                && (current_live_id != Value::from(live_id.as_str())
                    || current_host != Value::from(host.as_str())
                    || shared.canonical(get(&meta, "source")?.unwrap_or(&Value::Null))
                        != Value::from(shared.canonical_str(platform)));
            let orphan = || -> Result<bool> { Ok(*item(&meta, "source")? == Value::from("orphan")) };
            let segment_count = || -> Result<Value> { Ok(item(item(&meta, "stats")?, "segment_count")?.clone()) };
            if changed {
                shared.end_current_meeting(inner, "changed_meeting")?;
                need_new_or_reopen = true;
            } else if orphan()? && compare_int(&segment_count()?, 0, ">")?.is_gt() {
                shared.end_current_meeting(inner, "meeting_join")?;
                need_new_or_reopen = true;
            } else if orphan()? && pyops::equals_int(&segment_count()?, 0) {
                if let Some(existing) = shared.find_same_day_meeting(&live_id, platform, &host)? {
                    let orphan_path = inner.current_meeting_path.clone();
                    shared.reopen_meeting(inner, &existing)?;
                    if let Some(path) = orphan_path.filter(|p| p.exists()) {
                        let _ = fs::remove_dir_all(path);
                    }
                    shared.print(&format!("Reopened same-day meeting: {existing}"));
                } else {
                    set(&mut meta, "source", Value::from(shared.canonical_str(platform)))?;
                    Shared::save_meta(inner, &meta)?;
                }
            }
        }
        if need_new_or_reopen {
            if let Some(existing) = shared.find_same_day_meeting(&live_id, platform, &host)? {
                shared.reopen_meeting(inner, &existing)?;
                shared.print(&format!("Reopened same-day meeting: {existing}"));
            } else {
                shared.create_meeting(inner, platform, None)?;
            }
        }
        let mut meta = Shared::load_meta(inner)?;
        let conference = item_mut(&mut meta, "conference")?;
        set(conference, "live_url", Value::from(url))?;
        set(conference, "live_id", Value::from(live_id))?;
        set(conference, "host", Value::from(host))?;
        if let Some(title) = title.filter(|t| !t.is_empty())
            && !get(&meta, "title")?.is_some_and(Value::truthy)
        {
            set(&mut meta, "title", Value::from(title))?;
            set(&mut meta, "title_source", Value::from(platform))?;
        }
        set(&mut meta, "updated_at", shared.now_iso())?;
        Shared::save_meta(inner, &meta)?;
        Ok(inner.current_meeting_id.clone())
    }

    /// `on_ktalk_join(url, title)`: the deprecated alias for KTalk.
    ///
    /// # Errors
    /// As [`Self::on_meeting_join`].
    pub fn on_ktalk_join(&self, url: &str, title: Option<&str>) -> Result<Option<String>> {
        self.on_meeting_join(url, title, "ktalk")
    }

    /// `on_ktalk_leave(reason)`: end the meeting and open an orphan after it.
    ///
    /// # Errors
    /// As Python raises.
    pub fn on_ktalk_leave(&self, reason: &str) -> Result<String> {
        let mut guard = self.shared.lock();
        let inner = &mut *guard;
        if inner.current_meeting_path.is_none() {
            return self.shared.create_meeting(inner, "orphan", None);
        }
        let prev = self.shared.end_current_meeting(inner, reason)?;
        self.shared.create_meeting(inner, "orphan", prev.as_deref())
    }

    /// `split_on_silence_timeout()`: an orphan with segments ends and a new one opens.
    ///
    /// # Errors
    /// As Python raises.
    pub fn split_on_silence_timeout(&self) -> Result<Option<String>> {
        let mut guard = self.shared.lock();
        let inner = &mut *guard;
        if inner.current_meeting_path.is_none() {
            return Ok(None);
        }
        let meta = Shared::load_meta(inner)?;
        if *item(&meta, "source")? != Value::from("orphan") {
            return Ok(None);
        }
        if pyops::equals_int(item(item(&meta, "stats")?, "segment_count")?, 0) {
            return Ok(None);
        }
        let prev = self.shared.end_current_meeting(inner, "silence_timeout")?;
        self.shared.create_meeting(inner, "orphan", prev.as_deref()).map(Some)
    }

    /// `end_session()`: end the meeting, then wait for title jobs outside the lock.
    ///
    /// # Errors
    /// As Python raises.
    pub fn end_session(&self) -> Result<Option<String>> {
        let (meeting_id, titles) = {
            let mut guard = self.shared.lock();
            let inner = &mut *guard;
            // A failure leaves the meeting current, as Python's exception skips the rest.
            let meeting_id = if inner.current_meeting_path.is_some() {
                self.shared.end_current_meeting(inner, "session_end")?
            } else {
                None
            };
            inner.current_meeting_id = None;
            inner.current_meeting_path = None;
            (meeting_id, inner.titles.take())
        };
        if let Some(worker) = titles {
            drop(worker.jobs);
            let _ = worker.thread.join();
        }
        Ok(meeting_id)
    }

    /// Wait until the title jobs submitted so far are done (Python has no such call;
    /// its executor drains only at `end_session`).
    pub fn drain_titles(&self) {
        let (done, wait) = mpsc::channel();
        {
            let inner = self.shared.lock();
            let Some(worker) = inner.titles.as_ref() else { return };
            if worker.jobs.send(TitleJob::Barrier(done)).is_err() {
                return;
            }
        }
        let _ = wait.recv();
    }

    /// `append_segment(...)`: the segment's id.
    ///
    /// # Errors
    /// As Python raises.
    pub fn append_segment(&self, segment: &NewSegment) -> Result<i64> {
        let shared = &self.shared;
        let mut guard = shared.lock();
        let inner = &mut *guard;
        if inner.current_meeting_path.is_none() {
            shared.create_meeting(inner, "orphan", None)?;
        }
        let meeting_path = inner.current_meeting_path.clone().unwrap_or_default();
        inner.segment_counter += 1;
        let segment_id = inner.segment_counter;
        let now = shared.now();
        let spoken = segment.captured_at.map_or(now, time::from_timestamp);
        let mut row = Map::new();
        row.insert("id", Value::from(segment_id));
        row.insert("ts", Value::from(spoken.isoformat()));
        row.insert("ts_display", Value::from(time::hms(spoken.local_wall(shared.options.clock.as_ref()))));
        row.insert("speaker", Value::from(segment.speaker.as_str()));
        row.insert("speaker_id", Value::from(segment.speaker_id.as_str()));
        row.insert("text", Value::from(segment.text.as_str()));
        row.insert("is_local", Value::Bool(segment.is_local));
        row.insert("cont", Value::Bool(segment.is_continuation));
        row.insert("uncertain", Value::Bool(segment.uncertain));
        row.insert("uncertain_reason", Value::from(segment.uncertain_reason.as_str()));
        row.insert("audio_offset_ms", Value::from(segment.audio_offset_ms));
        row.insert("refined", Value::Bool(false));
        append_line(&meeting_path.join("transcript.jsonl"), &pyjson::dumps(&Value::Object(row), None))?;

        let meta_path = meeting_path.join("meta.json");
        let mut meta = read_json(&meta_path)?;
        let words = i64::try_from(py::split_whitespace(&segment.text).count()).unwrap_or(i64::MAX);
        let stats = item_mut(&mut meta, "stats")?;
        set(stats, "segment_count", Value::from(segment_id))?;
        let total = add_int(item(stats, "words_count")?, words)?;
        set(stats, "words_count", total)?;
        set(&mut meta, "updated_at", Value::from(now.isoformat()))?;
        let first_seen = Value::from(now.isoformat());
        let (speaker, speaker_id) = (segment.speaker.as_str(), segment.speaker_id.as_str());
        let participants = item_mut(&mut meta, "participants")?;
        let known = pyops::contains(participants, speaker_id)?;
        let new_participant = || {
            let mut p = Map::new();
            p.insert("name", Value::from(speaker));
            p.insert("is_local", Value::Bool(segment.is_local));
            p.insert("first_seen", first_seen.clone());
            Value::Object(p)
        };
        if segment.is_local && !known {
            let mut existing_local = None;
            for (pid, p) in items(participants, "items")?.iter() {
                if get(p, "is_local")?.is_some_and(Value::truthy) {
                    existing_local = Some(pid.clone());
                    break;
                }
            }
            if let Some(local) = existing_local.filter(|pid| !pid.is_empty()) {
                // Already a local participant: rename it instead of adding another.
                let entry = item_mut(participants, &local)?;
                if !speaker.is_empty() && *item(entry, "name")? != Value::from(speaker) {
                    set(entry, "name", Value::from(speaker))?;
                }
            } else {
                set(participants, speaker_id, new_participant())?;
            }
        } else if !known {
            set(participants, speaker_id, new_participant())?;
        } else {
            let entry = item_mut(participants, speaker_id)?;
            if *item(entry, "name")? != Value::from(speaker) {
                set(entry, "name", Value::from(speaker))?;
            }
        }
        save_json(&meta_path, &meta)?;
        Ok(segment_id)
    }

    /// `update_segments(updates, meeting_path)`: rewrite matching rows in place; an
    /// explicit `meeting_path` pins the meeting against a switch in between.
    ///
    /// # Errors
    /// As Python raises.
    pub fn update_segments(&self, updates: &[SegmentUpdate], meeting_path: Option<&Path>) -> Result<()> {
        let target = match meeting_path {
            Some(path) => Some(path.to_path_buf()),
            None => self.shared.lock().current_meeting_path.clone(),
        };
        let Some(target) = target else { return Ok(()) };
        if updates.is_empty() {
            return Ok(());
        }
        let _guard = self.shared.lock();
        let transcript_path = target.join("transcript.jsonl");
        if !transcript_path.exists() {
            return Ok(());
        }
        let find = |id: &Value| updates.iter().rev().find(|u| pyops::equals_int(id, u.segment_id));
        let content = read_text(&transcript_path)?;
        let mut new_lines = Vec::new();
        let mut words_delta: i64 = 0;
        for line in py::strip(&content).split('\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(mut segment) = pyjson::loads(line) else {
                new_lines.push(line.to_owned());
                continue;
            };
            let id = get(&segment, "id")?.cloned().unwrap_or(Value::Null);
            if let Some(update) = find(&id) {
                if let Some(speaker) = &update.speaker {
                    set(&mut segment, "speaker", Value::from(speaker.as_str()))?;
                    set(&mut segment, "speaker_id", Value::from(speaker.as_str()))?;
                }
                if let Some(uncertain) = update.uncertain {
                    set(&mut segment, "uncertain", Value::Bool(uncertain))?;
                }
                if let Some(reason) = &update.uncertain_reason {
                    set(&mut segment, "uncertain_reason", Value::from(reason.as_str()))?;
                }
                if let Some(text) = &update.text {
                    let old = get(&segment, "text")?.map_or_else(String::new, Value::py_str);
                    let delta = py::split_whitespace(text).count() as i64 - py::split_whitespace(&old).count() as i64;
                    words_delta += delta;
                    set(&mut segment, "text", Value::from(text.as_str()))?;
                    set(&mut segment, "refined", Value::Bool(true))?;
                }
            }
            new_lines.push(pyjson::dumps(&segment, None));
        }
        let temp = target.join("transcript.jsonl.tmp");
        fs::write(&temp, format!("{}\n", new_lines.join("\n"))).map_err(|e| StorageError::os(&e, &temp))?;
        fs::rename(&temp, &transcript_path).map_err(|e| StorageError::os(&e, &transcript_path))?;
        if words_delta != 0 {
            let meta_path = target.join("meta.json");
            if meta_path.exists() {
                let mut meta = read_json(&meta_path)?;
                let stats = items(&meta, "setdefault")?.get("stats").cloned();
                if stats.is_none() {
                    set(&mut meta, "stats", Value::Object(Map::new()))?;
                }
                let stats = item_mut(&mut meta, "stats")?;
                let current = pyops::to_int(get(stats, "words_count")?.unwrap_or(&Value::Int(0)))?;
                set(stats, "words_count", Value::from((current + words_delta).max(0)))?;
                save_json(&meta_path, &meta)?;
            }
        }
        Ok(())
    }

    /// `update_title(title)`: only an unset title or a "Joining…" placeholder changes.
    ///
    /// # Errors
    /// As Python raises.
    pub fn update_title(&self, title: &str) -> Result<()> {
        let inner = self.shared.lock();
        if inner.current_meeting_path.is_none() || title.is_empty() {
            return Ok(());
        }
        let mut meta = Shared::load_meta(&inner)?;
        let current = match get(&meta, "title")? {
            Some(t) if t.truthy() => t.clone(),
            _ => Value::from(""),
        };
        let placeholder = match &current {
            Value::Str(s) => s.is_empty() || s.starts_with("Joining"),
            other => return Err(pyops::attribute_error(other, "startswith")),
        };
        if placeholder {
            set(&mut meta, "title", Value::from(title))?;
            set(&mut meta, "title_source", Value::from("ktalk"))?;
            set(&mut meta, "updated_at", self.shared.now_iso())?;
            Shared::save_meta(&inner, &meta)?;
        }
        Ok(())
    }

    /// `update_participants(participants)`: `{id: {"name", "is_local", …}}` from the
    /// conference.
    ///
    /// # Errors
    /// As Python raises.
    pub fn update_participants(&self, participants: &Map) -> Result<()> {
        let inner = self.shared.lock();
        if inner.current_meeting_path.is_none() {
            return Ok(());
        }
        let mut meta = Shared::load_meta(&inner)?;
        let now = self.shared.now_iso();
        for (pid, pinfo) in participants.iter() {
            let is_local = get(pinfo, "is_local")?.cloned().unwrap_or(Value::Bool(false));
            let stored = item_mut(&mut meta, "participants")?;
            if !pyops::contains(stored, pid)? {
                if is_local.truthy() {
                    let mut existing = None;
                    for (id, p) in items(stored, "items")?.iter() {
                        if get(p, "is_local")?.is_some_and(Value::truthy) {
                            existing = Some(id.clone());
                            break;
                        }
                    }
                    if let Some(existing) = existing.filter(|id| !id.is_empty() && id != pid) {
                        // The conference's name of the local user is the more accurate.
                        if let Some(name) = get(pinfo, "name")?.filter(|n| n.truthy()) {
                            set(item_mut(stored, &existing)?, "name", name.clone())?;
                        }
                        continue;
                    }
                }
                let mut p = Map::new();
                p.insert(
                    "name",
                    get(pinfo, "name")?.cloned().unwrap_or_else(|| Value::from(format!("Participant_{pid}"))),
                );
                p.insert("is_local", is_local);
                p.insert("first_seen", now.clone());
                set(stored, pid, Value::Object(p))?;
            } else if let Some(name) = get(pinfo, "name")?.filter(|n| n.truthy()) {
                let entry = item_mut(stored, pid)?;
                if !name.py_eq(item(entry, "name")?) {
                    set(entry, "name", name.clone())?;
                }
            }
        }
        set(&mut meta, "updated_at", now)?;
        Shared::save_meta(&inner, &meta)
    }

    /// `get_current_transcript()`.
    ///
    /// # Errors
    /// As Python raises.
    pub fn get_current_transcript(&self) -> Result<Vec<Value>> {
        let Some(path) = self.current_meeting_path() else { return Ok(Vec::new()) };
        read_rows(&path.join("transcript.jsonl"))
    }

    /// `list_meetings(include_active, refresh_cache)`: ended meetings from the index,
    /// the external directories' (cached) indexes, and the active ones, newest first.
    ///
    /// # Errors
    /// As Python raises.
    pub fn list_meetings(&self, include_active: bool, refresh_cache: bool) -> Result<Vec<Value>> {
        let index_path = self.shared.base_path.join("index.json");
        let mut meetings: Vec<Value> = Vec::new();
        if index_path.exists() {
            let data = read_json(&index_path)?;
            meetings = match get(&data, "meetings")? {
                Some(Value::Array(list)) => list.clone(),
                None => Vec::new(),
                Some(other) => {
                    return Err(StorageError::Type(format!("'{}' object is not iterable", other.type_name())));
                }
            };
        }
        for (ext_path, source_override) in &self.shared.options.external_paths {
            if ext_path.exists() {
                meetings.extend(self.external_index(ext_path, source_override.as_deref(), refresh_cache)?);
            }
        }
        if include_active {
            let mut active = self.list_active_meetings()?;
            active.extend(meetings);
            meetings = active;
        }
        sort_desc(&mut meetings, |m| Ok(get(m, "created_at")?.cloned().unwrap_or_else(|| Value::from(""))))?;
        meetings
            .into_iter()
            .map(|meeting| {
                let source = self.shared.canonical(get(&meeting, "source")?.unwrap_or(&Value::Null));
                let mut copy = items(&meeting, "items")?.clone();
                copy.insert("source", source);
                Ok(Value::Object(copy))
            })
            .collect()
    }

    fn external_index(&self, ext_path: &Path, source_override: Option<&str>, refresh: bool) -> Result<Vec<Value>> {
        let key_text = format!("{}:{}", ext_path.display(), source_override.unwrap_or("None"));
        let digest = Md5::digest(key_text.as_bytes());
        let cache_key: String = digest.iter().map(|b| format!("{b:02x}")).collect::<String>()[..8].to_owned();
        let cache_dir = &self.shared.options.external_index_cache_dir;
        let cache_file = cache_dir.join(format!("{cache_key}.json"));
        if !refresh && cache_file.exists() {
            let cached = (|| -> Option<Vec<Value>> {
                let cache_mtime = mtime_seconds(&fs::metadata(&cache_file).ok()?)?;
                let dir_mtime = mtime_seconds(&fs::metadata(ext_path).ok()?)?;
                if cache_mtime < dir_mtime {
                    return None;
                }
                let data = read_json(&cache_file).ok()?;
                match get(&data, "meetings").ok()? {
                    Some(Value::Array(list)) => Some(list.clone()),
                    _ => Some(Vec::new()),
                }
            })();
            if let Some(meetings) = cached {
                return Ok(meetings);
            }
        }
        let meetings = self.scan_external_meetings(ext_path, source_override)?;
        fs::create_dir_all(cache_dir).map_err(|e| StorageError::os(&e, cache_dir))?;
        let mut cache = Map::new();
        cache.insert("path", Value::from(ext_path.to_string_lossy().into_owned()));
        cache.insert("source_override", Value::from(source_override));
        cache.insert("meetings", Value::Array(meetings.clone()));
        cache.insert("updated_at", self.shared.now_iso());
        fs::write(&cache_file, pyjson::dumps(&Value::Object(cache), Some(2)))
            .map_err(|e| StorageError::os(&e, &cache_file))?;
        Ok(meetings)
    }

    fn scan_external_meetings(&self, path: &Path, source_override: Option<&str>) -> Result<Vec<Value>> {
        let mut meetings = Vec::new();
        let empty = Value::Object(Map::new());
        for meeting_dir in is_dir_entries(path)? {
            let meta_path = meeting_dir.join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let attempt = (|| -> Result<Value> {
                let meta = read_json(&meta_path)?;
                let source = match source_override.filter(|s| !s.is_empty()) {
                    Some(s) => Value::from(s),
                    None => get(&meta, "source")?.cloned().unwrap_or_else(|| Value::from("unknown")),
                };
                let stats = get_or_empty(&meta, "stats", &empty)?;
                let conference = match get(&meta, "conference")? {
                    Some(c) => c,
                    None => get_or_empty(&meta, "ktalk", &empty)?,
                };
                let names = items(get_or_empty(&meta, "participants", &empty)?, "items")?
                    .iter()
                    .map(|(k, p)| Ok(get(p, "name")?.cloned().unwrap_or_else(|| Value::from(k.as_str()))))
                    .collect::<Result<Vec<_>>>()?;
                let mut entry = Map::new();
                entry.insert("id", item(&meta, "id")?.clone());
                entry.insert("created_at", get(&meta, "created_at")?.cloned().unwrap_or(Value::Null));
                entry.insert("ended_at", get(&meta, "ended_at")?.cloned().unwrap_or(Value::Null));
                entry.insert("duration_seconds", get(stats, "duration_seconds")?.cloned().unwrap_or(Value::Int(0)));
                entry.insert("title", get(&meta, "title")?.cloned().unwrap_or(Value::Null));
                entry.insert("source", self.shared.canonical(&source));
                entry.insert("end_reason", get(&meta, "end_reason")?.cloned().unwrap_or(Value::Null));
                entry.insert("conference_url", get(conference, "live_url")?.cloned().unwrap_or(Value::Null));
                entry.insert("segment_count", get(stats, "segment_count")?.cloned().unwrap_or(Value::Int(0)));
                entry.insert("participants", Value::Array(names));
                entry.insert("_external", Value::Bool(true));
                entry.insert("_external_path", Value::from(path.to_string_lossy().into_owned()));
                Ok(Value::Object(entry))
            })();
            match attempt {
                Ok(entry) => meetings.push(entry),
                Err(e @ (StorageError::Json(_) | StorageError::Key(_))) => {
                    self.shared.eprint(&format!("Warning: Failed to load {}: {e}", meta_path.display()));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(meetings)
    }

    /// `list_active_meetings()`: meetings not ended, newest first.
    ///
    /// # Errors
    /// As Python raises.
    pub fn list_active_meetings(&self) -> Result<Vec<Value>> {
        let mut active = Vec::new();
        let empty = Value::Object(Map::new());
        for meeting_dir in is_dir_entries(&self.shared.base_path)? {
            let meta_path = meeting_dir.join("meta.json");
            if !meta_path.exists() {
                continue;
            }
            let attempt = (|| -> Result<Option<Value>> {
                let meta = read_json(&meta_path)?;
                if get(&meta, "ended_at")?.is_some_and(|v| !v.is_null()) {
                    return Ok(None);
                }
                // Live owns the capture state; a joined room alone does not mean its
                // devices started.
                if get(get_or_empty(&meta, "mode", &empty)?, "ktalk_only")?.is_some_and(Value::truthy)
                    && !get(&meta, "recording_active")?.is_some_and(Value::truthy)
                {
                    return Ok(None);
                }
                let created_at = item(&meta, "created_at")?.clone();
                let conference = match get(&meta, "conference")? {
                    Some(c) => c,
                    None => get_or_empty(&meta, "ktalk", &empty)?,
                };
                let stats = get_or_empty(&meta, "stats", &empty)?;
                let names = items(get_or_empty(&meta, "participants", &empty)?, "values")?
                    .values()
                    .map(|p| item(p, "name").cloned())
                    .collect::<Result<Vec<_>>>()?;
                let mut entry = Map::new();
                entry.insert("id", item(&meta, "id")?.clone());
                entry.insert("created_at", created_at.clone());
                entry.insert("ended_at", Value::Null);
                entry.insert("duration_seconds", Value::from(self.calc_duration(&created_at)));
                entry.insert("title", get(&meta, "title")?.cloned().unwrap_or(Value::Null));
                entry.insert("source", self.shared.canonical(get(&meta, "source")?.unwrap_or(&Value::Null)));
                entry.insert("end_reason", Value::Null);
                entry.insert("ktalk_live_url", get(conference, "live_url")?.cloned().unwrap_or(Value::Null));
                entry.insert("segment_count", get(stats, "segment_count")?.cloned().unwrap_or(Value::Int(0)));
                entry.insert("participants", Value::Array(names));
                entry.insert("is_active", Value::Bool(true));
                Ok(Some(Value::Object(entry)))
            })();
            match attempt {
                Ok(Some(entry)) => active.push(entry),
                Ok(None) | Err(StorageError::Json(_) | StorageError::Key(_)) => {}
                Err(e) => return Err(e),
            }
        }
        sort_desc(&mut active, |m| item(m, "created_at").cloned())?;
        Ok(active)
    }

    fn calc_duration(&self, created_at: &Value) -> i64 {
        parse_iso(created_at).and_then(|created| Ok(self.shared.now().minus(&created)?)).map_or(0, |d| d / 1_000_000)
    }

    /// `get_meeting(meeting_id)`: `{"meta", "segments"}` from the base path or an
    /// external one.
    ///
    /// # Errors
    /// As Python raises.
    pub fn get_meeting(&self, meeting_id: &str) -> Result<Option<Value>> {
        let Some(path) = self.find_meeting_path(meeting_id) else { return Ok(None) };
        let meta = self.read_meeting_meta(&path)?;
        let segments = read_rows(&path.join("transcript.jsonl"))?;
        let mut map = Map::new();
        map.insert("meta", meta);
        map.insert("segments", Value::Array(segments));
        Ok(Some(Value::Object(map)))
    }

    /// `find_meeting_path(meeting_id)`: the base path first, then the external ones.
    #[must_use]
    pub fn find_meeting_path(&self, meeting_id: &str) -> Option<PathBuf> {
        std::iter::once(self.shared.base_path.as_path())
            .chain(self.shared.options.external_paths.iter().map(|(p, _)| p.as_path()))
            .map(|root| root.join(meeting_id))
            .find(|p| p.exists())
    }

    /// `_read_meeting_meta`: WAVs resolved beside the meeting, the stored file
    /// untouched.
    fn read_meeting_meta(&self, meeting_path: &Path) -> Result<Value> {
        let mut meta = read_json(&meeting_path.join("meta.json"))?;
        if pyops::contains(&meta, "source")? {
            let source = self.shared.canonical(item(&meta, "source")?);
            set(&mut meta, "source", source)?;
        }
        for track in ["mic", "system"] {
            let audio = meeting_path.join(format!("audio_{track}.wav"));
            if audio.is_file() {
                set(&mut meta, &format!("audio_{track}_path"), Value::from(audio.to_string_lossy().into_owned()))?;
            }
        }
        Ok(meta)
    }

    /// `set_recording_active(active)`.
    ///
    /// # Errors
    /// As Python raises.
    pub fn set_recording_active(&self, active: bool) -> Result<()> {
        let inner = self.shared.lock();
        if inner.current_meeting_path.is_none() {
            return Ok(());
        }
        let mut meta = Shared::load_meta(&inner)?;
        // A same-day meeting may have been created with another policy.
        set(&mut meta, "mode", Value::Object(inner.mode.clone()))?;
        set(&mut meta, "recording_active", Value::Bool(active))?;
        Shared::save_meta(&inner, &meta)
    }

    /// `set_audio_paths(mic_path, system_path, meeting_path=)`.
    ///
    /// # Errors
    /// As Python raises.
    pub fn set_audio_paths(&self, mic: Option<&str>, system: Option<&str>, meeting_path: Option<&Path>) -> Result<()> {
        let inner = self.shared.lock();
        let Some(path) = meeting_path.map(Path::to_path_buf).or_else(|| inner.current_meeting_path.clone()) else {
            return Ok(());
        };
        let meta_path = path.join("meta.json");
        if !meta_path.exists() {
            return Ok(());
        }
        let mut meta = read_json(&meta_path)?;
        if let Some(mic) = mic.filter(|m| !m.is_empty()) {
            set(&mut meta, "audio_mic_path", Value::from(mic))?;
        }
        if let Some(system) = system.filter(|s| !s.is_empty()) {
            set(&mut meta, "audio_system_path", Value::from(system))?;
        }
        set(&mut meta, "updated_at", self.shared.now_iso())?;
        save_json(&meta_path, &meta)
    }

    /// `get_audio_paths(meeting_id)`: the recorded WAVs that still exist.
    ///
    /// # Errors
    /// As Python raises.
    pub fn get_audio_paths(&self, meeting_id: &str) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
        let meeting_path = self.shared.base_path.join(meeting_id);
        if !meeting_path.join("meta.json").exists() {
            return Ok((None, None));
        }
        let meta = self.read_meeting_meta(&meeting_path)?;
        let existing = |key: &str| -> Result<Option<PathBuf>> {
            Ok(match get(&meta, key)? {
                Some(Value::Str(p)) if !p.is_empty() && Path::new(p).exists() => Some(PathBuf::from(p)),
                _ => None,
            })
        };
        Ok((existing("audio_mic_path")?, existing("audio_system_path")?))
    }

    /// `create_transcript_backup(meeting_id)`: an existing backup is kept.
    ///
    /// # Errors
    /// As Python raises.
    pub fn create_transcript_backup(&self, meeting_id: &str) -> Result<Option<PathBuf>> {
        let meeting_path = self.shared.base_path.join(meeting_id);
        let transcript = meeting_path.join("transcript.jsonl");
        let backup = meeting_path.join("transcript.jsonl.backup");
        if !transcript.exists() {
            return Ok(None);
        }
        if backup.exists() {
            return Ok(Some(backup));
        }
        copy2(&transcript, &backup)?;
        let meta_path = meeting_path.join("meta.json");
        let mut meta = read_json(&meta_path)?;
        set(&mut meta, "transcript_backup_path", Value::from(backup.to_string_lossy().into_owned()))?;
        set(&mut meta, "updated_at", self.shared.now_iso())?;
        save_json(&meta_path, &meta)?;
        Ok(Some(backup))
    }

    /// `restore_transcript_backup(meeting_id)`.
    ///
    /// # Errors
    /// As Python raises.
    pub fn restore_transcript_backup(&self, meeting_id: &str) -> Result<bool> {
        let meeting_path = self.shared.base_path.join(meeting_id);
        let backup = meeting_path.join("transcript.jsonl.backup");
        if !backup.exists() {
            return Ok(false);
        }
        copy2(&backup, &meeting_path.join("transcript.jsonl"))?;
        fs::remove_file(&backup).map_err(|e| StorageError::os(&e, &backup))?;
        let meta_path = meeting_path.join("meta.json");
        let mut meta = read_json(&meta_path)?;
        let map = meta.as_object_mut().ok_or_else(|| pyops::attribute_error(&Value::Null, "pop"))?;
        for key in ["diarization_mic", "diarization_system", "transcript_backup_path"] {
            map.remove(key);
        }
        set(&mut meta, "updated_at", self.shared.now_iso())?;
        save_json(&meta_path, &meta)?;
        Ok(true)
    }

    /// `cleanup_old_audio(retention_hours)`: delete WAVs whose last write is older
    /// than the retention (an actively written file is always fresh), parsing
    /// `meta.json` only where audio was deleted. Returns how many files went.
    ///
    /// # Errors
    /// As Python raises.
    pub fn cleanup_old_audio(&self, retention_hours: f64) -> Result<usize> {
        #[allow(clippy::cast_precision_loss)]
        let now = self.shared.options.clock.now_micros() as f64 / 1e6;
        let cutoff = now - retention_hours * 3600.0;
        let mut deleted = 0;
        for meeting_dir in is_dir_entries(&self.shared.base_path)? {
            let mut stale = Vec::new();
            for track in ["mic", "system"] {
                let wav = meeting_dir.join(format!("audio_{track}.wav"));
                if let Ok(meta) = fs::metadata(&wav)
                    && mtime_seconds(&meta).is_some_and(|m| m < cutoff)
                {
                    stale.push((track, wav));
                }
            }
            if stale.is_empty() {
                continue;
            }
            let attempt = (|| -> Result<()> {
                for (_, wav) in &stale {
                    fs::remove_file(wav).map_err(|e| StorageError::os(&e, wav))?;
                    deleted += 1;
                }
                let meta_path = meeting_dir.join("meta.json");
                if meta_path.exists() {
                    let mut meta = read_json(&meta_path)?;
                    for (track, _) in &stale {
                        set(&mut meta, &format!("audio_{track}_path"), Value::Null)?;
                    }
                    set(&mut meta, "audio_deleted_at", self.shared.now_iso())?;
                    set(&mut meta, "updated_at", self.shared.now_iso())?;
                    save_json(&meta_path, &meta)?;
                }
                Ok(())
            })();
            match attempt {
                Ok(()) => {}
                Err(e @ (StorageError::Json(_) | StorageError::Os(_))) => {
                    self.shared.print(&format!("Warning: Cleanup error for {}: {e}", dir_name(&meeting_dir)));
                }
                Err(e) => return Err(e),
            }
        }
        if deleted > 0 {
            self.shared
                .print(&format!("Cleaned up {deleted} audio file(s) older than {}h", pyops::format_g(retention_hours)));
        }
        Ok(deleted)
    }

    /// `delete_meeting(meeting_id)`: the directory and its index entry; false for an
    /// unknown (or external) meeting.
    ///
    /// # Errors
    /// `ValueError` for a meeting still being recorded, and what Python raises.
    pub fn delete_meeting(&self, meeting_id: &str) -> Result<bool> {
        let meeting_path = self.shared.base_path.join(meeting_id);
        if !meeting_path.is_dir() {
            return Ok(false);
        }
        let meta_path = meeting_path.join("meta.json");
        let meta = if meta_path.exists() {
            let meta = read_json(&meta_path)?;
            if get(&meta, "ended_at")?.is_none_or(Value::is_null) {
                return Err(StorageError::Value("Cannot delete an active meeting".into()));
            }
            Some(meta)
        } else {
            None
        };
        if let Some(meta) = &meta {
            self.relink_neighbors(meta)?;
        }
        let index_path = self.shared.base_path.join("index.json");
        if index_path.exists() {
            let mut index = read_json(&index_path)?;
            let all = match get(&index, "meetings")? {
                Some(Value::Array(list)) => list.clone(),
                None => Vec::new(),
                Some(other) => {
                    return Err(StorageError::Type(format!("'{}' object is not iterable", other.type_name())));
                }
            };
            let mut kept = Vec::new();
            for m in &all {
                if get(m, "id")?.is_none_or(|id| *id != Value::from(meeting_id)) {
                    kept.push(m.clone());
                }
            }
            if kept.len() != all.len() {
                set(&mut index, "meetings", Value::Array(kept))?;
                save_json(&index_path, &index)?;
            }
        }
        let _ = fs::remove_dir_all(&meeting_path);
        Ok(true)
    }

    fn relink_neighbors(&self, meta: &Value) -> Result<()> {
        let prev = get(meta, "prev_meeting_id")?.cloned().unwrap_or(Value::Null);
        let next = get(meta, "next_meeting_id")?.cloned().unwrap_or(Value::Null);
        let own = get(meta, "id")?.cloned().unwrap_or(Value::Null);
        for (neighbor, field, target) in [(&prev, "next_meeting_id", &next), (&next, "prev_meeting_id", &prev)] {
            if !neighbor.truthy() {
                continue;
            }
            let path = self.shared.base_path.join(neighbor.py_str()).join("meta.json");
            if !path.exists() {
                continue;
            }
            let mut other = read_json(&path)?;
            if get(&other, field)?.is_some_and(|v| v.py_eq(&own)) || (get(&other, field)?.is_none() && own.is_null()) {
                set(&mut other, field, target.clone())?;
                save_json(&path, &other)?;
            }
        }
        Ok(())
    }

    /// `set_diarization_result(meeting_id, track, speakers_found)`.
    ///
    /// # Errors
    /// As Python raises.
    pub fn set_diarization_result(&self, meeting_id: &str, track: &str, speakers_found: i64) -> Result<()> {
        let meta_path = self.shared.base_path.join(meeting_id).join("meta.json");
        if !meta_path.exists() {
            return Ok(());
        }
        let mut meta = read_json(&meta_path)?;
        let mut result = Map::new();
        result.insert("performed_at", self.shared.now_iso());
        result.insert("speakers_found", Value::from(speakers_found));
        set(&mut meta, &format!("diarization_{track}"), Value::Object(result))?;
        set(&mut meta, "updated_at", self.shared.now_iso())?;
        save_json(&meta_path, &meta)
    }
}

impl Drop for MeetingStorage {
    fn drop(&mut self) {
        // Python's executor thread outlives nothing either: let queued titles finish.
        let titles = self.shared.lock().titles.take();
        if let Some(worker) = titles {
            drop(worker.jobs);
            let _ = worker.thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
