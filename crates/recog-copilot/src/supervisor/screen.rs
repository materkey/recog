//! What the Copilot pane shows (`classify_screen`) and how to answer its startup
//! dialogs (`trust_dialog_keys`).
//!
//! Markers frozen on claude 2.1.197 rendered screens (the spike fixtures); after a
//! claude update re-capture the fixtures and re-run the classify tests.

use recog_core::protocol;

use crate::py;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Spawning,
    Online,
    Working,
    Blocked,
    Offline,
}

impl AgentState {
    /// The `status` of a `copilot_agent_status` frame.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentState::Spawning => protocol::AGENT_SPAWNING,
            AgentState::Online => protocol::AGENT_ONLINE,
            AgentState::Working => protocol::AGENT_WORKING,
            AgentState::Blocked => protocol::AGENT_BLOCKED,
            AgentState::Offline => protocol::AGENT_OFFLINE,
        }
    }
}

pub const BLOCKED_MARKERS: [&str; 3] = ["enter to confirm", "esc to cancel", "❯ 1."];
pub const WORKING_MARKER: &str = "esc to interrupt";
pub const PROMPT_CHAR: &str = "❯";
pub const DEV_CHANNELS_MARKER: &str = "local development";
/// The workspace-trust dialog (fresh spawns when the project pre-approves permissions
/// in `.claude/settings.local.json`), current and older wording.
pub const TRUST_DIALOG_MARKERS: [&str; 2] = ["trust this folder", "trust the files in this folder"];
/// Dialogs safe to answer: dev-channels (option 1, "Yes, load them") and workspace
/// trust (the same repository the supervisor spawned the session into), in this order.
pub const AUTO_DISMISS_MARKERS: [&str; 3] = [DEV_CHANNELS_MARKER, TRUST_DIALOG_MARKERS[0], TRUST_DIALOG_MARKERS[1]];
pub const TRUST_ACCEPT_OPTION: &str = "yes, i trust";
pub const PROMPT_TAIL_LINES: usize = 15;

fn line_index(lines: &[&str], needle: &str) -> Option<usize> {
    lines.iter().position(|line| line.contains(needle))
}

/// Keys that move the selection onto the accept option and confirm it. Driven by the
/// rendered lines rather than a fixed key: claude 2.1.270 dropped the option numbers
/// and starts the cursor on "No, exit". `1` when the layout is not recognised.
pub fn trust_dialog_keys(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    let lines = py::splitlines(&lowered);
    let (Some(accept), Some(cursor)) = (line_index(&lines, TRUST_ACCEPT_OPTION), line_index(&lines, PROMPT_CHAR))
    else {
        return vec!["1".to_owned()];
    };
    let step = if accept > cursor { "Down" } else { "Up" };
    let mut keys = vec![step.to_owned(); accept.abs_diff(cursor)];
    keys.push("Enter".to_owned());
    keys
}

/// Whether a dialog waits for a choice (its footer or a numbered cursor), given the
/// lower-cased pane.
pub fn dialog_shown(lowered: &str) -> bool {
    BLOCKED_MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// The agent state a captured pane shows. First match wins: no session → dialog
/// footer → spinner → prompt → transitional. `None` means the tmux session is gone;
/// an empty pane is a startup screen, not offline.
pub fn classify_screen(text: Option<&str>) -> AgentState {
    let Some(text) = text else { return AgentState::Offline };
    let lowered = text.to_lowercase();
    if dialog_shown(&lowered) {
        return AgentState::Blocked;
    }
    if lowered.contains(WORKING_MARKER) {
        return AgentState::Working;
    }
    let lines = py::splitlines(py::rstrip(text));
    let tail = &lines[lines.len().saturating_sub(PROMPT_TAIL_LINES)..];
    if tail.iter().any(|line| line.contains(PROMPT_CHAR)) {
        return AgentState::Online;
    }
    AgentState::Spawning
}
