//! stderr logging with the prefixes the Python modules use, since tools and people grep
//! the service logs for them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The supervisor's prefix, as in `recog/copilot_supervisor.py`.
pub const SUPERVISOR_PREFIX: &str = "[copilot-supervisor]";

/// How often one key may log, as `recog/log_utils.py` defaults it.
pub const RATE_LIMIT_INTERVAL: Duration = Duration::from_secs(30);

/// key -> (when it last logged, messages suppressed since).
static RATE_LIMITED: OnceLock<Mutex<HashMap<String, (Instant, u64)>>> = OnceLock::new();

thread_local! {
    /// Lines logged on this thread while a [`Capture`] is alive (tests read them the
    /// way the Python tests read `capsys`).
    static CAPTURED: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// One line on stderr.
pub fn line(message: &str) {
    eprintln!("{message}");
    CAPTURED.with(|captured| {
        if let Some(lines) = captured.borrow_mut().as_mut() {
            lines.push(message.to_owned());
        }
    });
}

/// Logs `message` at most once per [`RATE_LIMIT_INTERVAL`] for each `key`; the next
/// line after a quiet spell says how many were suppressed, as `log_rate_limited` does.
pub fn rate_limited(key: &str, message: &str) {
    let now = Instant::now();
    let suppressed = {
        let mut state = RATE_LIMITED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        match state.get_mut(key) {
            Some((last, suppressed)) if now.duration_since(*last) < RATE_LIMIT_INTERVAL => {
                *suppressed += 1;
                return;
            }
            Some(entry) => std::mem::replace(entry, (now, 0)).1,
            None => {
                state.insert(key.to_owned(), (now, 0));
                0
            }
        }
    };
    if suppressed > 0 {
        line(&format!("{message} (+{suppressed} suppressed)"));
    } else {
        line(message);
    }
}

/// Forgets every key, so that a test sees its first message logged.
pub fn reset_rate_limits() {
    if let Some(state) = RATE_LIMITED.get() {
        state.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// Collects what this thread logs until dropped.
pub struct Capture(());

impl Capture {
    pub fn start() -> Self {
        CAPTURED.with(|captured| *captured.borrow_mut() = Some(Vec::new()));
        Capture(())
    }

    /// Everything logged so far, one string.
    pub fn text(&self) -> String {
        CAPTURED.with(|captured| captured.borrow().as_ref().map(|lines| lines.join("\n")).unwrap_or_default())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        CAPTURED.with(|captured| *captured.borrow_mut() = None);
    }
}
