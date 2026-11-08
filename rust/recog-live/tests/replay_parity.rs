//! The live process against Python by golden replay (docs/plans/2026-11-07-golden-sessions-design.md).
//!
//! Each synthetic session (tests/parity/golden_sessions.py: a microphone alone, a KTalk
//! call on per-speaker PCM with refine, system audio with nobody else in the call, a
//! silence split into a second orphan meeting, the PCM watchdog's fallback and
//! recovery) is recorded with the Python recorder, replayed into the Python live
//! process with a synthetic model answering every ASR call (those answers join the
//! session), and replayed into this binary, which answers from the session by input
//! key. `compare_replays` then sets the two outputs against each other: speech rows
//! (speaker, local or not, text, refined, uncertain, within 1.5 s), markers, and the
//! comparable fields of every meeting's meta.json.
//!
//! Both replays run in real time, the sessions side by side: about a minute in all.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value as Json;

const SESSIONS: [&str; 5] = ["mic_only", "ktalk_pcm", "system_no_remote", "silence_split", "watchdog"];

fn sandboxed(command: &mut Command, sandbox: &Path) {
    std::fs::create_dir_all(sandbox.join("home")).unwrap();
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", sandbox.join("home"))
        .env("RECOG_CONFIG_FILE", sandbox.join("no-config.toml"))
        .env("RECOG_CONFERENCE_HOSTS_FILE", sandbox.join("no-hosts.json"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONUTF8", "1")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

fn python_step(python: &Path, sandbox: &Path, args: &[&str]) -> Json {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/golden_sessions.py");
    let mut command = Command::new(python);
    command.arg(script).arg(common::repo_root()).args(args);
    sandboxed(&mut command, sandbox);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "golden_sessions.py {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().rev().find(|l| l.starts_with('{')).unwrap_or("{}");
    serde_json::from_str(last).unwrap()
}

fn rust_replay(sandbox: &Path, session: &Path, out: &Path) -> Json {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recog"));
    command.arg("replay").arg(session).arg(out).args(["--tail", "3"]);
    sandboxed(&mut command, sandbox);
    let output = command.output().unwrap();
    assert!(output.status.success(), "recog replay failed:\n{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_str(&std::fs::read_to_string(out.join("replay.json")).unwrap()).unwrap()
}

struct Outcome {
    name: &'static str,
    compare: Json,
    rust_misses: i64,
    work: PathBuf,
}

fn run_session(python: &Path, root: &Path, name: &'static str) -> Outcome {
    let work = root.join(name);
    let session = work.join("session");
    python_step(python, &work, &["make", name, session.to_str().unwrap()]);
    python_step(python, &work, &["fill", session.to_str().unwrap(), work.join("py").to_str().unwrap(), "3"]);
    let summary = rust_replay(&work, &session, &work.join("rust"));
    let compare = python_step(
        python,
        &work,
        &["compare", session.to_str().unwrap(), work.join("py").to_str().unwrap(), work.join("rust").to_str().unwrap()],
    );
    Outcome { name, compare, rust_misses: summary["asr_misses"].as_i64().unwrap(), work }
}

fn rows(work: &Path, side: &str) -> Vec<Json> {
    let mut metas: Vec<(String, PathBuf)> = std::fs::read_dir(work.join(side).join("data/meetings"))
        .unwrap()
        .filter_map(|e| {
            let dir = e.unwrap().path();
            let meta: Json = serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).ok()?).ok()?;
            Some((meta["created_at"].as_str()?.to_owned(), dir))
        })
        .collect();
    metas.sort();
    metas
        .iter()
        .flat_map(|(_, dir)| {
            std::fs::read_to_string(dir.join("transcript.jsonl"))
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str::<Json>(l).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn count(compare: &Json, key: &str) -> usize {
    compare[key].as_array().map_or(0, Vec::len)
}

#[test]
fn synthetic_sessions_replayed_into_python_and_rust_then_rows_markers_and_meta_agree() {
    let Some(python) = common::python() else { return };
    let root = tempfile::tempdir().unwrap();
    let outcomes: Vec<Outcome> = std::thread::scope(|scope| {
        let handles: Vec<_> =
            SESSIONS.iter().map(|name| scope.spawn(|| run_session(&python, root.path(), name))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for o in &outcomes {
        let c = &o.compare;
        eprintln!(
            "{}: {} rows matched, {} only Python, {} only Rust; {} markers matched, {} only Python, {} only Rust; \
             meta equal {}; ASR misses Python {} Rust {}",
            o.name,
            c["matched"],
            count(c, "only_first"),
            count(c, "only_second"),
            c["markers_matched"],
            count(c, "markers_only_first"),
            count(c, "markers_only_second"),
            c["meta_equal"],
            c["asr_misses"][0],
            o.rust_misses,
        );
    }
    for o in &outcomes {
        let c = &o.compare;
        assert_eq!(o.rust_misses, 0, "{}: Rust asked ASR for audio Python never did", o.name);
        assert_eq!(count(c, "only_first") + count(c, "only_second"), 0, "{}: rows differ: {c}", o.name);
        assert_eq!(count(c, "markers_only_first") + count(c, "markers_only_second"), 0, "{}: {c}", o.name);
        assert_eq!(c["meta_equal"], Json::Bool(true), "{}: meta.json differs: {c}", o.name);
        assert!(c["matched"].as_u64().unwrap() > 0, "{}: no rows at all", o.name);
    }
    // The scenarios did what they are for, on the Rust side as on the Python one.
    let by_name = |name: &str| outcomes.iter().find(|o| o.name == name).unwrap();
    for side in ["py", "rust"] {
        let ktalk = rows(&by_name("ktalk_pcm").work, side);
        assert!(ktalk.iter().any(|r| r["refined"] == Json::Bool(true)), "{side}: no refined row");
        assert!(ktalk.iter().any(|r| r["speaker"] == "Bob" && r["speaker_id"] == "bob-0002"), "{side}: no PCM row");
        assert!(ktalk.iter().any(|r| r["speaker"] == "Me" && r["is_local"] == Json::Bool(true)), "{side}");
        let system = rows(&by_name("system_no_remote").work, side);
        assert!(system.iter().any(|r| r["speaker"] == "Системный звук"), "{side}: {system:?}");
        let watchdog = rows(&by_name("watchdog").work, side);
        let reasons: Vec<&str> = watchdog.iter().filter_map(|r| r["uncertain_reason"].as_str()).collect();
        assert_eq!(reasons.iter().filter(|r| **r == "pcm_watchdog_transition").count(), 2, "{side}: {reasons:?}");
        assert!(reasons.iter().any(|r| r.starts_with("fallback_mixed")), "{side}: {reasons:?}");
        let split = by_name("silence_split").work.join(side).join("data/meetings");
        let meetings = std::fs::read_dir(split).unwrap().filter(|e| e.as_ref().unwrap().path().is_dir()).count();
        assert_eq!(meetings, 2, "{side}: the silence split made no second meeting");
        let mic = rows(&by_name("mic_only").work, side);
        let texts: Vec<&str> = mic.iter().filter_map(|r| r["text"].as_str()).collect();
        assert_eq!(texts.iter().filter(|t| t.starts_with("привет коллеги")).count(), 2, "{side}: {texts:?}");
    }
}
