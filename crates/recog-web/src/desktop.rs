//! The KTalk desktop integration as the web server hosts it (`recog/desktop/`, ported
//! in `recog-desktop`): the observer the lifespan starts and stops, and
//! `GET /api/desktop/status`, which reads the files the integration keeps under
//! `<data dir>/desktop/`.
//!
//! The observer's inspector work runs in a child process, as Python's runs
//! `python -m recog.desktop.worker`: this binary again, as
//! `recog-web desktop-worker --root … (--target … | --recover)`.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use recog_core::pyjson::Value;
use recog_desktop::service::{Context, DesktopIntegration, WorkerCommand};

use crate::config::Config;

/// The first argument that makes `recog-web` the desktop worker.
pub const WORKER_ARGUMENT: &str = "desktop-worker";

/// `DesktopIntegration(DATA_DIR / "desktop")` on this system, its workers this
/// binary run with [`WORKER_ARGUMENT`] in the checkout.
#[must_use]
pub fn integration(config: &Config) -> Arc<DesktopIntegration> {
    let command = WorkerCommand {
        program: std::env::current_exe().unwrap_or_else(|_| "recog-web".into()),
        prefix: vec![OsString::from(WORKER_ARGUMENT)],
        cwd: Some(config.repo_root.clone()),
        env: Vec::new(),
    };
    DesktopIntegration::new(config.data_dir.join("desktop"), Context::system(command))
}

/// `status(root)`.
#[must_use]
pub fn status(root: &Path) -> Value {
    recog_desktop::service::status(root)
}
