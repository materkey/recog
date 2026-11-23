//! The Codex Copilot's developer instructions, from the checkout's own playbook files.

use std::path::Path;

use recog_copilot::codex::instructions::{CODEX_NOTE, developer_instructions};

#[test]
fn checkout_playbook_when_read_then_note_agent_and_playbook_without_front_matter() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    let text = developer_instructions(&root).unwrap();

    assert!(text.starts_with(CODEX_NOTE));
    assert!(text.contains("You are the local copilot for a live KTalk meeting."));
    assert!(text.contains("# Copilot Mode — operating playbook"));
    assert!(!text.contains("initialPrompt:"));
    assert!(!text.contains("when_to_use:"));
}

#[test]
fn missing_playbook_when_read_then_an_error_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();

    let error = developer_instructions(dir.path()).unwrap_err();

    assert!(error.contains("copilot-session.md"), "{error}");
}
