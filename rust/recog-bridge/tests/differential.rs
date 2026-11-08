//! The Rust bridge against the Python one on the same scenarios.
//!
//! Each scenario under `tests/parity/scenarios/` is a script of client connections
//! (extensions with Origin headers, the webapp subscriber, the supervisor, the
//! Copilot channel), frames, calls from the live side, queries and clock steps. It is
//! run by `tests/parity/bridge_driver.py` on `recog/ktalk_bridge.py` and by
//! `recog-bridge-probe` on this crate, each with a recording live side, a real
//! meeting storage in its own temp dir and the same virtual clock; then everything
//! they did is compared exactly: the text of every frame each client received, the
//! closes, every call into the live side, the query results, the golden taps, the
//! increment runs, the files under the storage (meta.json, transcript.jsonl,
//! index.json, the debug file) and the log lines.
//!
//! The interpreter is `RECOG_BRIDGE_PARITY_PYTHON` (scripts/check.sh passes its
//! own), else the checkout's `venv-gigaam/bin/python`; without one the tests fail.
//!
//! Legitimate differences, normalised before comparing (see [`normalize`]): the
//! kernel's peer ports of real sockets, and the traceback websockets prints under a
//! refused handshake, which the port logs as one line.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use recog_core::pyjson::{self, Map, Value};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn python() -> PathBuf {
    if let Some(p) = std::env::var_os("RECOG_BRIDGE_PARITY_PYTHON").filter(|p| !p.is_empty()) {
        return p.into();
    }
    let venv = repo_root().join("venv-gigaam/bin/python");
    assert!(venv.exists(), "no Python for the parity tests: set RECOG_BRIDGE_PARITY_PYTHON");
    venv
}

/// The environment both sides run in: a fixed zone, temp HOME and settings, no
/// installation configuration, the default microphone name.
fn command(program: &Path, home: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TZ", "Etc/GMT-3")
        .env("LANG", "C.UTF-8")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("RECOG_CONFIG_FILE", home.join("absent-config.toml"))
        .env("RECOG_CONFERENCE_HOSTS_FILE", home.join("absent-hosts.json"))
        .env("RECOG_DATA_DIR", home.join("data"))
        .env("RECOG_EXTRA_WS_ORIGINS", "")
        .env("MIC_SPEAKER_NAME", "Me")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run(mut command: Command, scenario: &str, side: &str) -> Value {
    let mut child = command.spawn().unwrap_or_else(|e| panic!("{side}: {e}"));
    std::io::Write::write_all(child.stdin.as_mut().expect("stdin"), scenario.as_bytes()).expect("write scenario");
    let output = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{side} failed ({}):\n{}\n{stdout}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    pyjson::loads(stdout.trim()).unwrap_or_else(|e| panic!("{side} output: {e}: {stdout}"))
}

fn run_python(scenario: &str) -> Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let (work, home) = (dir.path().join("work"), dir.path().join("home"));
    std::fs::create_dir_all(&work).expect("work");
    std::fs::create_dir_all(&home).expect("home");
    let mut command = command(&python(), &home);
    command
        .arg("-I")
        .arg(repo_root().join("rust/recog-bridge/tests/parity/bridge_driver.py"))
        .arg(repo_root())
        .arg(&work);
    run(command, scenario, "python")
}

fn run_rust(scenario: &str) -> Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let (work, home) = (dir.path().join("work"), dir.path().join("home"));
    std::fs::create_dir_all(&work).expect("work");
    std::fs::create_dir_all(&home).expect("home");
    let mut command = command(Path::new(env!("CARGO_BIN_EXE_recog-bridge-probe")), &home);
    command.arg(&work).arg(&home);
    run(command, scenario, "rust")
}

/// Removes what may legitimately differ:
/// - over real sockets the peer ports are the kernel's: `('127.0.0.1', <port>)`;
/// - websockets logs a refused handshake with a traceback; the port logs one line,
///   `opening handshake failed: <reason>`, so the traceback is folded into it.
fn normalize(out: Value) -> Value {
    let Value::Object(mut map) = out else { return out };
    if let Some(Value::Array(lines)) = map.get("stderr").cloned() {
        let mut folded: Vec<Value> = Vec::new();
        let mut in_traceback = false;
        for line in lines {
            let Value::Str(text) = line else { continue };
            if text == "opening handshake failed" {
                in_traceback = true;
                continue;
            }
            if in_traceback {
                // The last line of a traceback is `module.Error: reason`.
                if text.starts_with("Traceback") || text.starts_with(' ') {
                    continue;
                }
                let reason = text.split_once(": ").map_or(text.as_str(), |(_, r)| r);
                folded.push(Value::from(format!("opening handshake failed: {reason}")));
                in_traceback = false;
                continue;
            }
            folded.push(Value::from(ports(&text)));
        }
        map.insert("stderr", Value::Array(folded));
    }
    Value::Object(map)
}

/// `('127.0.0.1', 54662)` → `('127.0.0.1', <port>)` unless it is a fake socket's
/// fixed 400xx port.
fn ports(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("('127.0.0.1', ") {
        let (head, tail) = rest.split_at(at + "('127.0.0.1', ".len());
        out.push_str(head);
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if digits.len() == 5 && digits.starts_with("400") {
            out.push_str(&digits);
        } else {
            out.push_str("<port>");
        }
        rest = &tail[digits.len()..];
    }
    out.push_str(rest);
    out
}

fn describe(value: &Value) -> String {
    let text = pyjson::dumps(value, None);
    if text.chars().count() > 600 { format!("{}…", text.chars().take(600).collect::<String>()) } else { text }
}

/// The first difference, as a path and both values.
fn first_difference(path: &str, a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in x.iter() {
                match y.get(k) {
                    Some(w) => {
                        if let Some(d) = first_difference(&format!("{path}.{k}"), v, w) {
                            return Some(d);
                        }
                    }
                    None => return Some(format!("{path}.{k}: only in Python: {}", describe(v))),
                }
            }
            y.keys()
                .find(|k| !x.contains_key(k))
                .map(|k| format!("{path}.{k}: only in Rust: {}", describe(y.get(k).expect("key"))))
                .or_else(|| {
                    (x.keys().collect::<Vec<_>>() != y.keys().collect::<Vec<_>>())
                        .then(|| format!("{path}: key order differs"))
                })
        }
        (Value::Array(x), Value::Array(y)) => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                if let Some(d) = first_difference(&format!("{path}[{i}]"), v, w) {
                    return Some(d);
                }
            }
            (x.len() != y.len()).then(|| {
                let extra = if x.len() > y.len() { ("Python", &x[y.len()]) } else { ("Rust", &y[x.len()]) };
                format!("{path}: {} vs {} items; first extra in {}: {}", x.len(), y.len(), extra.0, describe(extra.1))
            })
        }
        (Value::Str(x), Value::Str(y)) if x != y => {
            // Frames and files are JSON text: show where their values differ.
            match (pyjson::loads(x), pyjson::loads(y)) {
                (Ok(px), Ok(py)) if px != py => first_difference(&format!("{path}<json>"), &px, &py)
                    .or_else(|| Some(format!("{path}: same JSON, different text:\n  py: {x}\n  rs: {y}"))),
                (Ok(_), Ok(_)) => Some(format!("{path}: same JSON, different text:\n  py: {x}\n  rs: {y}")),
                _ => Some(format!("{path}:\n  py: {x}\n  rs: {y}")),
            }
        }
        _ if a != b => Some(format!("{path}:\n  py: {}\n  rs: {}", describe(a), describe(b))),
        _ => None,
    }
}

fn check(name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/scenarios").join(format!("{name}.json"));
    let scenario = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let python = normalize(run_python(&scenario));
    let rust = normalize(run_rust(&scenario));
    // RECOG_BRIDGE_PARITY_DUMP=<dir> keeps both outputs for a look.
    if let Some(dir) = std::env::var_os("RECOG_BRIDGE_PARITY_DUMP") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("dump dir");
        std::fs::write(dir.join(format!("{name}.py.json")), pyjson::dumps(&python, Some(1))).expect("dump");
        std::fs::write(dir.join(format!("{name}.rs.json")), pyjson::dumps(&rust, Some(1))).expect("dump");
    }
    if let Some(difference) = first_difference(name, &python, &rust) {
        panic!("the Rust bridge differs from Python in {difference}");
    }
    // The scenario must have exercised something: every scenario has a client.
    let received = python.as_object().and_then(|o| o.get("received")).and_then(Value::as_object).map_or(0, Map::len);
    assert!(received > 0, "{name}: no client received anything");
}

macro_rules! scenarios {
    ($($name:ident => $file:literal,)*) => {
        $(
            #[test]
            fn $name() {
                check($file);
            }
        )*
    };
}

scenarios! {
    given_meeting_lifecycle_when_replayed_then_same_frames_calls_and_files => "meeting_lifecycle",
    given_origins_when_clients_connect_then_same_admissions_and_closes => "origin_gate",
    given_real_websockets_when_origins_and_frames_arrive_then_same_frames_and_closes => "origin_gate_ws",
    given_a_second_conference_tab_when_it_joins_then_same_refusal_and_owner_rules => "second_tab",
    given_participant_lists_when_queried_then_same_microphone_answers => "participants_and_mic",
    given_speaker_updates_when_queried_then_same_history_segments_and_attribution => "speakers",
    given_pcm_chunks_when_dispatched_then_same_chunks_rejections_and_logs => "pcm_chunks",
    given_copilot_channel_when_chatting_then_same_relay_and_stale_rules => "copilot_channel",
    given_transcripts_when_sent_then_same_ticks_and_buffer => "transcript_tick",
    given_supervisor_and_channel_reports_when_merged_then_same_agent_state => "agent_state",
    given_manual_sessions_when_spawned_and_preempted_then_same_relay_and_mirror => "manual_session",
    given_settings_changes_when_refreshed_then_same_policy_frames => "settings_refresh",
    given_profile_changes_when_refreshed_then_same_identity_frames => "profile_refresh",
    given_ktalk_only_when_joining_then_same_capture_switching => "ktalk_only",
    given_stalled_and_failing_clients_when_broadcasting_then_same_timeouts_and_cleanup => "stalled_clients",
    given_odd_frames_and_hello_races_when_read_then_same_handling => "misc_frames",
}
