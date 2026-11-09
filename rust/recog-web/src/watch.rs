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
        NotifyKind::Create(kind) => one(EventKind::Created, created_directory(*kind)),
        NotifyKind::Remove(kind) => one(EventKind::Deleted, Some(matches!(kind, RemoveKind::Folder))),
        NotifyKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() == 2 => {
            vec![(EventKind::Moved, paths[1].is_dir(), paths[0].clone(), Some(paths[1].clone()))]
        }
        NotifyKind::Modify(ModifyKind::Name(_)) => paths.iter().map(|p| renamed(p)).collect(),
        NotifyKind::Modify(_) | NotifyKind::Any | NotifyKind::Other => one(EventKind::Modified, None),
        NotifyKind::Access(_) => Vec::new(),
    }
}

/// Whether a creation was a directory, when notify says; else the path decides.
fn created_directory(kind: CreateKind) -> Option<bool> {
    match kind {
        CreateKind::Folder => Some(true),
        CreateKind::File => Some(false),
        _ => None,
    }
}

/// One side of a rename: a creation when the path exists, else a deletion.
fn renamed(path: &Path) -> (EventKind, bool, PathBuf, Option<PathBuf>) {
    if path.exists() {
        (EventKind::Created, path.is_dir(), path.to_path_buf(), None)
    } else {
        (EventKind::Deleted, false, path.to_path_buf(), None)
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

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, DataChange, MetadataKind};

    use super::*;

    fn events(kind: NotifyKind, paths: &[&Path]) -> Vec<(EventKind, bool, PathBuf, Option<PathBuf>)> {
        let event = paths.iter().fold(notify::Event::new(kind), |event, path| event.add_path(path.to_path_buf()));
        translate(&event)
    }

    #[test]
    fn creations_and_removals_when_translated_then_watchdog_kinds_with_the_directory_flag() {
        let dir = tempfile::tempdir().unwrap();
        let (folder, file) = (dir.path().join("d"), dir.path().join("f"));
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&file, "").unwrap();

        for (kind, path, expected) in [
            (NotifyKind::Create(CreateKind::Folder), file.as_path(), (EventKind::Created, true)),
            (NotifyKind::Create(CreateKind::File), folder.as_path(), (EventKind::Created, false)),
            (NotifyKind::Create(CreateKind::Any), folder.as_path(), (EventKind::Created, true)),
            (NotifyKind::Create(CreateKind::Other), file.as_path(), (EventKind::Created, false)),
            (NotifyKind::Remove(RemoveKind::Folder), file.as_path(), (EventKind::Deleted, true)),
            (NotifyKind::Remove(RemoveKind::File), folder.as_path(), (EventKind::Deleted, false)),
            (NotifyKind::Remove(RemoveKind::Any), folder.as_path(), (EventKind::Deleted, false)),
        ] {
            assert_eq!(events(kind, &[path]), [(expected.0, expected.1, path.to_path_buf(), None)], "{kind:?}");
        }
    }

    #[test]
    fn renames_when_translated_then_a_move_or_a_creation_and_a_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let (gone, here) = (dir.path().join("gone"), dir.path().join("here"));
        std::fs::create_dir(&here).unwrap();
        let both = NotifyKind::Modify(ModifyKind::Name(RenameMode::Both));

        assert_eq!(events(both, &[&gone, &here]), [(EventKind::Moved, true, gone.clone(), Some(here.clone()))]);
        assert_eq!(
            events(NotifyKind::Modify(ModifyKind::Name(RenameMode::From)), &[&gone, &here]),
            [(EventKind::Deleted, false, gone.clone(), None), (EventKind::Created, true, here.clone(), None)]
        );
        assert_eq!(events(both, &[&here]), [(EventKind::Created, true, here, None)]);
    }

    #[test]
    fn modifications_and_accesses_when_translated_then_modified_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "").unwrap();

        for kind in [
            NotifyKind::Modify(ModifyKind::Data(DataChange::Content)),
            NotifyKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
            NotifyKind::Any,
            NotifyKind::Other,
        ] {
            assert_eq!(events(kind, &[&file]), [(EventKind::Modified, false, file.clone(), None)], "{kind:?}");
        }
        assert!(events(NotifyKind::Access(AccessKind::Any), &[&file]).is_empty());
    }
}
