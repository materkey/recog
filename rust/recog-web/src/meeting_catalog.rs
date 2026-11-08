//! The meeting list as a disposable read model — the port of
//! `recog/web/meeting_catalog.py`.
//!
//! Only the refresh worker touches files; requests read the last published snapshot.
//! File events are hints that mark `index.json` or a `meta.json` dirty; a full stat
//! reconciliation every 30 s (and a retry within a second after an unreadable file or
//! directory) catches what the events missed. A file is reread only when its inode,
//! size, mtime or ctime changed, and a file that changed while it was read, or holds
//! invalid metadata, keeps its last valid entry until it reads cleanly.
//!
//! Known difference: rows with the same `created_at` keep the order of their paths;
//! Python's depends on the iteration order of a set of paths, which changes from run to
//! run.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use recog_core::install_config::InstallConfig;
use recog_core::pyjson::{self, Map, Value};
use recog_core::time::{self as pytime, SystemClock};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use crate::util::Event;

type Signature = (u64, u64, i64, i64);

#[derive(Clone, Debug)]
enum Entry {
    Index(Vec<Value>),
    Meta(Map),
}

#[derive(Default)]
struct Files {
    /// Path order instead of Python's dict insertion order (see the module notes).
    entries: BTreeMap<PathBuf, (Signature, Entry)>,
    retry_paths: BTreeSet<PathBuf>,
    rescan_needed: bool,
}

/// Timings of the refresh loop.
#[derive(Clone, Copy, Debug)]
pub struct Timings {
    pub reconcile: Duration,
    pub debounce: Duration,
    pub retry: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self { reconcile: Duration::from_secs(30), debounce: Duration::from_millis(50), retry: Duration::from_secs(1) }
    }
}

struct Inner {
    base_path: PathBuf,
    external_paths: Vec<(PathBuf, Option<String>)>,
    roots: Vec<PathBuf>,
    config: InstallConfig,
    timings: Timings,
    files: Mutex<Files>,
    meetings: RwLock<Arc<Vec<Map>>>,
    dirty: Mutex<BTreeSet<PathBuf>>,
    wake: Event,
    lock: AsyncMutex<()>,
    needs_retry: AtomicBool,
    stopping: AtomicBool,
    started: AtomicBool,
    /// Files read and parsed, for tests: an unchanged file is never reopened.
    parsed: std::sync::atomic::AtomicUsize,
}

/// `MeetingCatalog(base_path, external_paths)`.
#[derive(Clone)]
pub struct MeetingCatalog {
    inner: Arc<Inner>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

/// The kind of a file system event, as watchdog names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Created,
    Modified,
    Deleted,
    Moved,
}

/// Why a file cannot be used: Python's `(OSError, ValueError, KeyError, TypeError,
/// AttributeError)` with its text.
struct Unusable(String);

fn no_get(value: &Value) -> Unusable {
    Unusable(format!("'{}' object has no attribute 'get'", value.type_name()))
}

/// `value.get(key, default)` of a dict; anything else is an `AttributeError`.
fn get_or<'a>(value: &'a Value, key: &str, default: &'a Value) -> Result<&'a Value, Unusable> {
    match value {
        Value::Object(map) => Ok(map.get(key).unwrap_or(default)),
        other => Err(no_get(other)),
    }
}

fn signature(path: &Path) -> std::io::Result<Signature> {
    let m = std::fs::metadata(path)?;
    Ok((m.ino(), m.size(), m.mtime() * 1_000_000_000 + m.mtime_nsec(), m.ctime() * 1_000_000_000 + m.ctime_nsec()))
}

fn validate_entry(row: &Value) -> Result<(), Unusable> {
    let Value::Object(map) = row else { return Err(Unusable("entry requires an id".into())) };
    if !matches!(map.get("id"), Some(Value::Str(_))) {
        return Err(Unusable("entry requires an id".into()));
    }
    for field in ["created_at", "title"] {
        if !matches!(map.get(field), None | Some(Value::Null | Value::Str(_))) {
            return Err(Unusable(format!("invalid {field}")));
        }
    }
    if let Some(participants) = map.get("participants")
        && !matches!(participants, Value::Array(items) if items.iter().all(|p| matches!(p, Value::Str(_))))
    {
        return Err(Unusable("invalid participants".into()));
    }
    if map.get("is_active").is_some_and(Value::truthy) && !matches!(map.get("created_at"), Some(Value::Str(_))) {
        return Err(Unusable("active entry requires created_at".into()));
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String, Unusable> {
    let raw = std::fs::read(path).map_err(|e| Unusable(e.to_string()))?;
    let text = std::str::from_utf8(&raw).map_err(|e| Unusable(pyjson::utf8_error_message(&raw, &e)))?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

impl Inner {
    fn index_path(&self) -> PathBuf {
        self.base_path.join("index.json")
    }

    fn canonical(&self, source: &Value) -> Result<Value, Unusable> {
        match source {
            Value::Str(s) => Ok(Value::from(self.config.canonical_source(Some(s)).unwrap_or(s))),
            Value::Array(_) | Value::Object(_) => Err(Unusable(format!(
                "cannot use '{}' as a dict key (unhashable type: '{}')",
                source.type_name(),
                source.type_name()
            ))),
            other => Ok(other.clone()),
        }
    }

    fn metadata_entry(&self, path: &Path, meta: &Value) -> Result<Map, Unusable> {
        let empty = Value::Object(Map::new());
        let null = Value::Null;
        let zero = Value::Int(0);
        let map = meta.as_object().ok_or_else(|| no_get(meta))?;
        let (Some(Value::Str(id)), Some(Value::Str(created_at))) = (map.get("id"), map.get("created_at")) else {
            return Err(Unusable("metadata requires id and created_at".into()));
        };
        let root = path.parent().and_then(Path::parent).unwrap_or(Path::new(""));
        let external = root != self.base_path;
        let mut source = map.get("source").cloned().unwrap_or(Value::Null);
        if external {
            let Some((_, override_source)) = self.external_paths.iter().find(|(p, _)| p == root) else {
                return Err(Unusable("StopIteration".into()));
            };
            source = match override_source.as_deref().filter(|s| !s.is_empty()) {
                Some(s) => Value::from(s),
                None if source.truthy() => source,
                None => Value::from("unknown"),
            };
        }
        let active = !external && map.get("ended_at").is_none_or(Value::is_null);
        let stats = get_or(meta, "stats", &empty)?;
        let ktalk = get_or(meta, "ktalk", &empty)?;
        let conference = get_or(meta, "conference", ktalk)?;
        let participants = get_or(meta, "participants", &empty)?;
        let Value::Object(participants) = participants else {
            return Err(Unusable(format!("'{}' object has no attribute 'items'", participants.type_name())));
        };
        let names = participants
            .iter()
            .map(|(key, p)| Ok(get_or(p, "name", &Value::from(key.as_str()))?.clone()))
            .collect::<Result<Vec<Value>, Unusable>>()?;
        let mut entry = Map::new();
        entry.insert("id", Value::from(id.as_str()));
        entry.insert("created_at", Value::from(created_at.as_str()));
        entry.insert("ended_at", map.get("ended_at").cloned().unwrap_or(Value::Null));
        entry.insert("duration_seconds", get_or(stats, "duration_seconds", &zero)?.clone());
        entry.insert("title", map.get("title").cloned().unwrap_or(Value::Null));
        entry.insert("source", self.canonical(&source)?);
        entry.insert(
            "end_reason",
            if active { Value::Null } else { map.get("end_reason").cloned().unwrap_or(Value::Null) },
        );
        entry.insert(
            if external { "conference_url" } else { "ktalk_live_url" },
            get_or(conference, "live_url", &null)?.clone(),
        );
        entry.insert("segment_count", get_or(stats, "segment_count", &zero)?.clone());
        entry.insert("participants", Value::Array(names));
        entry.insert("recorder", get_or(get_or(meta, "callrec", &empty)?, "recorder", &null)?.clone());
        if active {
            entry.insert("is_active", Value::Bool(true));
        }
        if external {
            entry.insert("_external", Value::Bool(true));
            entry.insert("_external_path", Value::from(root.to_string_lossy().into_owned()));
        }
        Ok(entry)
    }

    fn parse_file(&self, path: &Path) -> Result<Entry, Unusable> {
        self.parsed.fetch_add(1, Ordering::SeqCst);
        let data = pyjson::loads(&read_text(path)?).map_err(|e| Unusable(e.0))?;
        if data.as_object().is_none() {
            return Err(Unusable("expected a JSON object".into()));
        }
        if path == self.index_path() {
            let empty = Value::Array(Vec::new());
            let Value::Array(rows) = get_or(&data, "meetings", &empty)? else {
                return Err(Unusable("invalid meetings index".into()));
            };
            for row in rows {
                validate_entry(row)?;
            }
            Ok(Entry::Index(rows.clone()))
        } else {
            let entry = self.metadata_entry(path, &data)?;
            validate_entry(&Value::Object(entry.clone()))?;
            Ok(Entry::Meta(entry))
        }
    }

    fn read_file(&self, files: &mut Files, path: &Path) {
        let sig = match signature(path) {
            Ok(sig) => sig,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                files.entries.remove(path);
                files.retry_paths.remove(path);
                return;
            }
            Err(e) => return self.unusable(files, path, &Unusable(e.to_string())),
        };
        if files.entries.get(path).is_some_and(|(cached, _)| *cached == sig) {
            files.retry_paths.remove(path);
            return;
        }
        let parsed = self.parse_file(path).and_then(|entry| match signature(path) {
            Ok(after) if after == sig => Ok(entry),
            Ok(_) => Err(Unusable("file changed during read".into())),
            Err(e) => Err(Unusable(e.to_string())),
        });
        match parsed {
            Ok(entry) => {
                files.entries.insert(path.to_path_buf(), (sig, entry));
                files.retry_paths.remove(path);
            }
            Err(_) if !path.exists() => {
                files.entries.remove(path);
                files.retry_paths.remove(path);
            }
            Err(error) => self.unusable(files, path, &error),
        }
    }

    fn unusable(&self, files: &mut Files, path: &Path, error: &Unusable) {
        if !files.retry_paths.contains(path) {
            eprintln!("Cannot refresh {}; retaining last valid metadata: {}", path.display(), error.0);
        }
        files.retry_paths.insert(path.to_path_buf());
    }

    /// `_refresh_files(paths)` and the publication of its result.
    fn refresh_files(&self, paths: Option<BTreeSet<PathBuf>>) {
        let mut files = self.files.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut scan_failed = false;
        let paths = match paths {
            Some(paths) if !files.rescan_needed => paths,
            _ => {
                let mut paths = BTreeSet::from([self.index_path()]);
                for root in &self.roots {
                    match std::fs::read_dir(root) {
                        Ok(children) => {
                            for child in children.flatten() {
                                if child.path().is_dir() {
                                    paths.insert(child.path().join("meta.json"));
                                }
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            eprintln!("Cannot scan {}; retaining catalog entries: {e}", root.display());
                            let kept: Vec<PathBuf> = files
                                .entries
                                .keys()
                                .filter(|p| p.parent().and_then(Path::parent) == Some(root.as_path()))
                                .cloned()
                                .collect();
                            paths.extend(kept);
                            scan_failed = true;
                        }
                    }
                }
                files.entries.retain(|p, _| paths.contains(p));
                files.retry_paths.retain(|p| paths.contains(p));
                files.rescan_needed = scan_failed;
                paths
            }
        };
        let mut all = paths;
        all.extend(files.retry_paths.iter().cloned());
        for path in &all {
            self.read_file(&mut files, path);
        }
        let mut local: Map = Map::new();
        if let Some((_, Entry::Index(rows))) = files.entries.get(&self.index_path()) {
            for row in rows {
                if let Value::Object(map) = row {
                    local.insert(map.get("id").map(Value::py_str).unwrap_or_default(), row.clone());
                }
            }
        }
        let mut external = Vec::new();
        for (path, (_, entry)) in &files.entries {
            let Entry::Meta(entry) = entry else { continue };
            if path.file_name().and_then(|n| n.to_str()) != Some("meta.json") {
                continue;
            }
            let id = entry.get("id").map(Value::py_str).unwrap_or_default();
            if entry.get("_external").is_some_and(Value::truthy) {
                external.push(entry.clone());
            } else if entry.get("is_active").is_some_and(Value::truthy)
                || entry.get("segment_count").is_some_and(Value::truthy)
            {
                local.insert(id, Value::Object(entry.clone()));
            } else {
                local.remove(&id);
            }
        }
        let mut meetings: Vec<Map> = local.values().filter_map(|v| v.as_object().cloned()).collect();
        meetings.extend(external);
        let key = |row: &Map| match row.get("created_at") {
            Some(Value::Str(s)) if !s.is_empty() => s.clone(),
            _ => String::new(),
        };
        meetings.sort_by_key(|row| std::cmp::Reverse(key(row)));
        let needs_retry = !files.retry_paths.is_empty() || scan_failed;
        *self.meetings.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(meetings);
        self.needs_retry.store(needs_retry, Ordering::SeqCst);
    }
}

impl MeetingCatalog {
    #[must_use]
    pub fn new(
        base_path: &Path,
        external_paths: &[(PathBuf, Option<String>)],
        config: &InstallConfig,
        timings: Timings,
    ) -> Self {
        let resolve = |p: &Path| recog_core::paths::resolve(p);
        let base_path = resolve(base_path);
        let external_paths: Vec<(PathBuf, Option<String>)> =
            external_paths.iter().map(|(p, s)| (resolve(p), s.clone())).collect();
        let mut roots = vec![base_path.clone()];
        for (p, _) in &external_paths {
            if !roots.contains(p) {
                roots.push(p.clone());
            }
        }
        Self {
            inner: Arc::new(Inner {
                base_path,
                external_paths,
                roots,
                config: config.clone(),
                timings,
                files: Mutex::new(Files::default()),
                meetings: RwLock::new(Arc::new(Vec::new())),
                dirty: Mutex::new(BTreeSet::new()),
                wake: Event::new(),
                lock: AsyncMutex::new(()),
                needs_retry: AtomicBool::new(false),
                stopping: AtomicBool::new(false),
                started: AtomicBool::new(false),
                parsed: std::sync::atomic::AtomicUsize::new(0),
            }),
            task: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn base_path(&self) -> &Path {
        &self.inner.base_path
    }

    /// Every directory the catalog lists, the meetings directory first.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.inner.roots
    }

    /// How many files the catalog has read so far.
    #[must_use]
    pub fn files_read(&self) -> usize {
        self.inner.parsed.load(Ordering::SeqCst)
    }

    /// `start()`: events count from now (the observer runs already), then a first
    /// refresh, then the loop.
    pub async fn start(&self) {
        self.inner.started.store(true, Ordering::SeqCst);
        self.refresh(None).await;
        let inner = Arc::clone(&self.inner);
        let this = self.clone();
        *self.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(tokio::spawn(async move { this.run(inner).await }));
    }

    /// `stop()`.
    pub async fn stop(&self) {
        self.inner.stopping.store(true, Ordering::SeqCst);
        self.inner.wake.set();
        let task = self.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.inner.started.store(false, Ordering::SeqCst);
    }

    /// `on_any_event`: mark what an event touched dirty and wake the loop.
    pub fn on_any_event(&self, kind: EventKind, is_directory: bool, src: &Path, dest: Option<&Path>) {
        let inner = &self.inner;
        let mut paths = BTreeSet::new();
        for path in std::iter::once(src).chain(dest) {
            if path.as_os_str().is_empty() {
                continue;
            }
            if is_directory {
                if kind != EventKind::Modified && path.parent().is_some_and(|p| inner.roots.iter().any(|r| r == p)) {
                    paths.insert(path.join("meta.json"));
                }
            } else if path == inner.index_path()
                || (path.file_name().and_then(|n| n.to_str()) == Some("meta.json")
                    && path.parent().and_then(Path::parent).is_some_and(|p| inner.roots.iter().any(|r| r == p)))
            {
                paths.insert(path.to_path_buf());
            }
        }
        if !paths.is_empty() && inner.started.load(Ordering::SeqCst) && !inner.stopping.load(Ordering::SeqCst) {
            inner.dirty.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend(paths);
            inner.wake.set();
        }
    }

    async fn run(&self, inner: Arc<Inner>) {
        let mut deadline = tokio::time::Instant::now() + inner.timings.reconcile;
        while !inner.stopping.load(Ordering::SeqCst) {
            let mut timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            if inner.needs_retry.load(Ordering::SeqCst) {
                timeout = timeout.min(inner.timings.retry);
            }
            inner.wake.wait_timeout(timeout).await;
            if inner.stopping.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(inner.timings.debounce).await;
            let dirty = std::mem::take(&mut *inner.dirty.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            inner.wake.clear();
            let full = tokio::time::Instant::now() >= deadline;
            self.refresh_inner(if full { None } else { Some(dirty) }, true).await;
            if full {
                deadline = tokio::time::Instant::now() + inner.timings.reconcile;
            }
        }
    }

    /// `refresh(paths)`: serialized with every other refresh; `None` rescans.
    pub async fn refresh(&self, paths: Option<BTreeSet<PathBuf>>) {
        self.refresh_inner(paths, false).await;
    }

    async fn refresh_inner(&self, paths: Option<BTreeSet<PathBuf>>, from_loop: bool) {
        let _guard = self.inner.lock.lock().await;
        let inner = Arc::clone(&self.inner);
        // The worker publishes its own result: dropping this future cannot leave a
        // second worker running beside it (the file cache stays locked meanwhile).
        let _ = tokio::task::spawn_blocking(move || inner.refresh_files(paths)).await;
        if !from_loop && self.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some() {
            self.inner.wake.set();
        }
    }

    /// `list_meetings()`: copies of the snapshot, active meetings with their current
    /// duration.
    #[must_use]
    pub fn list_meetings(&self) -> Vec<Value> {
        let snapshot = Arc::clone(&self.inner.meetings.read().unwrap_or_else(std::sync::PoisonError::into_inner));
        let now = pytime::now_utc(&SystemClock);
        snapshot
            .iter()
            .map(|entry| {
                let mut row = entry.clone();
                if row.get("is_active").is_some_and(Value::truthy) {
                    let seconds = match row.get("created_at") {
                        Some(Value::Str(created)) => pytime::fromisoformat(created)
                            .ok()
                            .and_then(|created| now.minus(&created).ok())
                            .map_or(0, |micros| micros / 1_000_000),
                        _ => 0,
                    };
                    row.insert("duration_seconds", Value::Int(i128::from(seconds)));
                }
                Value::Object(row)
            })
            .collect()
    }
}
