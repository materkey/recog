//! The `[zoom-app] …` lines on stderr: the helper state once per change, and what was
//! left out once per meeting. The prefix and wording are the Python module's, so the
//! log reads the same whichever implementation runs.

use std::io::Write;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::helper_app::HELPER_NAME;
use crate::py::{self, PyError, PyResult};

/// Where the lines go: stderr, or a buffer in tests. Cheap to clone.
#[derive(Clone)]
pub struct Logger(Arc<Mutex<Box<dyn Write + Send>>>);

impl Logger {
    pub fn stderr() -> Self {
        Self::to(Box::new(std::io::stderr()))
    }

    pub fn to(sink: Box<dyn Write + Send>) -> Self {
        Self(Arc::new(Mutex::new(sink)))
    }

    /// A logger into a shared buffer, and the buffer.
    pub fn buffer() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (Self::to(Box::new(SharedBuffer(Arc::clone(&buffer)))), buffer)
    }

    /// One `[zoom-app] <message>` line.
    pub fn line(&self, message: &str) {
        let mut sink = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(sink, "[zoom-app] {message}");
        let _ = sink.flush();
    }
}

struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn message(state: &str) -> Option<String> {
    Some(match state {
        "not_trusted" => format!(
            "\"{HELPER_NAME}\" has no Accessibility permission — Zoom meetings are not detected \
             (System Settings → Privacy & Security → Accessibility)"
        ),
        "zoom_not_running" => "Zoom is not running".into(),
        "idle" => "Zoom is running, no meeting".into(),
        "meeting" => "Zoom meeting detected".into(),
        _ => return None,
    })
}

/// One line per change of the helper state; `_StatusLog` of the Python module.
pub struct StatusLog {
    pub state: Option<Value>,
    unread_noted: bool,
    own_mic_noted: bool,
    logger: Logger,
}

impl StatusLog {
    pub fn new(logger: Logger) -> Self {
        Self { state: None, unread_noted: false, own_mic_noted: false, logger }
    }

    /// Whether the last helper state was `state`.
    pub fn is(&self, state: &str) -> bool {
        self.state.as_ref().and_then(Value::as_str) == Some(state)
    }

    /// Note the helper state; a changed state is logged and opens a new "meeting" for
    /// the once-per-meeting notes. A non-empty list or object as the state fails, as
    /// it does in Python (an unhashable dictionary key).
    pub fn note(&mut self, state: Option<&Value>) -> PyResult<()> {
        let same = match (state, self.state.as_ref()) {
            (None, None) => true,
            (Some(new), Some(old)) => py::eq(new, old),
            _ => false,
        };
        if same {
            return Ok(());
        }
        self.state = state.cloned();
        self.unread_noted = false;
        self.own_mic_noted = false;
        let key = state.filter(|value| py::truthy(value));
        let text = match key {
            None => None,
            Some(Value::String(state)) => message(state),
            Some(Value::Array(_) | Value::Object(_)) => return Err(PyError("unhashable type".into())),
            Some(_) => None,
        };
        let text = text.unwrap_or_else(|| format!("unknown helper state: {}", py::display_opt(state)));
        self.logger.line(&text);
        Ok(())
    }

    /// Once per meeting: tiles that are left out because their naming is not understood.
    pub fn note_unread(&mut self, unread: &[Value]) {
        if unread.is_empty() || self.unread_noted {
            return;
        }
        self.unread_noted = true;
        self.logger.line(&format!(
            "{} participant tile(s) ignored: Zoom names them in a way this version does not know, e.g. {}",
            unread.len(),
            py::json_dumps(&unread[0])
        ));
    }

    /// Once per meeting: the meeting menu's audio item says nothing known.
    pub fn note_own_mic_unknown(&mut self, unknown: bool) {
        if !unknown || self.own_mic_noted {
            return;
        }
        self.own_mic_noted = true;
        self.logger.line(
            "the meeting menu has no known audio item: your microphone follows your tile, or counts as muted \
             without one (as for a webinar attendee)",
        );
    }
}
