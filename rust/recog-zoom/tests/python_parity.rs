//! Differential test: the same input to `recog/zoom_app.py` and to this crate, the
//! answers compared exactly.
//!
//! Inputs: the scenarios of `tests/test_zoom_app.py` as event lists, snapshot
//! sequences rebuilt from recorded Accessibility dumps (`fixtures/recorded.json`), and
//! seeded random sequences that walk the edges (the 6 s and 30 s windows, namesakes,
//! names with commas, unknown wording, lines Python fails on). Each event's frames
//! (as the JSON text sent to the bridge), log lines and session state must be equal.
//! The vocabulary of the installed Zoom, a synthetic Zoom with awkward strings files,
//! the pure functions and the helper's `Info.plist` bytes are compared too.
//!
//! The interpreter is `RECOG_ZOOM_PARITY_PYTHON` (`scripts/check.sh` passes its own),
//! else the checkout's `venv-gigaam`, else `python3`; it must import `recog.zoom_app`.
#![forbid(unsafe_code)]

mod common;
mod parity;

use std::path::PathBuf;
use std::process::{Command, Stdio};

use common::*;
use parity::{Scenario, random_calls, random_scenario, rust_calls, rust_session};
use recog_zoom::log::Logger;
use recog_zoom::vocabulary::{Vocabulary, default_app_paths, load_vocabulary};
use serde_json::{Value, json};

fn repo_root() -> PathBuf {
    recog_zoom::run::default_repo_root()
}

fn python() -> PathBuf {
    if let Some(configured) = std::env::var_os("RECOG_ZOOM_PARITY_PYTHON").filter(|value| !value.is_empty()) {
        return configured.into();
    }
    let venv = repo_root().join("venv-gigaam/bin/python");
    if venv.exists() { venv } else { PathBuf::from("python3") }
}

/// Ask the Python driver; panics with its stderr when it fails.
fn ask_python(mut request: Value) -> Value {
    request["repo_root"] = json!(repo_root());
    let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/parity/zoom_app_driver.py");
    let mut child = Command::new(python())
        .arg("-I")
        .arg(driver)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("cannot start {}: {error}", python().display()));
    let input = serde_json::to_vec(&request).unwrap();
    let writer = {
        let mut stdin = child.stdin.take().unwrap();
        std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &input))
    };
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(
        output.status.success(),
        "the Python driver failed ({}): {}",
        python().display(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn compare_sessions(scenarios: &[Scenario]) -> usize {
    let request = json!({"kind": "sessions", "scenarios": scenarios.iter().map(Scenario::to_json).collect::<Vec<_>>()});
    let python = ask_python(request);
    let python = python.as_array().unwrap();
    assert_eq!(python.len(), scenarios.len());
    let (mut events, mut frames, mut logged, mut failed) = (0, 0, 0, 0);
    for (scenario, expected) in scenarios.iter().zip(python) {
        let actual = rust_session(scenario);
        let expected = expected.as_array().unwrap();
        for (index, (mine, theirs)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(
                mine, theirs,
                "scenario {:?}, event {index}: {}\nrust:   {mine}\npython: {theirs}",
                scenario.name, scenario.events[index]
            );
        }
        assert_eq!(actual.len(), expected.len(), "scenario {:?}: different event counts", scenario.name);
        events += actual.len();
        frames += actual.iter().map(|event| event["sent"].as_array().unwrap().len()).sum::<usize>();
        logged += actual.iter().map(|event| event["log"].as_array().unwrap().len()).sum::<usize>();
        failed += actual.iter().filter(|event| event["error"] == true).count();
    }
    eprintln!(
        "{} scenarios, {events} events: {frames} frames, {logged} log lines, {failed} failed lines",
        scenarios.len()
    );
    events
}

#[test]
fn given_the_python_test_scenarios_when_both_sessions_run_them_then_every_event_is_the_same() {
    let events = compare_sessions(&parity::ported_scenarios());
    assert!(events > 120, "{events}");
}

#[test]
fn given_recorded_meetings_when_both_sessions_run_them_then_every_event_is_the_same() {
    let text = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/recorded.json"))
        .unwrap();
    let scenarios: Vec<Scenario> =
        serde_json::from_str::<Vec<Value>>(&text).unwrap().iter().map(Scenario::from_json).collect();
    assert_eq!(scenarios.len(), 3);
    compare_sessions(&scenarios);
}

#[test]
fn given_seeded_random_sequences_when_both_sessions_run_them_then_every_event_is_the_same() {
    let scenarios: Vec<Scenario> = (0..400).map(random_scenario).collect();
    let events = compare_sessions(&scenarios);
    assert!(events > 12_000, "{events}");
}

#[test]
fn given_random_arguments_when_both_call_the_pure_functions_then_the_answers_are_the_same() {
    let calls = random_calls(4_000);
    let python = ask_python(json!({"kind": "calls", "calls": calls}));
    let rust = rust_calls(&calls);
    for (index, (mine, theirs)) in rust.iter().zip(python.as_array().unwrap()).enumerate() {
        assert_eq!(mine, theirs, "call {}", calls[index]);
    }
}

fn sorted(vocabulary: &Vocabulary) -> Value {
    json!({
        "muted": vocabulary.muted,
        "unmuted": vocabulary.unmuted,
        "mic_open_item": vocabulary.mic_open_item,
        "mic_closed_item": vocabulary.mic_closed_item,
    })
}

#[test]
fn given_the_installed_zoom_when_both_load_its_wording_then_the_vocabularies_are_equal() {
    let python = ask_python(json!({"kind": "vocabulary", "apps": null}));
    let rust = load_vocabulary(&default_app_paths());

    assert_eq!(sorted(&rust), python);
    if default_app_paths()[0].exists() {
        // Zoom Workplace ships 17 languages: far more wording than English alone.
        assert!(rust.muted.len() > Vocabulary::english().muted.len() + 20, "{:?}", rust.muted);
    }
}

#[test]
fn given_awkward_strings_files_when_both_load_them_then_the_vocabularies_are_equal() {
    let root = tempfile::tempdir().unwrap();
    let app = zoom_app(root.path(), &[("ru", RUSSIAN)]);
    let resources = app.join("Contents/Resources");
    let write = |language: &str, bytes: Vec<u8>| {
        let folder = resources.join(format!("{language}.lproj"));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Localizable.strings"), bytes).unwrap();
    };
    let le = |text: &str| text.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
    let mut be = vec![0xfe, 0xff];
    be.extend("\"LN_Audio_Muted_Accessibility_7\"=\"ΣΊΓΑΣ\";".encode_utf16().flat_map(u16::to_be_bytes));
    write("el", be);
    // Without a byte-order mark: Python's text stream refuses it.
    write(
        "de",
        le("\"LN_Plist_Accessibility_VoIP_Muted_1\" \t=\r\n \"  Ton AUS\u{1f} \";\r\"Mute Audio\" = \"Stumm\";"),
    );
    write(
        "sv",
        utf16(
            "\"LN_Plist_Accessibility_VoIP_Unmuted_3\" \t=\r\n \"\u{1c} Ljud PÅ\u{1f}\u{a0}\";\r\"Mute Telephone\"=\"\u{3000}Tyst\";",
        ),
    );
    write(
        "es",
        utf16("\"LN_Plist_Accessibility_Tele_Unmuted_٣\" = \"Teléfono \\\"activado\\\"\";\n\"Join Audio\"=\"Unir\" ;"),
    );
    write(
        "it",
        utf16(
            "\"LN_Plist_Accessibility_VoIP_Muted_\" = \"no digits\";\n\"LN_Plist_Accessibility_No_Audio_5\" = \"İ SESSİZ\";",
        ),
    );
    write("odd", vec![0xff, 0xfe, 0x41]);
    write("surrogate", vec![0xff, 0xfe, 0x00, 0xd8, 0x41, 0x00]);
    write("", utf16("\"Unmute Telephone\" = \"Hidden\";"));
    std::fs::create_dir_all(resources.join("dir.lproj/Localizable.strings")).unwrap();
    std::fs::write(resources.join("file.lproj"), "not a folder").unwrap();
    let apps = [app, root.path().join("absent.app")];

    let python = ask_python(json!({"kind": "vocabulary", "apps": apps}));
    let rust = load_vocabulary(&apps);

    assert_eq!(sorted(&rust), python);
    assert!(rust.muted.contains("σίγας"), "{:?}", rust.muted);
    assert!(rust.unmuted.contains("ljud på") && rust.mic_open_item.contains("tyst"), "{rust:?}");
    assert!(!rust.muted.contains("ton aus"), "{rust:?}");
}

#[test]
fn given_the_helper_info_when_both_write_the_plist_then_the_bytes_are_equal() {
    let python = ask_python(json!({"kind": "plist"}));
    let bytes = recog_zoom::helper_app::info_plist(&recog_zoom::helper_app::helper_info());
    let expected = String::from_utf8(base64_decode(python.as_str().unwrap())).unwrap();

    assert_eq!(bytes, expected);
}

fn base64_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for byte in text.bytes().filter(|&byte| byte != b'=') {
        bits = (bits << 6) | ALPHABET.iter().position(|&c| c == byte).unwrap() as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    out
}

#[test]
fn given_the_status_log_when_both_log_the_same_events_then_the_lines_are_equal() {
    // Covered event by event above; this pins one unread sample with non-ASCII text
    // and a float, which go through Python's json.dumps formatting.
    let (logger, buffer) = Logger::buffer();
    let mut log = recog_zoom::log::StatusLog::new(logger);
    log.note(Some(&json!("meeting"))).unwrap();
    log.note_unread(&[json!({"description": "Иван, Video on", "x": 1.5e16, "children": []})]);
    let line = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(line.contains("e.g. {\"description\": \"Иван, Video on\", \"x\": 1.5e+16, \"children\": []}"), "{line}");
}
