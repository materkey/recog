//! The channel's pure parts against `recog/copilot_channel.py` on random inputs
//! (`tests/oracle/channel_oracle.py`, run with the repository's interpreter): the push
//! formatting, the literal context block, FastMCP's argument validation of the four
//! tools, pydantic's JSON of a tool result, and the privacy masking before a push.
//! Outputs are compared as Python's `json.dumps` of them, so key order counts.
//!
//! The interpreter is `RECOG_ORACLE_PYTHON` (scripts/check.sh sets it), else
//! `venv-gigaam/bin/python` of the checkout; without either the tests say so and pass.
#![forbid(unsafe_code)]

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::oracle_python;
use recog_copilot::channel::context::{build_copilot_context, format_channel_event};
use recog_copilot::channel::pydantic::{self, validate_arguments};
use recog_copilot::channel::tools::TOOLS;
use recog_core::privacy::{CopilotPrivacy, PrivacyStore};
use recog_core::pyjson::{self, Map, Value};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The Python outputs for `cases`, or `None` without an interpreter.
fn oracle(cases: &[Value]) -> Option<Vec<Value>> {
    let Some(python) = oracle_python() else {
        eprintln!("no Python interpreter for the oracle (set RECOG_ORACLE_PYTHON): skipped");
        return None;
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle/channel_oracle.py");
    let mut child = Command::new(&python)
        .arg(&script)
        .arg(repo_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", python.display()));
    let input = pyjson::dumps(&Value::Array(cases.to_vec()), None);
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(output.status.success(), "the oracle failed:\n{}", String::from_utf8_lossy(&output.stderr));
    let Value::Array(answers) = pyjson::loads(std::str::from_utf8(&output.stdout).unwrap()).unwrap() else {
        panic!("the oracle's answer is not a list")
    };
    assert_eq!(answers.len(), cases.len());
    Some(answers)
}

fn assert_same(kind: &str, cases: &[Value], rust: &[Value], python: &[Value]) {
    for (i, (r, p)) in rust.iter().zip(python).enumerate() {
        let (r, p) = (pyjson::dumps(r, None), pyjson::dumps(p, None));
        assert_eq!(
            r,
            p,
            "{kind} case {i} differs\ninput: {}\nrust:   {r}\npython: {p}",
            pyjson::dumps(&cases[i], None)
        );
    }
    eprintln!("{kind}: {} cases identical", cases.len());
}

/// xorshift64*: reproducible inputs without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<T: Clone>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())].clone()
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

fn object(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

const TEXTS: [&str; 14] = [
    "",
    "hi",
    "что нового?",
    "Имя {{GOAL}}",
    "Alias \"quoted\"",
    "it's",
    "line\nbreak\ttab",
    "$[literal] {user_name}",
    "speaker_0",
    "Alice",
    "😀 é",
    "a\u{2028}b\u{0}c\u{7f}",
    "  7 ",
    "Bob: Second\nAlice: First",
];

const FLOATS: [f64; 11] = [0.0, -0.0, 1.0, 1.5, 0.1, 1e20, 1.5e-7, 1e16, 1730000000123.456, 5e-324, 1e15];

fn scalar(rng: &mut Rng) -> Value {
    match rng.below(9) {
        0 => Value::Null,
        1 => Value::Bool(rng.chance(50)),
        2 => Value::Int(rng.pick(&[0, 1, -1, 2, 180, 300, 301, 9_007_199_254_740_993, 123_456_789_012_345_678_901])),
        3 => Value::Float(rng.pick(&FLOATS)),
        4 => Value::Float(rng.pick(&[f64::NAN, f64::INFINITY, f64::NEG_INFINITY])),
        _ => Value::from(rng.pick(&TEXTS)),
    }
}

fn value(rng: &mut Rng, depth: usize) -> Value {
    if depth == 0 || rng.chance(60) {
        return scalar(rng);
    }
    let n = rng.below(4);
    if rng.chance(50) {
        Value::Array((0..n).map(|_| value(rng, depth - 1)).collect())
    } else {
        let keys = ["a", "speaker", "text", "é", "with space", "x"];
        Value::Object((0..n).map(|_| (rng.pick(&keys).to_owned(), value(rng, depth - 1))).collect())
    }
}

const EVENT_TYPES: [&str; 8] = [
    "copilot_user_message",
    "transcript_tick",
    "meeting_started",
    "meeting_ended",
    "user_profile_updated",
    "copilot_policy_updated",
    "something_else",
    "",
];

const EVENT_KEYS: [&str; 17] = [
    "text",
    "new_segments",
    "recent_context",
    "meeting_title",
    "request_id",
    "meeting_id",
    "local_user_name",
    "local_user_aliases",
    "local_user_profile_revision",
    "local_user_identity_known",
    "copilot_instructions",
    "copilot_policy_revision",
    "privacy_mode",
    "meeting-with-hyphen",
    "created_at",
    "recent_segments",
    "segments",
];

fn segment(rng: &mut Rng) -> Value {
    let mut pairs =
        vec![("speaker", rng.pick(&[Value::from("Alice"), Value::from("Bob"), Value::from(""), Value::Null]))];
    if rng.chance(50) {
        pairs.push(("text", Value::from(rng.pick(&TEXTS))));
    }
    if rng.chance(30) {
        pairs.push(("speaker_id", Value::from("bob@example.test")));
    }
    if rng.chance(15) {
        pairs.push(("event_type", Value::from("session_pause")));
    }
    if rng.chance(40) {
        pairs.push(("meeting_id", rng.pick(&[Value::from("m-1"), Value::from("m-2")])));
    }
    object(pairs)
}

fn event(rng: &mut Rng) -> Value {
    let mut map = Map::new();
    if !rng.chance(5) {
        map.insert("type", Value::from(rng.pick(&EVENT_TYPES)));
    }
    for _ in 0..rng.below(9) {
        let key = rng.pick(&EVENT_KEYS);
        let v = match key {
            "new_segments" | "recent_segments" | "segments" if rng.chance(70) => {
                Value::Array((0..rng.below(4)).map(|_| segment(rng)).collect())
            }
            "local_user_aliases" if rng.chance(70) => {
                Value::Array((0..rng.below(3)).map(|_| Value::from(rng.pick(&TEXTS))).collect())
            }
            _ => value(rng, 2),
        };
        map.insert(key, v);
    }
    Value::Object(map)
}

#[test]
fn random_events_when_formatted_then_push_and_context_match_python() {
    // given
    let mut rng = Rng(0xC4A7_0001);
    let mut cases = Vec::new();
    for _ in 0..600 {
        cases.push(object(vec![("kind", Value::from("format")), ("event", event(&mut rng))]));
        cases.push(object(vec![("kind", Value::from("context")), ("snapshot", event(&mut rng))]));
    }
    // when
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| match case.as_object().unwrap().get("kind").unwrap().as_str().unwrap() {
            "format" => {
                let event = case.as_object().unwrap().get("event").unwrap().as_object().unwrap();
                match format_channel_event(event) {
                    Ok((content, meta)) => {
                        object(vec![("content", Value::from(content)), ("meta", Value::Object(meta))])
                    }
                    Err(error) => object(vec![("error", Value::from(error))]),
                }
            }
            _ => Value::from(build_copilot_context(
                case.as_object().unwrap().get("snapshot").unwrap().as_object().unwrap(),
            )),
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("format/context", &cases, &rust, &python);
    }
}

/// Values a tool argument can arrive as, typed or not.
fn argument(rng: &mut Rng) -> Value {
    let strings = [
        "0",
        "-5",
        "1",
        "1.0",
        "12.000",
        "1.5",
        "abc",
        " 7 ",
        "1_0",
        "1__0",
        "_1",
        "+4",
        "1e3",
        "",
        "null",
        "[1]",
        "{}",
        "{\"a\": 1}",
        "true",
        "false",
        "yes",
        "No",
        "off",
        "T",
        "2",
        "\"x\"",
        "٣",
        "99999999999999999999",
        "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy",
        "éééééééééééééééééééééééééééééééééééé",
        "reply",
    ];
    match rng.below(6) {
        0 => Value::from(rng.pick(&strings)),
        1 => Value::from(rng.pick(&strings)),
        2 => Value::Int(rng.pick(&[0, 1, -1, 2, 5, 60, 300, 400, 123_456_789_012_345_678_901])),
        3 => Value::Float(rng.pick(&[0.0, 1.0, 1.5, 5.0, -0.0, 1e300, 9.3e18, f64::INFINITY, f64::NAN])),
        4 => rng.pick(&[Value::Null, Value::Bool(true), Value::Bool(false)]),
        _ => value(rng, 2),
    }
}

#[test]
fn random_arguments_when_validated_then_values_or_errors_match_fastmcp() {
    // given
    let mut rng = Rng(0xC4A7_0002);
    let cases: Vec<Value> = (0..1500)
        .map(|_| {
            let tool = &TOOLS[rng.below(TOOLS.len())];
            let mut arguments = Map::new();
            for field in tool.fields {
                if rng.chance(75) {
                    arguments.insert(field.name, argument(&mut rng));
                }
            }
            if rng.chance(10) {
                arguments.insert("extra", argument(&mut rng));
            }
            object(vec![
                ("kind", Value::from("validate")),
                ("tool", Value::from(tool.name)),
                ("arguments", Value::Object(arguments)),
            ])
        })
        .collect();
    // when
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| {
            let case = case.as_object().unwrap();
            let name = case.get("tool").unwrap().as_str().unwrap();
            let spec = TOOLS.iter().find(|t| t.name == name).unwrap();
            let arguments = case.get("arguments").unwrap().as_object().unwrap();
            match validate_arguments(&format!("{name}Arguments"), spec.fields, arguments) {
                Ok(values) => object(vec![("value", Value::Object(values))]),
                Err(error) => object(vec![("error", Value::from(error))]),
            }
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("validate", &cases, &rust, &python);
    }
}

#[test]
fn random_results_when_serialised_then_text_matches_pydantic() {
    // given
    let mut rng = Rng(0xC4A7_0003);
    let cases: Vec<Value> =
        (0..800).map(|_| object(vec![("kind", Value::from("to_json")), ("value", value(&mut rng, 4))])).collect();
    // when
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| Value::from(pydantic::to_json(case.as_object().unwrap().get("value").unwrap())))
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("to_json", &cases, &rust, &python);
    }
}

#[test]
fn random_deliveries_when_privacy_on_then_masks_and_pushes_match_python() {
    // given
    let mut rng = Rng(0xC4A7_0004);
    let cases: Vec<Value> = (0..150)
        .map(|_| {
            let payloads: Vec<Value> = (0..1 + rng.below(5))
                .map(|_| {
                    // Well-formed payloads only: on segments that are not dicts, a
                    // `recent_context` that is not a string or a list as `meeting_id`,
                    // Python raises where recog-core skips (its documented difference).
                    let mut payload = event(&mut rng).as_object().unwrap().clone();
                    for key in ["recent_segments", "new_segments", "segments", "recent_context", "meeting_id"] {
                        payload.remove(key);
                    }
                    if rng.chance(60) {
                        payload.insert("meeting_id", rng.pick(&[Value::from("m-1"), Value::from("m-2"), Value::Null]));
                    }
                    if rng.chance(50) {
                        let segments = (0..rng.below(4)).map(|_| segment(&mut rng)).collect();
                        payload
                            .insert(rng.pick(&["recent_segments", "new_segments", "segments"]), Value::Array(segments));
                    }
                    if rng.chance(30) {
                        payload.insert("recent_context", Value::from(rng.pick(&TEXTS)));
                    }
                    if rng.chance(40) {
                        payload.insert("local_user_name", Value::from(rng.pick(&["Alice", "Bob", ""])));
                    }
                    Value::Object(payload)
                })
                .collect();
            object(vec![("kind", Value::from("privacy")), ("payloads", Value::Array(payloads))])
        })
        .collect();
    // when
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| {
            let dir = tempfile::tempdir().unwrap();
            let store = PrivacyStore::new(dir.path());
            store.write(true).unwrap();
            let mut privacy = CopilotPrivacy::new(store);
            let Some(Value::Array(payloads)) = case.as_object().unwrap().get("payloads") else { unreachable!() };
            Value::Array(
                payloads
                    .iter()
                    .map(|payload| {
                        let masked = privacy.apply(payload.as_object().unwrap()).unwrap();
                        let push = match format_channel_event(&masked) {
                            Ok((content, meta)) => {
                                object(vec![("content", Value::from(content)), ("meta", Value::Object(meta))])
                            }
                            Err(error) => object(vec![("error", Value::from(error))]),
                        };
                        object(vec![("masked", Value::Object(masked)), ("push", push)])
                    })
                    .collect(),
            )
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("privacy", &cases, &rust, &python);
    }
}
