//! The JavaScript of the extension and of the web UI spells the bridge protocol's
//! words by hand. This keeps both sides in step: renaming a word in
//! `recog_core::protocol` or in the JavaScript alone fails here.

use std::fs;
use std::path::{Path, PathBuf};

use recog_core::protocol as p;

/// The extension's sources (`browser-extension/src`) or the web UI (`app.js`).
#[derive(Clone, Copy, Debug)]
enum Side {
    Extension,
    WebUi,
}

/// Every word the JavaScript shares with Rust, and the sides that write it down.
const SHARED_WITH_JS: &[(&str, &[Side])] = &[
    (p::MEETING_JOIN, &[Side::Extension]),
    (p::MEETING_JOIN_REJECTED, &[Side::Extension]),
    (p::MEETING_LEAVE, &[Side::Extension]),
    (p::PARTICIPANTS_UPDATE, &[Side::Extension]),
    (p::SPEAKER_UPDATE, &[Side::Extension]),
    (p::MEETING_STARTED, &[Side::Extension]),
    (p::MEETING_ENDED, &[Side::Extension]),
    (p::TRANSCRIPT_REFINE, &[Side::Extension, Side::WebUi]),
    (p::COPILOT_CHAT_MESSAGE, &[Side::Extension, Side::WebUi]),
    (p::COPILOT_REPLY, &[Side::Extension, Side::WebUi]),
    (p::COPILOT_STATUS, &[Side::Extension, Side::WebUi]),
    (p::COPILOT_CHAT_ECHO, &[Side::WebUi]),
    (p::COPILOT_SPAWN_REQUEST, &[Side::WebUi]),
    (p::COPILOT_STOP_REQUEST, &[Side::WebUi]),
    (p::STATUS_AGENT_STATE, &[Side::Extension, Side::WebUi]),
    (p::STATUS_MANUAL_PREEMPTED, &[Side::WebUi]),
    (p::STATUS_CONFERENCE_ACTIVE, &[Side::WebUi]),
    (p::STATUS_SUPERVISOR_DISCONNECTED, &[Side::WebUi]),
    (p::STATUS_BRIDGE_DISCONNECTED, &[Side::WebUi]),
    (p::AGENT_SPAWNING, &[Side::Extension, Side::WebUi]),
    (p::AGENT_ONLINE, &[Side::Extension, Side::WebUi]),
    (p::AGENT_WORKING, &[Side::Extension, Side::WebUi]),
    (p::AGENT_BLOCKED, &[Side::Extension, Side::WebUi]),
    (p::AGENT_OFFLINE, &[Side::Extension, Side::WebUi]),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `.js` files under `dir`, tests left out: they quote the words too.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|x| x == "js") && !path.to_string_lossy().ends_with(".test.js") {
            out.push(path);
        }
    }
}

fn side_text(side: Side) -> String {
    let root = repo_root();
    let files = match side {
        Side::Extension => {
            let mut files = Vec::new();
            sources(&root.join("browser-extension/src"), &mut files);
            files
        }
        Side::WebUi => vec![root.join("recog/web/static/app.js")],
    };
    assert!(!files.is_empty(), "no JavaScript for {side:?}");
    files.iter().map(|f| fs::read_to_string(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()))).collect()
}

/// A JavaScript string literal of `word`, in any of the three quotes.
fn has_literal(text: &str, word: &str) -> bool {
    ['\'', '"', '`'].iter().any(|q| text.contains(&format!("{q}{word}{q}")))
}

#[test]
fn shared_protocol_words_when_javascript_is_read_then_each_side_spells_them_as_rust_does() {
    let extension = side_text(Side::Extension);
    let web_ui = side_text(Side::WebUi);
    let missing: Vec<String> = SHARED_WITH_JS
        .iter()
        .flat_map(|(word, sides)| sides.iter().map(move |side| (*word, *side)))
        .filter(|(word, side)| {
            let text = match side {
                Side::Extension => &extension,
                Side::WebUi => &web_ui,
            };
            !has_literal(text, word)
        })
        .map(|(word, side)| format!("{word:?} in {side:?}"))
        .collect();
    assert!(missing.is_empty(), "protocol words the JavaScript no longer spells: {missing:?}");
}
