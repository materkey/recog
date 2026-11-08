//! The source's life: start the helper, serve its snapshots, start it again.
//!
//! A helper without the Accessibility permission reports `not_trusted` and exits:
//! macOS answers `AXIsProcessTrusted()` from a per-process cache, so a running helper
//! never learns of a grant. A fresh one is started every 15 s, told not to show the
//! system dialog again; any other end of the helper's stream is a failure.

use std::path::PathBuf;
use std::time::Duration;

use crate::bridge::{self, Connector, PumpError};
use crate::helper_link::{self, LaunchError, Launcher};
use crate::log::{Logger, StatusLog};
use crate::session::ZoomAppSession;

/// The timings of the Python module, adjustable for tests.
#[derive(Debug, Clone)]
pub struct Timing {
    /// Between two attempts to reach the bridge.
    pub reconnect: Duration,
    /// Between helpers started while the permission is missing.
    pub permission_retry: Duration,
    /// How long a launched helper has to connect back.
    pub helper_connect: Duration,
    /// Background mode: before starting over after a failure.
    pub restart: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            reconnect: Duration::from_secs(3),
            permission_retry: Duration::from_secs(15),
            helper_connect: helper_link::HELPER_CONNECT,
            restart: Duration::from_secs(30),
        }
    }
}

/// Why a run ended. It never ends on its own: the helper is started again after it
/// reported the missing permission, and the bridge is reconnected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The helper application could not be built or found.
    Build(String),
    /// The helper could not be launched or did not connect.
    Launch(LaunchError),
    /// The helper's stream ended or carried a line that cannot be read.
    Helper(PumpError),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Build(reason) => f.write_str(reason),
            Self::Launch(error) => error.fmt(f),
            Self::Helper(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RunError {}

/// Serve the helper application at `app` with `session` until a failure.
pub async fn run_with(
    app: PathBuf,
    mut session: ZoomAppSession,
    launcher: &impl Launcher,
    connector: &impl Connector,
    bridge_url: &str,
    timing: &Timing,
    logger: &Logger,
) -> RunError {
    let mut log = StatusLog::new(logger.clone());
    let mut prompt = true;
    loop {
        let mut lines = match helper_link::helper_lines(&app, prompt, launcher, timing.helper_connect).await {
            Ok(lines) => lines,
            Err(error) => return RunError::Launch(error),
        };
        let ended =
            bridge::serve(connector, bridge_url, &mut lines, &mut session, &mut log, logger, timing.reconnect).await;
        drop(lines);
        // A helper without the permission exits after reporting it; anything else that
        // ends the stream is a failure.
        if ended != PumpError::HelperGone || !log.is("not_trusted") {
            return RunError::Helper(ended);
        }
        prompt = false;
        tokio::time::sleep(timing.permission_retry).await;
    }
}

/// Where the source looks for its parts.
#[derive(Debug, Clone)]
pub struct Options {
    /// The checkout: the Swift source and the helper bundle live there.
    pub repo_root: PathBuf,
    pub bridge_url: String,
    /// The installed Zoom applications whose wording is read.
    pub zoom_apps: Vec<PathBuf>,
    pub timing: Timing,
}

impl Options {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            bridge_url: bridge::BRIDGE_URL.into(),
            zoom_apps: crate::vocabulary::default_app_paths(),
            timing: Timing::default(),
        }
    }
}

/// The checkout this crate was built from (`rust/recog-zoom/../..`).
pub fn default_repo_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// `run()` of the Python module: the installed Zoom's wording, the helper built when
/// needed, `open` for the launch and the WebSocket to the bridge.
#[cfg(target_os = "macos")]
pub async fn run(options: &Options, logger: &Logger) -> RunError {
    let session = ZoomAppSession::new(crate::vocabulary::load_vocabulary(&options.zoom_apps));
    let identity = match crate::settings::codesign_identity_from_env(&options.repo_root) {
        Ok(identity) => identity,
        Err(reason) => return RunError::Build(reason),
    };
    let toolchain = crate::helper_app::SystemToolchain;
    let app = match crate::helper_app::find_or_build_helper(&options.repo_root, &identity, &toolchain, logger) {
        Ok(app) => app,
        Err(error) => return RunError::Build(error.0),
    };
    let connector = bridge::WsConnector::default();
    run_with(app, session, &helper_link::OpenLauncher, &connector, &options.bridge_url, &options.timing, logger).await
}

/// Keep a source running: log why a run ended and start over after `timing.restart`,
/// until `stop` completes. A failure must not take the caller down with it.
pub async fn supervise<F, R>(start: F, restart: Duration, logger: &Logger, stop: impl std::future::Future<Output = ()>)
where
    F: Fn() -> R,
    R: std::future::Future<Output = RunError>,
{
    tokio::pin!(stop);
    loop {
        tokio::select! {
            biased;
            () = &mut stop => return,
            error = start() => logger.line(&format!(
                "stopped: {error}; starting over in {:.0} s",
                restart.as_secs_f64()
            )),
        }
        tokio::select! {
            biased;
            () = &mut stop => return,
            () = tokio::time::sleep(restart) => {}
        }
    }
}

/// Run the source in a thread of the calling process (`start_in_background()`);
/// `None` off macOS. The thread lives as long as the process.
pub fn spawn_in_background(options: Options) -> Option<std::thread::JoinHandle<()>> {
    let logger = Logger::stderr();
    #[cfg(not(target_os = "macos"))]
    {
        let _ = options;
        logger.line("the native Zoom source supports macOS only — not started");
        None
    }
    #[cfg(target_os = "macos")]
    {
        let thread = std::thread::Builder::new().name("zoom-app".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            match runtime {
                Ok(runtime) => runtime.block_on(supervise(
                    || run(&options, &logger),
                    options.timing.restart,
                    &logger,
                    std::future::pending(),
                )),
                Err(error) => logger.line(&format!("stopped: cannot start a runtime: {error}")),
            }
        });
        match thread {
            Ok(thread) => Some(thread),
            Err(error) => {
                Logger::stderr().line(&format!("cannot start the source thread: {error}"));
                None
            }
        }
    }
}
