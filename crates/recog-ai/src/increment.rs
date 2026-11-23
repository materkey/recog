//! Versioned AI summaries of a meeting (`recog/increment.py`).
//!
//! `meetings/<id>/increments/<prompt-id>/vN.md` holds a YAML frontmatter and the answer,
//! `vN_meta.json` the provenance and status. Version N covers only the segments after
//! the last completed or in-progress version of the same participant; a version is
//! claimed with an exclusive create of its metadata (status `generating`, with its
//! segment range), so that two processes never produce the same version or the same
//! delta. A placeholder older than ten minutes counts as a crashed run and is marked
//! failed, which frees its segments. While a model runs, a heartbeat refreshes the
//! placeholder's time stamp.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use recog_core::meeting_storage::{self, UnnamedLocal};
use recog_core::pyjson::Value as CoreValue;

use crate::error::{AiError, Result};
use crate::install::{LegacyEnv, default_prompts_dir};
use crate::json::{self, Json, Map};
use crate::prompt_registry::{PromptConfig, PromptRegistry};
use crate::provider::{AiProvider, CliProviderFactory, ProviderFactory, complete_in_order};
use crate::pytext;
use crate::settings::{AiSettingsStore, Fallback, INCREMENT_TASK, ResolvedTask, render_prompt};
use crate::template::Variables;
use crate::timestamp;

pub const STALE_GENERATING_TIMEOUT_SECONDS: i64 = 600;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(120);

/// Called after a successful generation with `(meeting_id, version, prompt_id)`.
pub type Callback = Arc<dyn Fn(&str, i64, &str) + Send + Sync>;

/// `info.get(key, default)` of a name that Python then joins into text: a missing key
/// gives the default, anything but a string is an error.
fn field_or(info: &Json, key: &str, default: &str) -> Result<String> {
    match info.get(key) {
        None => Ok(default.to_owned()),
        Some(Json::Str(text)) => Ok(text.clone()),
        Some(other) => Err(AiError::Type(format!("expected str instance, {} found", other.type_name()))),
    }
}

/// `meta.get(key) or default` of text that Python uses as a string.
fn text_or(meta: &Json, key: &str, default: &str) -> Result<String> {
    match meta.get(key).filter(|value| value.truthy()) {
        None => Ok(default.to_owned()),
        Some(Json::Str(text)) => Ok(text.clone()),
        Some(other) => Err(AiError::Type(format!("'{}' object has no attribute 'replace'", other.type_name()))),
    }
}

/// Aborts a spawned task when dropped, so that a cancelled generation stops its
/// heartbeat as Python's `finally` does.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// `seg.get(key) or ""` formatted in an f-string.
fn field_or_empty(segment: &Json, key: &str) -> String {
    segment.get(key).filter(|value| value.truthy()).map(Json::py_str).unwrap_or_default()
}

fn object(value: &Json) -> Result<&Map> {
    value.as_object().ok_or_else(|| AiError::no_get(value.type_name()))
}

/// `_segments_to_transcript`: `[ts] Speaker: text` lines, events and empty texts left out.
pub fn segments_to_transcript(segments: &[Json]) -> Result<String> {
    let mut lines = Vec::new();
    for segment in segments {
        object(segment)?;
        if segment.get("event_type").is_some_and(Json::truthy) {
            continue;
        }
        let text = field_or_empty(segment, "text");
        if text.is_empty() {
            continue;
        }
        lines.push(format!(
            "[{}] {}: {text}",
            field_or_empty(segment, "ts_display"),
            field_or_empty(segment, "speaker")
        ));
    }
    Ok(lines.join("\n"))
}

/// Names of the local participants (`is_local` true), an unnamed one by its id
/// (`recog_core::meeting_storage::local_participant_names`, which reads core's JSON).
fn local_participant_names(meta: &Json) -> Result<Vec<String>> {
    let meta = recog_core::pyjson::loads(&meta.dumps(None)).map_err(|error| AiError::Value(error.0))?;
    meeting_storage::local_participant_names(&meta, UnnamedLocal::ParticipantId)
        .map_err(|error| AiError::Type(error.to_string()))?
        .into_iter()
        .map(|name| match name {
            CoreValue::Str(text) => Ok(text),
            other => Err(AiError::Type(format!("expected str instance, {} found", other.type_name()))),
        })
        .collect()
}

/// `meta.get("participants", {})` as an object.
fn participants(meta: &Json) -> Result<Option<&Map>> {
    match object(meta)?.get("participants") {
        None => Ok(None),
        Some(value) => object(value).map(Some),
    }
}

/// `v{N}_meta.json` → N.
fn version_from_filename(name: &str) -> Option<i64> {
    name.strip_prefix('v')?.strip_suffix("_meta.json")?.parse().ok()
}

/// `json.dumps(data, ensure_ascii=False, indent=2) + "\n"`.
fn save_json(path: &Path, data: &Json) -> std::io::Result<()> {
    std::fs::write(path, data.dumps(Some(2)) + "\n")
}

fn read_json(path: &Path) -> std::result::Result<Json, ReadError> {
    let text = std::fs::read(path).map_err(ReadError::Io)?;
    let text = String::from_utf8(text).map_err(|error| ReadError::Decode(error.to_string()))?;
    Json::parse(&pytext::universal_newlines(&text)).map_err(|_| ReadError::Json)
}

/// Why a metadata file could not be read: Python retries `JSONDecodeError` and
/// `OSError`, and lets a `UnicodeDecodeError` through.
enum ReadError {
    Io(std::io::Error),
    Json,
    Decode(String),
}

fn segment_id(segment: &Json) -> Result<i128> {
    match segment.get("id") {
        None => Ok(0),
        Some(value) => value.as_int().ok_or_else(|| AiError::Type(format!("segment id is a {}", value.type_name()))),
    }
}

fn matching_files(dir: &Path, accept: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_str().is_some_and(&accept))
        .map(|entry| entry.path())
        .collect()
}

/// `glob("v*_meta.json")`.
fn meta_files(dir: &Path) -> Vec<PathBuf> {
    matching_files(dir, |name| name.starts_with('v') && name.ends_with("_meta.json") && name.len() >= 11)
}

/// The class-level lock dict: one lock per (meeting, prompt), shared by every generator
/// of the process so that two of them never pick the same version.
type Locks = HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>;
static LOCKS: LazyLock<Mutex<Locks>> = LazyLock::new(Default::default);

fn lock_for(meeting_id: &str, prompt_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = LOCKS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry((meeting_id.to_owned(), prompt_id.to_owned())).or_default().clone()
}

/// A claimed version: `(version, delta segments, first id, last id)`.
struct Claim {
    version: i64,
    delta: Vec<Json>,
    start: i128,
    end: i128,
}

/// What the version files tell `_next_version`: the highest version, and the last
/// segment covered for the wanted participant.
struct SeenVersions {
    wanted: Json,
    max_version: i64,
    last_segment: Option<i128>,
}

impl SeenVersions {
    /// One metadata file; a stale placeholder is marked failed first.
    fn account_file(&mut self, path: &Path, now: i64) -> Result<()> {
        match read_recovering_stale(path, now) {
            Ok((meta, status)) => self.account(&meta, status),
            Err(ReadError::Decode(message)) => Err(AiError::Value(message)),
            Err(ReadError::Io(_) | ReadError::Json) => self.account_retry(path),
        }
    }

    /// The file may be being written by another claim (created, then filled): retry
    /// once, then count at least its version from the name so that the claim does not
    /// collide with it.
    fn account_retry(&mut self, path: &Path) -> Result<()> {
        std::thread::sleep(Duration::from_millis(50));
        match read_json(path) {
            Ok(meta) => self.account(&meta, None),
            Err(ReadError::Decode(message)) => Err(AiError::Value(message)),
            Err(_) => {
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                if let Some(version) = version_from_filename(name) {
                    self.max_version = self.max_version.max(version);
                }
                Ok(())
            }
        }
    }

    /// Versions are counted past failed ones. `status` replaces the file's when given.
    fn account(&mut self, meta: &Json, status: Option<String>) -> Result<()> {
        object(meta)?;
        self.max_version = self.max_version.max(meta_version(meta)? as i64);
        let status = status.or_else(|| meta.get("status").and_then(Json::as_str).map(str::to_owned));
        let participant = meta.get("participant_id").cloned().unwrap_or(Json::Null);
        // Completed results are final; in-progress ranges stop two processes from
        // summarizing the same delta. Failed versions free their segments.
        if participant == self.wanted
            && matches!(status.as_deref(), Some("completed" | "generating"))
            && let Some(end) = range_end(meta)
            && self.last_segment.is_none_or(|last| end > last)
        {
            self.last_segment = Some(end);
        }
        Ok(())
    }
}

/// A metadata file, with a stale `generating` placeholder marked failed on disk; the
/// status is `Some("failed")` when it was. A failed save is an OSError inside Python's
/// try: the retry path.
fn read_recovering_stale(path: &Path, now: i64) -> std::result::Result<(Json, Option<String>), ReadError> {
    let mut meta = read_json(path)?;
    if !is_stale_placeholder(&meta, now) {
        return Ok((meta, None));
    }
    let map = meta.as_object_mut().expect("object with a status");
    map.insert("status".into(), Json::from("failed"));
    map.insert("error".into(), Json::from("stale generating placeholder (process crash recovery)"));
    save_json(path, &meta).map_err(ReadError::Io)?;
    Ok((meta, Some("failed".to_owned())))
}

/// A `generating` placeholder whose time stamp is older than the stale timeout.
fn is_stale_placeholder(meta: &Json, now: i64) -> bool {
    if meta.get("status").and_then(Json::as_str) != Some("generating") {
        return false;
    }
    let started = meta.get("generated_at").and_then(Json::as_str).and_then(timestamp::parse_aware_isoformat);
    started.is_some_and(|started| (now - started) as f64 / 1e6 > STALE_GENERATING_TIMEOUT_SECONDS as f64)
}

/// `meta.get("version", 0)`, which must be an int.
fn meta_version(meta: &Json) -> Result<i128> {
    match meta.get("version") {
        None => Ok(0),
        Some(value) => value.as_int().ok_or_else(|| AiError::Type(format!("version is a {}", value.type_name()))),
    }
}

/// The last segment of a `[start, end]` range of integers.
fn range_end(meta: &Json) -> Option<i128> {
    match meta.get("segment_range") {
        Some(Json::Array(range)) if range.len() == 2 => range[1].as_int(),
        _ => None,
    }
}

/// The segments after `last`, every segment without it.
fn delta_after(segments: &[Json], last: Option<i128>) -> Result<Vec<Json>> {
    let Some(last) = last else {
        return Ok(segments.to_vec());
    };
    let mut delta = Vec::new();
    for segment in segments {
        if segment_id(segment)? > last {
            delta.push(segment.clone());
        }
    }
    Ok(delta)
}

/// The first and the last segment id of a nonempty delta.
fn id_span(delta: &[Json]) -> Result<(i128, i128)> {
    let ids = delta.iter().map(segment_id).collect::<Result<Vec<_>>>()?;
    Ok((*ids.iter().min().expect("nonempty"), *ids.iter().max().expect("nonempty")))
}

/// Writes `data` to a new file at `path`; `false` when the file exists already.
fn create_new_json(path: &Path, data: &Json) -> Result<bool> {
    match OpenOptions::new().write(true).create_new(true).mode(0o644).open(path) {
        Ok(mut file) => {
            file.write_all((data.dumps(Some(2)) + "\n").as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// `meta.json` of a meeting, an error when it cannot be read.
fn read_meeting_meta(path: &Path) -> Result<Json> {
    read_json(path).map_err(|error| match error {
        ReadError::Io(error) => AiError::Io(error),
        ReadError::Json => AiError::Value("Invalid JSON in meta.json".to_owned()),
        ReadError::Decode(message) => AiError::Value(message),
    })
}

/// The non-empty lines of `transcript.jsonl`; `None` without the file or a segment.
fn read_segments(meeting_path: &Path) -> Result<Option<Vec<Json>>> {
    let transcript_path = meeting_path.join("transcript.jsonl");
    if !transcript_path.exists() {
        return Ok(None);
    }
    let content = pytext::read_text(&transcript_path)?;
    let mut segments = Vec::new();
    for (index, line) in pytext::strip(&content).split('\n').enumerate() {
        if pytext::strip(line).is_empty() {
            continue;
        }
        let segment = Json::parse(line).map_err(|error| {
            AiError::Value(format!("Malformed JSONL in transcript.jsonl line {}: {error}", index + 1))
        })?;
        segments.push(segment);
    }
    Ok((!segments.is_empty()).then_some(segments))
}

/// The name an increment is generated for: the local participants (`User` without
/// one), or the given participant's name, its id when it has none.
fn user_name(meta: &Json, participant_id: Option<&str>) -> Result<String> {
    let Some(pid) = participant_id else {
        let names = local_participant_names(meta)?;
        return Ok(if names.is_empty() { "User".to_owned() } else { names.join(", ") });
    };
    let participant = participants(meta)?.and_then(|participants| participants.get(pid));
    let Some(participant) = participant else {
        return Err(AiError::Value(format!("Participant '{pid}' not found")));
    };
    object(participant)?;
    text_or(participant, "name", pid)
}

/// The names of every participant, by id when one has none.
fn participant_names(meta: &Json) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if let Some(map) = participants(meta)? {
        for (pid, info) in map {
            object(info)?;
            names.push(field_or(info, "name", pid)?);
        }
    }
    Ok(names)
}

/// The template of a saved prompt: an explicit override from the settings, else the
/// caller's. A caller can supply a generated PromptConfig with an existing registry ID:
/// only an explicit override replaces its text.
fn prompt_template<'a>(snapshot: &'a Map, task_id: &str, prompt: &'a PromptConfig) -> &'a str {
    let saved_prompt = |id: &str| {
        snapshot.get("tasks").and_then(|tasks| tasks.get(id)).and_then(|task| task.get("prompt")).and_then(Json::as_str)
    };
    let template = saved_prompt(task_id).unwrap_or(&prompt.template);
    if prompt.id == "my-increment" {
        return saved_prompt(INCREMENT_TASK).unwrap_or(template);
    }
    template
}

/// A rendered increment prompt with the settings it was resolved from.
struct Rendered {
    settings: ResolvedTask,
    prompt: String,
    participants: Vec<String>,
}

/// Which provider, model, effort and settings revision produce a version.
fn generation_meta(provider: &dyn AiProvider, settings: &ResolvedTask) -> Map {
    let mut generation = Map::new();
    generation.insert("provider".into(), Json::from(provider.key()));
    generation.insert("model".into(), Json::from(provider.model()));
    generation.insert("reasoning_effort".into(), provider.reasoning_effort().map_or(Json::Null, Json::from));
    generation.insert("settings_revision".into(), Json::from(settings.revision.as_str()));
    generation
}

/// The metadata of a completed version.
fn completed_meta(base: &Map, generation: Map, claim: &Claim, duration: Duration) -> Json {
    let mut completed = base.clone();
    completed.extend(generation);
    completed.insert("segment_range".into(), Json::Array(vec![Json::Int(claim.start), Json::Int(claim.end)]));
    completed.insert("segment_count".into(), Json::from(claim.delta.len() as i64));
    completed.insert("generated_at".into(), Json::from(timestamp::utc_now_isoformat()));
    completed.insert("status".into(), Json::from("completed"));
    completed.insert("duration_seconds".into(), Json::Float(round_tenths(duration)));
    completed.insert("error".into(), Json::Null);
    Json::Object(completed)
}

/// Writes the failed metadata if a generation is dropped (cancelled) before it ends, so
/// that no `generating` placeholder outlives it; Python does this for `CancelledError`.
struct CancelGuard {
    path: PathBuf,
    base: Map,
    generation: Map,
    started: Instant,
    armed: bool,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = save_json(&self.path, &self.failed_meta("cancelled: "));
        eprintln!("[increment] {} CANCELLED: ", self.path.display());
    }
}

impl CancelGuard {
    /// The failed metadata of the version: its provenance, the time and the error.
    fn failed_meta(&self, error: &str) -> Json {
        let mut meta = self.generation.clone();
        meta.extend(self.base.clone());
        meta.insert("generated_at".into(), Json::from(timestamp::utc_now_isoformat()));
        meta.insert("status".into(), Json::from("failed"));
        meta.insert("duration_seconds".into(), Json::Float(round_tenths(self.started.elapsed())));
        meta.insert("error".into(), Json::from(error));
        Json::Object(meta)
    }
}

/// `round(seconds, 1)`: correctly rounded, ties to even, like Python's.
fn round_tenths(duration: Duration) -> f64 {
    format!("{:.1}", duration.as_secs_f64()).parse().unwrap_or(0.0)
}

fn escape_yaml_str(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ")
}

/// Generates and stores versioned increment summaries.
pub struct IncrementGenerator {
    meetings_base: PathBuf,
    prompts_dir: PathBuf,
    factory: Arc<dyn ProviderFactory>,
    legacy_env: Option<LegacyEnv>,
    callback: Option<Callback>,
    heartbeat_interval: Duration,
}

impl IncrementGenerator {
    /// `IncrementGenerator(meetings_base, prompts_dir)`: the CLI providers with the
    /// process environment, the shipped prompts by default.
    pub fn new(meetings_base: impl Into<PathBuf>, prompts_dir: Option<PathBuf>) -> IncrementGenerator {
        IncrementGenerator {
            meetings_base: meetings_base.into(),
            prompts_dir: prompts_dir.unwrap_or_else(default_prompts_dir),
            factory: Arc::new(CliProviderFactory::default()),
            legacy_env: None,
            callback: None,
            heartbeat_interval: HEARTBEAT_INTERVAL,
        }
    }

    pub fn with_factory(mut self, factory: Arc<dyn ProviderFactory>) -> IncrementGenerator {
        self.factory = factory;
        self
    }

    /// Fixed legacy model variables for the settings instead of the process's.
    pub fn with_legacy_env(mut self, env: LegacyEnv) -> IncrementGenerator {
        self.legacy_env = Some(env);
        self
    }

    pub fn with_heartbeat_interval(mut self, interval: Duration) -> IncrementGenerator {
        self.heartbeat_interval = interval;
        self
    }

    /// `set_callback`.
    pub fn set_callback(&mut self, callback: Callback) {
        self.callback = Some(callback);
    }

    fn increments_dir(&self, meeting_id: &str, prompt_id: &str) -> PathBuf {
        self.meetings_base.join(meeting_id).join("increments").join(prompt_id)
    }

    fn store(&self) -> AiSettingsStore {
        let parent = self.meetings_base.parent().unwrap_or(Path::new("")).to_path_buf();
        let store = AiSettingsStore::new(parent, Some(self.prompts_dir.clone()));
        match &self.legacy_env {
            Some(env) => store.with_legacy_env(env.clone()),
            None => store,
        }
    }

    /// `_next_version`: the next version number and the last segment covered for this
    /// participant. Versions are counted past failed ones; a stale placeholder is marked
    /// failed first.
    fn next_version(
        &self,
        meeting_id: &str,
        prompt_id: &str,
        participant_id: Option<&str>,
    ) -> Result<(i64, Option<i128>)> {
        let dir = self.increments_dir(meeting_id, prompt_id);
        if !dir.exists() {
            return Ok((1, None));
        }
        let mut seen =
            SeenVersions { wanted: participant_id.map_or(Json::Null, Json::from), max_version: 0, last_segment: None };
        let now = timestamp::now_micros();
        for path in meta_files(&dir) {
            seen.account_file(&path, now)?;
        }
        Ok((seen.max_version + 1, seen.last_segment))
    }

    /// `_claim_version`: creates the `generating` placeholder of the next version with
    /// its segment range, exclusively. `force` ignores the delta (manual regenerate).
    fn claim_version(
        &self,
        meeting_id: &str,
        prompt_id: &str,
        segments: &[Json],
        participant_id: Option<&str>,
        participant_name: &str,
        force: bool,
    ) -> Result<Option<Claim>> {
        let dir = self.increments_dir(meeting_id, prompt_id);
        std::fs::create_dir_all(&dir)?;
        for _ in 0..5 {
            let (version, last_segment) = self.next_version(meeting_id, prompt_id, participant_id)?;
            let delta = delta_after(segments, last_segment.filter(|_| !force))?;
            if delta.is_empty() {
                return Ok(None);
            }
            let (start, end) = id_span(&delta)?;
            let placeholder = json::object([
                ("version", Json::from(version)),
                ("prompt_id", Json::from(prompt_id)),
                ("participant_id", participant_id.map_or(Json::Null, Json::from)),
                ("participant_name", Json::from(participant_name)),
                ("status", Json::from("generating")),
                ("generated_at", Json::from(timestamp::utc_now_isoformat())),
                ("segment_range", Json::Array(vec![Json::Int(start), Json::Int(end)])),
            ]);
            if create_new_json(&dir.join(format!("v{version}_meta.json")), &placeholder)? {
                return Ok(Some(Claim { version, delta, start, end }));
            }
        }
        Err(AiError::Runtime(format!("Could not claim version for {prompt_id} after retries")))
    }

    /// Removes a `generating` placeholder of a skipped version.
    fn remove_generating_meta(dir: &Path, version: i64) {
        let path = dir.join(format!("v{version}_meta.json"));
        if let Ok(meta) = read_json(&path)
            && meta.get("status").and_then(Json::as_str) == Some("generating")
        {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Moves the old flat `increments/v*.md|_meta.json` into `increments/<default>/`.
    /// Idempotent; a file whose target exists stays where it is.
    pub fn migrate_flat_increments(&self, meeting_path: &Path, default_prompt_id: &str) -> Result<()> {
        let dir = meeting_path.join("increments");
        if !dir.exists() {
            return Ok(());
        }
        let mut flat = matching_files(&dir, |name| name.starts_with('v') && name.ends_with(".md") && name.len() >= 4);
        flat.extend(meta_files(&dir));
        if flat.is_empty() {
            return Ok(());
        }
        let target = dir.join(default_prompt_id);
        std::fs::create_dir_all(&target)?;
        for file in flat {
            let destination = target.join(file.file_name().expect("listed file"));
            if !destination.exists() {
                std::fs::rename(&file, &destination)?;
            }
        }
        Ok(())
    }

    fn frontmatter(
        meta: &Json,
        meeting_id: &str,
        version: i64,
        start: i128,
        end: i128,
        participants: &[String],
    ) -> Result<String> {
        let title = text_or(meta, "title", "Untitled meeting")?;
        let created = match meta.get("created_at") {
            None => String::new(),
            Some(Json::Str(text)) => text.clone(),
            Some(other) => return Err(AiError::Type(format!("'{}' object is not subscriptable", other.type_name()))),
        };
        let date = pytext::prefix_chars(&created, 10);
        let quoted: Vec<String> = participants.iter().map(|name| format!("\"{}\"", escape_yaml_str(name))).collect();
        Ok(format!(
            "---\nmeeting: \"{}\"\nmeeting_id: {meeting_id}\ndate: \"{date}\"\nparticipants: [{}]\nversion: {version}\nsegment_range: [{start}, {end}]\n---\n\n",
            escape_yaml_str(&title),
            quoted.join(", ")
        ))
    }

    /// `generate`: the next version of a meeting's increment for this prompt.
    ///
    /// `Ok(None)` when skipped (no meta or transcript, no new segment, no text). A
    /// transcript or meta that cannot be read is an error. Once a version is claimed, a
    /// failure is recorded as failed metadata and gives `Ok(None)`. `force` summarizes
    /// the whole transcript (manual regenerate of a finished meeting).
    pub async fn generate(
        &self,
        meeting_id: &str,
        meeting_path: &Path,
        prompt: &PromptConfig,
        force: bool,
        participant_id: Option<&str>,
    ) -> Result<Option<i64>> {
        let prompt_id = prompt.id.as_str();
        let meta_path = meeting_path.join("meta.json");
        if !meta_path.exists() {
            return Ok(None);
        }
        let Some(segments) = read_segments(meeting_path)? else {
            return Ok(None);
        };

        let lock = lock_for(meeting_id, prompt_id);
        let _guard = lock.lock().await;
        let meta = read_meeting_meta(&meta_path)?;
        let user_name = user_name(&meta, participant_id)?;

        let Some(claim) = self.claim_version(meeting_id, prompt_id, &segments, participant_id, &user_name, force)?
        else {
            eprintln!("[increment] {prompt_id} skipped: 0 delta segments");
            return Ok(None);
        };
        self.generate_claimed(meeting_id, prompt, &meta, &user_name, participant_id, claim).await
    }

    /// The part of `generate` after the claim: a failure is recorded as failed metadata
    /// and gives `Ok(None)`; the callback hears of a success.
    async fn generate_claimed(
        &self,
        meeting_id: &str,
        prompt: &PromptConfig,
        meta: &Json,
        user_name: &str,
        participant_id: Option<&str>,
        claim: Claim,
    ) -> Result<Option<i64>> {
        let prompt_id = prompt.id.as_str();
        let version = claim.version;
        let dir = self.increments_dir(meeting_id, prompt_id);
        let meta_file = dir.join(format!("v{version}_meta.json"));
        let base = json::object([
            ("version", Json::from(version)),
            ("prompt_id", Json::from(prompt_id)),
            ("prompt_name", Json::from(prompt.name.as_str())),
            ("participant_id", participant_id.map_or(Json::Null, Json::from)),
            ("participant_name", Json::from(user_name)),
        ]);
        let Json::Object(base) = base else { unreachable!() };
        let mut cancel =
            CancelGuard { path: meta_file.clone(), base, generation: Map::new(), started: Instant::now(), armed: true };

        let outcome = self.run(meeting_id, prompt, meta, user_name, &claim, &mut cancel).await;
        cancel.armed = false;
        match outcome {
            Ok(Some(())) => {
                if let Some(callback) = &self.callback {
                    callback(meeting_id, version, prompt_id);
                }
                Ok(Some(version))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                std::fs::create_dir_all(&dir)?;
                save_json(&meta_file, &cancel.failed_meta(&error.to_string()))?;
                eprintln!("[increment] {prompt_id} v{version} FAILED: {error}");
                Ok(None)
            }
        }
    }

    /// The part of `generate` after the claim whose failure is recorded as failed
    /// metadata. `Ok(None)`: skipped for lack of text.
    async fn run(
        &self,
        meeting_id: &str,
        prompt: &PromptConfig,
        meta: &Json,
        user_name: &str,
        claim: &Claim,
        cancel: &mut CancelGuard,
    ) -> Result<Option<()>> {
        let prompt_id = prompt.id.as_str();
        let version = claim.version;
        let dir = self.increments_dir(meeting_id, prompt_id);
        let transcript = segments_to_transcript(&claim.delta)?;
        if pytext::strip(&transcript).is_empty() {
            eprintln!("[increment] {prompt_id} v{version} skipped: no text in delta segments");
            Self::remove_generating_meta(&dir, version);
            return Ok(None);
        }
        let rendered = self.render(prompt, meta, user_name, transcript)?;
        std::fs::create_dir_all(&dir)?;

        let settings = &rendered.settings;
        let meta_file = dir.join(format!("v{version}_meta.json"));
        let heartbeat = AbortOnDrop(tokio::spawn(heartbeat(meta_file.clone(), self.heartbeat_interval)));
        // A failed or cancelled version names the provider that was being asked.
        let mut attempt = |provider: &dyn AiProvider| cancel.generation = generation_meta(provider, settings);
        let answered = complete_in_order(&*self.factory, settings, "increment", &rendered.prompt, &mut attempt).await;
        drop(heartbeat);
        let (provider, result) = answered?;
        let generation = generation_meta(&*provider, settings);
        let duration = cancel.started.elapsed();

        Self::write_markdown(&dir, meta, meeting_id, claim, &rendered.participants, &result)?;
        save_json(&meta_file, &completed_meta(&cancel.base, generation, claim, duration))?;
        eprintln!(
            "[increment] {prompt_id} v{version} generated ({} segments, {:.1}s)",
            claim.delta.len(),
            duration.as_secs_f64()
        );
        Ok(Some(()))
    }

    /// `vN.md`: the frontmatter and the answer.
    fn write_markdown(
        dir: &Path,
        meta: &Json,
        meeting_id: &str,
        claim: &Claim,
        participants: &[String],
        answer: &str,
    ) -> Result<()> {
        let frontmatter = Self::frontmatter(meta, meeting_id, claim.version, claim.start, claim.end, participants)?;
        std::fs::write(dir.join(format!("v{}.md", claim.version)), frontmatter + answer)?;
        Ok(())
    }

    /// The prompt of an increment: the scenario's settings from one snapshot, the saved
    /// template and the meeting's variables.
    fn render(&self, prompt: &PromptConfig, meta: &Json, user_name: &str, transcript: String) -> Result<Rendered> {
        let participants = participant_names(meta)?;
        let store = self.store();
        let snapshot = store.read()?;
        let task_id = format!("saved:{}", prompt.id);
        let settings = store.resolve_task(
            &task_id,
            Some(&snapshot),
            Fallback { prompt: Some(&prompt.template), label: Some(&prompt.name) },
        )?;
        let template = prompt_template(&snapshot, &task_id, prompt);
        let title = text_or(meta, "title", "")?;
        let variables: Variables = [
            ("user_name".to_owned(), user_name.to_owned()),
            ("transcript".to_owned(), transcript),
            ("participants".to_owned(), participants.join(", ")),
            ("meeting_title".to_owned(), title),
        ]
        .into_iter()
        .collect();
        let prompt = render_prompt(template, &variables)?;
        Ok(Rendered { settings, prompt, participants })
    }

    /// `generate_all`: every prompt matching the meeting title, concurrently. A failing
    /// prompt is reported and does not stop the others.
    pub async fn generate_all(&self, meeting_id: &str, meeting_path: &Path) -> Result<()> {
        self.migrate_flat_increments(meeting_path, "my-increment")?;
        let registry = PromptRegistry::new(&self.prompts_dir);
        let meta_path = meeting_path.join("meta.json");
        if !meta_path.exists() {
            return Ok(());
        }
        let meta = read_meeting_meta(&meta_path)?;
        let title = object(&meta)?.get("title").filter(|title| title.truthy()).map(Json::py_str).unwrap_or_default();
        let prompts = registry.matching(&title);
        if prompts.is_empty() {
            eprintln!("[increment] no matching prompts found");
            return Ok(());
        }
        let runs = prompts.iter().map(|prompt| self.generate(meeting_id, meeting_path, prompt, false, None));
        let results = futures_util::future::join_all(runs).await;
        for (prompt, result) in prompts.iter().zip(results) {
            if let Err(error) = result {
                eprintln!("[increment] {} unexpected error: {error}", prompt.id);
            }
        }
        Ok(())
    }

    /// `{prompt_id: [metas]}` of every prompt with increments, by prompt ID.
    pub fn list_all_increments(&self, meeting_id: &str) -> Result<Vec<(String, Vec<Json>)>> {
        let meeting_path = self.meetings_base.join(meeting_id);
        self.migrate_flat_increments(&meeting_path, "my-increment")?;
        let base = meeting_path.join("increments");
        if !base.exists() {
            return Ok(Vec::new());
        }
        let mut dirs: Vec<PathBuf> =
            std::fs::read_dir(&base)?.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
        dirs.sort();
        let mut result = Vec::new();
        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            let prompt_id = dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            let metas = self.list_increments(meeting_id, &prompt_id)?;
            if !metas.is_empty() {
                result.push((prompt_id, metas));
            }
        }
        Ok(result)
    }

    /// The readable metadata of a prompt's versions, by version.
    pub fn list_increments(&self, meeting_id: &str, prompt_id: &str) -> Result<Vec<Json>> {
        let dir = self.increments_dir(meeting_id, prompt_id);
        let mut metas = Vec::new();
        for path in meta_files(&dir) {
            match read_json(&path) {
                Ok(meta) => metas.push(meta),
                Err(ReadError::Decode(message)) => return Err(AiError::Value(message)),
                Err(ReadError::Io(_) | ReadError::Json) => {}
            }
        }
        for meta in &metas {
            object(meta)?;
            meta_version(meta)?;
        }
        metas.sort_by_key(|meta| meta_version(meta).unwrap_or(0));
        Ok(metas)
    }

    /// `{meta, content}` of one version; `content` is `None` without its `.md`.
    pub fn get_increment(
        &self,
        meeting_id: &str,
        prompt_id: &str,
        version: i64,
    ) -> Result<Option<(Json, Option<String>)>> {
        let dir = self.increments_dir(meeting_id, prompt_id);
        let meta_path = dir.join(format!("v{version}_meta.json"));
        if !meta_path.exists() {
            return Ok(None);
        }
        let meta = match read_json(&meta_path) {
            Ok(meta) => meta,
            Err(ReadError::Decode(message)) => return Err(AiError::Value(message)),
            Err(ReadError::Io(_) | ReadError::Json) => return Ok(None),
        };
        let md_path = dir.join(format!("v{version}.md"));
        let content = if md_path.exists() { Some(pytext::read_text(&md_path)?) } else { None };
        Ok(Some((meta, content)))
    }
}

/// `_heartbeat`: refreshes `generated_at` of a `generating` placeholder.
async fn heartbeat(path: PathBuf, interval: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        if let Ok(mut meta) = read_json(&path)
            && meta.get("status").and_then(Json::as_str) == Some("generating")
            && let Some(map) = meta.as_object_mut()
        {
            map.insert("generated_at".into(), Json::from(timestamp::utc_now_isoformat()));
            let _ = save_json(&path, &meta);
        }
    }
}
