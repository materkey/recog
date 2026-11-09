//! The KTalk desktop integration of recog in Rust (docs/plans/2026-11-07-rust-migration.md,
//! "Target layout"), in place of `recog/desktop/`: the observer the web server hosts
//! ([`service::DesktopIntegration`]) and `status()` behind `GET /api/desktop/status`,
//! the short-lived inspector worker ([`worker`]), the process checks ([`processes`],
//! psutil's role through [`system`]), the private state and extension snapshots
//! ([`storage`]) and `recog desktop {enable,disable,attach,status}` ([`cli`], the
//! `recog-desktop` binary). `crates/recog-desktop/bootstrap.js` stays JavaScript and is
//! embedded unchanged.
//!
//! The files under `<data dir>/desktop/` are Python's byte for byte, so an installation
//! moved from the Python modules keeps its state; `tests/ported.rs` holds the answers
//! the Python modules gave on scripted processes and a fake inspector, kept since the
//! Python side was deleted. Every OS query goes through
//! [`host::Host`], which tests replace as the Python tests monkeypatch psutil.
//!
//! Known differences from Python:
//! - a crash prints the exception's last line (`Kind: message`), not a traceback;
//! - `psutil.NoSuchProcess`/`AccessDenied` messages never name the process (psutil's
//!   do once it has read the name; only an observer log line can show one);
//! - macOS: a process start time is not corrected for a system clock change after
//!   boot (psutil adjusts by the boot time's drift);
//! - macOS: the environment is cut before the trailing strings XNU appends for dyld
//!   (`ptr_munge=`, `th_port=`, ...) by their names, since sysinfo reads past the
//!   empty string where psutil stops; an unknown new one would stay (none is a name
//!   the integration reads);
//! - macOS: the listening sockets come from a scan of every process (listeners has no
//!   per-process call), a few tens of milliseconds per check;
//! - an unreadable `Info.plist` is `InvalidFileException("Invalid file")` whatever the
//!   parser says (plistlib raises expat's own error for broken XML);
//! - `json.loads` of `app.asar` metadata reads UTF-8 only (Python also detects UTF-16
//!   and UTF-32);
//! - inspector URLs are split as Python 3.12.15's `urlsplit` splits them; older 3.12
//!   patch releases (3.12.8 is one) lack its bracketed-netloc check, so with them
//!   Python refuses `ws://x[::1]:9229/x` as `invalid_inspector_endpoint` where this
//!   says `attachment_failed` (no real inspector reports such a URL);
//! - `/json/list` is read with one-second timeouts on connect and each read, without
//!   `Accept-Encoding` (httpx: one second per operation, and it would decompress);
//! - the attachment deadline interrupts the worker at its awaits; a process query
//!   that hangs is not cut short (Python's runs in a thread the deadline abandons);
//! - `shutil.copytree` also copies extended attributes and file flags; the snapshot
//!   keeps contents, permission bits and times;
//! - the worker is the `recog-desktop worker` (or `recog-web desktop-worker`)
//!   subcommand, not `python -m recog.desktop.worker`.
#![forbid(unsafe_code)]

pub mod cli;
pub mod host;
pub mod inspector;
pub mod processes;
pub mod py;
pub mod service;
pub mod shlex;
pub mod storage;
pub mod system;
pub mod worker;

use std::path::PathBuf;

/// The checkout the binary is installed in (`<repo>/bin/`), else the one this crate
/// was built from (`crates/recog-desktop/../..`).
#[must_use]
pub fn default_repo_root() -> PathBuf {
    if let Some(repo) = recog_core::install::installed_repo() {
        return repo;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&root).unwrap_or(root)
}
