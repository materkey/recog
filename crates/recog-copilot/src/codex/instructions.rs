//! The Copilot's developer instructions on Codex (plan D5): the session agent's standing
//! text and the copilot-mode playbook from the installation, without their front matter,
//! after a note on what differs from Claude Code.

use std::path::Path;

/// What a Codex thread must know that the Claude-oriented playbook does not say.
pub const CODEX_NOTE: &str = "You run under Codex, not Claude Code. Each Copilot event arrives as a user turn \
holding one <channel source=\"copilot\" ...> block, the same block the playbook below describes. Nothing is \
pushed between turns, so never call await_copilot_event. Your written answer in a turn is shown to nobody: \
reach the user only through send_copilot_message (and set_copilot_status), and stay silent otherwise, as the \
playbook says. Do not run shell commands or edit files.";

/// The instructions from `repo_root`'s `.claude` files.
///
/// # Errors
/// A playbook file that cannot be read.
pub fn developer_instructions(repo_root: &Path) -> Result<String, String> {
    let agent = body(&repo_root.join(".claude/agents/copilot-session.md"))?;
    let playbook = body(&repo_root.join(".claude/skills/copilot-mode/SKILL.md"))?;
    Ok(format!("{CODEX_NOTE}\n\n{agent}\n\n{playbook}"))
}

/// The file's text after its `---` front matter, trimmed.
fn body(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let rest =
        text.strip_prefix("---\n").and_then(|front| front.split_once("\n---\n").map(|(_, rest)| rest)).unwrap_or(&text);
    Ok(rest.trim().to_owned())
}
