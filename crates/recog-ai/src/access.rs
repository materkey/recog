//! The AI functions level: whether recog may start `claude` or `codex` at all, only on
//! a click, or also on its own after each meeting (docs/plans/2026-11-23-ai-functions-switch-design.md).
//!
//! It is the `access` field of `ai-settings.json`. A file without the field is an
//! installation from before the level existed and counts as `Automatic`.

use std::path::Path;

use crate::json::Json;

/// What recog may send to a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// A new installation that has not answered; works as `Off`.
    Unasked,
    /// Neither CLI is started.
    Off,
    /// Only on a click: an analysis, a question, a title, the Copilot started by hand.
    Manual,
    /// `Manual`, plus the summary and title after each meeting and the Copilot autostart.
    Automatic,
}

impl Access {
    /// The value stored in the settings file.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Unasked => "unasked",
            Access::Off => "off",
            Access::Manual => "manual",
            Access::Automatic => "automatic",
        }
    }

    /// The level a stored value names, if any.
    #[must_use]
    pub fn parse(text: &str) -> Option<Access> {
        [Access::Unasked, Access::Off, Access::Manual, Access::Automatic]
            .into_iter()
            .find(|level| level.as_str() == text)
    }

    /// Whether a click may start a provider.
    #[must_use]
    pub fn allows_manual(self) -> bool {
        matches!(self, Access::Manual | Access::Automatic)
    }

    /// Whether recog may start a provider on its own after a meeting.
    #[must_use]
    pub fn allows_automatic(self) -> bool {
        self == Access::Automatic
    }
}

/// Whether a run before this one left a finished meeting: a `<dir>/meta.json` with a
/// non-empty `ended_at`. The current run's own recording has none yet. A missing
/// directory and an unreadable or malformed meta count as no meeting.
#[must_use]
pub fn has_finished_meeting(meetings_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(meetings_dir) else { return false };
    entries.flatten().any(|entry| meta_has_ended(&entry.path().join("meta.json")))
}

fn meta_has_ended(meta: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(meta) else { return false };
    let Ok(meta) = Json::parse(&text) else { return false };
    meta.get("ended_at").and_then(Json::as_str).is_some_and(|ended| !ended.is_empty())
}
