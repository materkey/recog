//! The shared privacy preference and name masking at the Copilot MCP boundary — the
//! port of `recog/privacy.py`.
//!
//! `privacy.json` under the data root holds `{"enabled": bool}`. With it enabled,
//! every payload handed to the Copilot gets its speaker names replaced by stable
//! per-meeting aliases (`speaker_0`, `speaker_1`, …) and loses the user's identity.
//!
//! Known differences: Python reads the first 1024 *characters* of the file, the port
//! the first 1024 bytes decoded as UTF-8 (the same for an ASCII file, which is what
//! `write` produces); a malformed payload (segments that are not dicts, a
//! `recent_context` that is not a string, an unhashable `meeting_id`) makes Python
//! raise, while the port skips the segment or reads the value as empty.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::pyjson::{self, Map, Value};
use crate::{paths, py};

/// What went wrong reading or writing the setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivacyError {
    /// Python's `ValueError` (or `JSONDecodeError`) text.
    Invalid(String),
    /// Python's `OSError` text.
    Io(String),
}

impl std::fmt::Display for PrivacyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PrivacyError {}

/// The setting as `read()` returns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Privacy {
    pub enabled: bool,
    pub configured: bool,
}

/// Anything that says whether privacy is on; [`PrivacyStore`] reads the file.
pub trait PrivacySource {
    /// # Errors
    /// The setting cannot be read; masking then refuses to deliver anything.
    fn read(&self) -> Result<Privacy, PrivacyError>;
}

/// `PrivacyStore(data_dir)`.
#[derive(Debug, Clone)]
pub struct PrivacyStore {
    pub path: PathBuf,
}

impl PrivacyStore {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self { path: data_dir.join("privacy.json") }
    }

    /// `write(enabled)`.
    ///
    /// # Errors
    /// A broken existing file, or a failed write.
    pub fn write(&self, enabled: bool) -> Result<Privacy, PrivacyError> {
        self.read()?;
        let io = |e: &std::io::Error, p: &Path| PrivacyError::Io(paths::os_error_message(e, p));
        let dir = self.path.parent().unwrap_or(Path::new("."));
        paths::mkdir_parents(dir, 0o700).map_err(|e| io(&e, dir))?;
        let temp = paths::create_unique(dir, ".privacy-", "", 0o600).map_err(|e| io(&e, dir))?;
        let contents =
            pyjson::dumps(&Value::Object([("enabled".to_owned(), Value::Bool(enabled))].into_iter().collect()), None);
        paths::replace_with(&self.path, temp, contents.as_bytes(), true, None).map_err(|e| io(&e, &self.path))?;
        Ok(Privacy { enabled, configured: true })
    }
}

impl PrivacySource for PrivacyStore {
    fn read(&self) -> Result<Privacy, PrivacyError> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Privacy { enabled: false, configured: false });
            }
            Err(e) => return Err(PrivacyError::Io(paths::os_error_message(&e, &self.path))),
        };
        let mut raw = Vec::new();
        file.take(1024).read_to_end(&mut raw).map_err(|e| PrivacyError::Io(paths::os_error_message(&e, &self.path)))?;
        let text =
            std::str::from_utf8(&raw).map_err(|e| PrivacyError::Invalid(pyjson::utf8_error_message(&raw, &e)))?;
        let data = pyjson::loads(text).map_err(|e| PrivacyError::Invalid(e.0))?;
        match data.as_object().and_then(|m| m.get("enabled")) {
            Some(Value::Bool(enabled)) => Ok(Privacy { enabled: *enabled, configured: true }),
            _ => Err(PrivacyError::Invalid("Invalid privacy setting".into())),
        }
    }
}

/// `CopilotPrivacy(store)`: masks payloads while privacy is on, with aliases stable
/// per meeting for the last 16 meetings.
pub struct CopilotPrivacy<S: PrivacySource> {
    pub store: S,
    meetings: Vec<(Value, Map)>,
    current_meeting: Value,
}

impl<S: PrivacySource> CopilotPrivacy<S> {
    pub fn new(store: S) -> Self {
        Self { store, meetings: Vec::new(), current_meeting: Value::Null }
    }

    /// `apply(payload)`: the payload as it may leave for the Copilot. The setting is
    /// read at delivery, not enqueue time, so enabling privacy also masks events that
    /// were queued before.
    ///
    /// # Errors
    /// The setting cannot be read: nothing is delivered rather than names exposed.
    pub fn apply(&mut self, payload: &Map) -> Result<Map, PrivacyError> {
        if !self.store.read()?.enabled {
            return Ok(payload.clone());
        }
        let mut result = payload.clone();
        let first_segment_meeting = match result.get("segments").filter(|s| s.truthy()) {
            Some(Value::Array(segments)) => segments
                .first()
                .map(|s| s.as_object().and_then(|m| m.get("meeting_id")).cloned().unwrap_or(Value::Null)),
            _ => None,
        };
        let meeting = match result.get("meeting_id") {
            Some(id) => id.clone(),
            None => first_segment_meeting.unwrap_or_else(|| self.current_meeting.clone()),
        };
        self.current_meeting = meeting.clone();
        let at = match self.meetings.iter().position(|(id, _)| id.py_eq(&meeting)) {
            Some(at) => at,
            None => {
                self.meetings.push((meeting.clone(), Map::new()));
                self.meetings.len() - 1
            }
        };
        let entry = self.meetings.remove(at);
        self.meetings.push(entry);
        if self.meetings.len() > 16 {
            self.meetings.remove(0);
        }
        let names = &mut self.meetings.last_mut().unwrap_or_else(|| unreachable!("just pushed")).1;
        let mut alias = |name: Option<&Value>| -> String {
            match name {
                Some(Value::Str(name)) if !name.is_empty() => {
                    let next = format!("speaker_{}", names.len());
                    names.setdefault(name, Value::from(next)).py_str()
                }
                _ => "speaker_?".to_owned(),
            }
        };
        for key in ["recent_segments", "new_segments", "segments"] {
            let Some(Value::Array(segments)) = result.get_mut(key) else { continue };
            for segment in segments.iter_mut() {
                let Some(segment) = segment.as_object_mut() else { continue };
                if segment.get("event_type").is_some_and(Value::truthy) {
                    continue;
                }
                let masked = alias(segment.get("speaker"));
                segment.insert("speaker", Value::from(masked.as_str()));
                if segment.contains_key("speaker_id") {
                    segment.insert("speaker_id", Value::from(masked));
                }
            }
        }
        if result.contains_key("recent_context") {
            let context = if let Some(Value::Array(segments)) = result.get("recent_segments") {
                segments
                    .iter()
                    .map(|segment| {
                        let field = |key: &str, fallback: &str| {
                            segment
                                .as_object()
                                .and_then(|m| m.get(key))
                                .filter(|v| v.truthy())
                                .map_or_else(|| fallback.to_owned(), Value::py_str)
                        };
                        format!("{}: {}", field("speaker", "?"), field("text", ""))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                // Compatibility with a bridge not yet restarted after upgrade.
                let text = result.get("recent_context").and_then(Value::as_str).unwrap_or_default().to_owned();
                py::splitlines(&text)
                    .into_iter()
                    .map(|line| match line.split_once(": ") {
                        Some((name, text)) => format!("{}: {text}", alias(Some(&Value::from(name)))),
                        None => line.to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            result.insert("recent_context", Value::from(context));
        }
        if result.contains_key("local_user_name") {
            let masked = alias(result.get("local_user_name"));
            result.insert("local_user_name", Value::from(masked));
        }
        if result.contains_key("local_user_aliases") {
            result.insert("local_user_aliases", Value::Array(Vec::new()));
        }
        if result.contains_key("local_user_identity_known") {
            result.insert("local_user_identity_known", Value::Bool(false));
        }
        if let Some(revision) = result.get("local_user_profile_revision") {
            let masked = format!("privacy:{}", revision.py_str());
            result.insert("local_user_profile_revision", Value::from(masked));
        }
        result.insert("privacy_mode", Value::Bool(true));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn object(text: &str) -> Map {
        pyjson::loads(text).unwrap().as_object().unwrap().clone()
    }

    #[test]
    fn missing_setting_when_read_then_default_without_creating_files() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let store = PrivacyStore::new(&dir.path().join("data"));
        // when / then
        assert_eq!(store.read(), Ok(Privacy { enabled: false, configured: false }));
        assert!(!dir.path().join("data").exists());
    }

    #[test]
    fn saved_setting_when_reopened_then_persists_with_private_permissions() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let store = PrivacyStore::new(dir.path());
        // when
        store.write(true).unwrap();
        // then
        assert_eq!(PrivacyStore::new(dir.path()).read(), Ok(Privacy { enabled: true, configured: true }));
        assert_eq!(fs::read_to_string(&store.path).unwrap(), "{\"enabled\": true}");
        assert_eq!(fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn privacy_enabled_when_delivering_then_masks_names_without_mutating_transcript() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let store = PrivacyStore::new(dir.path());
        store.write(true).unwrap();
        let mut privacy = CopilotPrivacy::new(store);
        let payload = object(
            r#"{"meeting_id": "m1", "recent_segments": [{"speaker": "Anna", "text": "hi"},
                {"speaker": "Система", "event_type": "session_pause", "text": "p"}],
                "recent_context": "Anna: hi", "local_user_name": "Anna", "local_user_aliases": ["Anna"],
                "local_user_identity_known": true, "local_user_profile_revision": "r1"}"#,
        );
        // when
        let masked = privacy.apply(&payload).unwrap();
        // then
        assert_eq!(
            pyjson::dumps(&Value::Object(masked), None),
            "{\"meeting_id\": \"m1\", \"recent_segments\": [{\"speaker\": \"speaker_0\", \"text\": \"hi\"}, \
             {\"speaker\": \"Система\", \"event_type\": \"session_pause\", \"text\": \"p\"}], \
             \"recent_context\": \"speaker_0: hi\\nСистема: p\", \"local_user_name\": \"speaker_0\", \
             \"local_user_aliases\": [], \"local_user_identity_known\": false, \
             \"local_user_profile_revision\": \"privacy:r1\", \"privacy_mode\": true}"
        );
        assert_eq!(payload.get("local_user_name"), Some(&Value::from("Anna")));
    }

    #[test]
    fn corrupt_setting_when_delivering_then_refuses_to_expose_names() {
        // given
        let dir = tempfile::tempdir().unwrap();
        let store = PrivacyStore::new(dir.path());
        fs::write(&store.path, "{\"enabled\": \"yes\"}").unwrap();
        let mut privacy = CopilotPrivacy::new(store);
        // when / then
        assert_eq!(privacy.apply(&Map::new()), Err(PrivacyError::Invalid("Invalid privacy setting".into())));
    }
}
