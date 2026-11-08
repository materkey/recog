//! The file system observer of the lifespan (watchdog's `Observer` in Python): the
//! meetings directory for the live watcher, the per-meeting watcher and the catalog,
//! the external meeting directories for the catalog alone.
//!
//! Events are hints. The two transcript watchers react to every event that leaves the
//! file in place (created, modified, renamed onto it), where watchdog's handler reacts
//! to modifications only: rereading is idempotent thanks to the seen-ID watermark, and
//! FSEvents often reports an append to a new file as one creation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};
use notify::{EventKind as NotifyKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::mpsc;

use crate::meeting_catalog::{EventKind, MeetingCatalog};
use crate::server::AppState;

/// One event as watchdog describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsEvent {
    pub kind: EventKind,
    pub is_directory: bool,
    pub src: PathBuf,
    pub dest: Option<PathBuf>,
    /// Whether it came from the meetings directory (else an external one).
    pub meetings: bool,
}

/// The running observer; dropping it stops watching.
pub struct FsWatcher {
    _watchers: Vec<RecommendedWatcher>,
}

fn translate(event: &notify::Event) -> Vec<(EventKind, bool, PathBuf, Option<PathBuf>)> {
    let paths = &event.paths;
    let one = |kind: EventKind, dir: Option<bool>| {
        paths.iter().map(|p| (kind, dir.unwrap_or_else(|| p.is_dir()), p.clone(), None)).collect::<Vec<_>>()
    };
    match &event.kind {
        NotifyKind::Create(CreateKind::Folder) => one(EventKind::Created, Some(true)),
        NotifyKind::Create(CreateKind::File) => one(EventKind::Created, Some(false)),
        NotifyKind::Create(_) => one(EventKind::Created, None),
        NotifyKind::Remove(RemoveKind::Folder) => one(EventKind::Deleted, Some(true)),
        NotifyKind::Remove(_) => one(EventKind::Deleted, Some(false)),
        NotifyKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() == 2 => {
            vec![(EventKind::Moved, paths[1].is_dir(), paths[0].clone(), Some(paths[1].clone()))]
        }
        NotifyKind::Modify(ModifyKind::Name(_)) => paths
            .iter()
            .map(|p| {
                if p.exists() {
                    (EventKind::Created, p.is_dir(), p.clone(), None)
                } else {
                    (EventKind::Deleted, false, p.clone(), None)
                }
            })
            .collect(),
        NotifyKind::Modify(_) | NotifyKind::Any | NotifyKind::Other => one(EventKind::Modified, None),
        NotifyKind::Access(_) => Vec::new(),
    }
}

fn watch(path: &Path, tx: mpsc::UnboundedSender<FsEvent>, meetings: bool) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        for (kind, is_directory, src, dest) in translate(&event) {
            let _ = tx.send(FsEvent { kind, is_directory, src, dest, meetings });
        }
    })?;
    watcher.watch(path, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// Hands one event to the watchers.
pub fn dispatch(state: &Arc<AppState>, catalog: &MeetingCatalog, event: &FsEvent) {
    catalog.on_any_event(event.kind, event.is_directory, &event.src, event.dest.as_deref());
    if !event.meetings || event.kind == EventKind::Deleted {
        return;
    }
    let path = event.dest.as_deref().unwrap_or(&event.src);
    state.live_watcher.on_modified(path, event.is_directory);
    state.transcript_watcher.on_modified(path, event.is_directory);
}

impl FsWatcher {
    /// Watches the meetings directory and every external one that is a directory; a
    /// failure leaves the catalog to its periodic reconciliation.
    pub fn start(state: &Arc<AppState>, catalog: &MeetingCatalog) -> Option<FsWatcher> {
        let (tx, mut rx) = mpsc::unbounded_channel::<FsEvent>();
        let mut watchers = Vec::new();
        match watch(&state.config.meetings_dir, tx.clone(), true) {
            Ok(w) => watchers.push(w),
            Err(e) => eprintln!("[observer] cannot watch {}: {e}", state.config.meetings_dir.display()),
        }
        for root in catalog.roots().iter().skip(1) {
            if !root.is_dir() {
                continue;
            }
            match watch(root, tx.clone(), false) {
                Ok(w) => watchers.push(w),
                Err(e) => {
                    eprintln!("[Catalog] Cannot watch {}; using periodic reconciliation: {e}", root.display());
                }
            }
        }
        let state = Arc::clone(state);
        let catalog = catalog.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                dispatch(&state, &catalog, &event);
            }
        });
        Some(FsWatcher { _watchers: watchers })
    }
}
