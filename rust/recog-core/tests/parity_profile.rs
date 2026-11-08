//! Parity with Python for the user profile and the privacy setting: the same
//! operations run through `recog/user_profile.py` / `recog/privacy.py` and through the
//! port, each in its own directory, and the answers, the files and their modes must
//! be equal. Revisions are random uuids on both sides, so each side's are numbered in
//! the order they appear before comparing.
#![forbid(unsafe_code)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use base64::Engine as _;
use common::{assert_none, difference, from_json, python, run_python, snapshot, to_json};
use recog_core::privacy::{CopilotPrivacy, Privacy, PrivacyError, PrivacySource as _, PrivacyStore};
use recog_core::user_profile::{ProfileError, UserProfileStore, resolve_identity};
use serde_json::{Value as Json, json};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Each uuid-shaped revision replaced by `<rev N>` in order of first appearance.
fn number_revisions(text: &str, seen: &mut BTreeMap<String, usize>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(|c: char| c.is_ascii_hexdigit()) {
        out.push_str(&rest[..at]);
        let candidate = &rest[at..];
        let is_uuid = candidate.len() >= 36
            && candidate.as_bytes()[..36].iter().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) { *b == b'-' } else { b.is_ascii_hexdigit() && !b.is_ascii_uppercase() }
            });
        if is_uuid {
            let n = seen.len();
            let id = *seen.entry(candidate[..36].to_owned()).or_insert(n);
            out.push_str(&format!("<rev {id}>"));
            rest = &candidate[36..];
        } else {
            out.push_str(&candidate[..1]);
            rest = &candidate[1..];
        }
    }
    out.push_str(rest);
    out
}

fn profile_ops() -> Vec<Json> {
    let profile = |body: &str| json!({"op": "write_raw", "base64": b64(body.as_bytes())});
    let mut too_large = br#"{"version": 1, "revision": "r", "name": "", "aliases": [], "pad": ""#.to_vec();
    too_large.extend(std::iter::repeat_n(b'x', 16400));
    too_large.extend(br#""}"#);
    vec![
        json!({"op": "read"}),
        json!({"op": "identity", "legacy": "Me", "override": null}),
        json!({"op": "write", "name": "  Анна  ", "aliases": ["Аня", "аня", "Me", "  ", "Straße", "STRASSE", "ǅ"]}),
        json!({"op": "read"}),
        json!({"op": "identity", "legacy": "Legacy", "override": " Cli "}),
        json!({"op": "identity", "legacy": "Legacy", "override": "аня"}),
        json!({"op": "write", "name": "me", "aliases": []}),
        json!({"op": "write", "name": "ME\u{0}", "aliases": []}),
        json!({"op": "write", "name": "a\nb", "aliases": []}),
        json!({"op": "write", "name": "x".repeat(81), "aliases": []}),
        json!({"op": "write", "name": format!(" {} ", "x".repeat(80)), "aliases": []}),
        json!({"op": "write", "name": "Anna", "aliases": vec!["a"; 17]}),
        json!({"op": "write", "name": "Anna", "aliases": ["x\u{200b}"]}),
        json!({"op": "write", "name": "Anna", "aliases": ["\u{e000}"]}),
        json!({"op": "clear"}),
        json!({"op": "read"}),
        json!({"op": "identity", "legacy": " Old ", "override": null}),
        profile("{"),
        json!({"op": "read"}),
        json!({"op": "write", "name": "Anna", "aliases": []}),
        profile("[]"),
        json!({"op": "read"}),
        profile(r#"{"version": true, "revision": "r", "name": "", "aliases": []}"#),
        json!({"op": "read"}),
        profile(r#"{"version": 1.0, "revision": "r", "name": "", "aliases": []}"#),
        json!({"op": "read"}),
        profile(r#"{"version": 1, "revision": " ", "name": "", "aliases": []}"#),
        json!({"op": "read"}),
        profile(&format!(r#"{{"version": 1, "revision": "{}", "name": "", "aliases": []}}"#, "r".repeat(129))),
        json!({"op": "read"}),
        profile(r#"{"version": 1, "revision": "r\u0000", "name": "", "aliases": []}"#),
        json!({"op": "read"}),
        profile(r#"{"version": 1, "revision": "r", "name": 5, "aliases": []}"#),
        json!({"op": "read"}),
        profile(r#"{"version": 1, "revision": "r", "name": "Me", "aliases": []}"#),
        json!({"op": "read"}),
        profile(r#"{"version": 1, "revision": "r", "name": "", "aliases": ["A"]}"#),
        json!({"op": "read"}),
        profile(r#"{"version": 1, "revision": "r", "name": "B", "aliases": "A"}"#),
        json!({"op": "read"}),
        profile(r#"{"version": 1, "revision": "r1", "name": " Bob ", "aliases": ["bob", "Me", "Rob", "rob"]}"#),
        json!({"op": "read"}),
        json!({"op": "identity", "legacy": "Legacy", "override": null}),
        json!({"op": "write_raw", "base64": b64(b"\xef\xbb\xbf{\"version\": 1, \"revision\": \"bom\", \"name\": \"\", \"aliases\": []}")}),
        json!({"op": "read"}),
        json!({"op": "write_raw", "base64": b64(b"\xff")}),
        json!({"op": "read"}),
        json!({"op": "write_raw", "base64": b64(&too_large)}),
        json!({"op": "read"}),
        profile(r#"{"version": 1, "revision": "r2", "name": "Ann", "aliases": []}"#),
        json!({"op": "write", "name": "Ann\u{2003}", "aliases": ["Анна", "ann"]}),
    ]
}

fn profile_error(e: &ProfileError) -> Json {
    match e {
        ProfileError::Invalid(m) => json!({"error": ["Value", m]}),
        ProfileError::Io(m) => json!({"error": ["Os", m]}),
    }
}

fn run_profile_ops(data_dir: &Path, ops: &[Json]) -> Vec<Json> {
    let store = UserProfileStore::new(data_dir);
    ops.iter()
        .map(|op| {
            let strings = |v: &Json| -> Vec<String> {
                v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect()
            };
            let result = match op["op"].as_str().unwrap() {
                "read" => store.read().map(|p| to_json(&p.to_json())),
                "write" => {
                    store.write(op["name"].as_str().unwrap(), &strings(&op["aliases"])).map(|p| to_json(&p.to_json()))
                }
                "clear" => store.clear().map(|p| to_json(&p.to_json())),
                "write_raw" => {
                    fs::create_dir_all(data_dir).unwrap();
                    let bytes =
                        base64::engine::general_purpose::STANDARD.decode(op["base64"].as_str().unwrap()).unwrap();
                    fs::write(&store.path, bytes).unwrap();
                    Ok(Json::Null)
                }
                "identity" => {
                    store.read().map(|p| to_json(&resolve_identity(&p, op["legacy"].as_str(), op["override"].as_str())))
                }
                other => panic!("unknown op {other}"),
            };
            result.unwrap_or_else(|e| profile_error(&e))
        })
        .collect()
}

#[test]
fn user_profile_operations_answer_and_write_the_same_files_as_python() {
    // given
    let Some(python) = python() else { return };
    let root = tempfile::tempdir().unwrap();
    let (py_side, rs_side) = (root.path().join("python"), root.path().join("rust"));
    fs::create_dir_all(&py_side).unwrap();
    fs::create_dir_all(&rs_side).unwrap();
    let ops = profile_ops();
    // when
    let answer = run_python(
        &python,
        "user_profile",
        &json!({"data_dir": py_side.join("a/b/data"), "ops": ops}),
        &root.path().join("sandbox"),
        "UTC",
    );
    let got = run_profile_ops(&rs_side.join("a/b/data"), &ops);
    // then
    let (mut py_seen, mut rs_seen) = (BTreeMap::new(), BTreeMap::new());
    let mut failures = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let expected: Json =
            serde_json::from_str(&number_revisions(&answer["results"][i].to_string(), &mut py_seen)).unwrap();
        let actual: Json = serde_json::from_str(&number_revisions(&got[i].to_string(), &mut rs_seen)).unwrap();
        failures.extend(difference(&format!("op {i} {op}"), &expected, &actual));
    }
    let files = |side: &Path, seen: &mut BTreeMap<String, usize>| -> Json {
        snapshot(side).into_iter().map(|(k, (mode, text))| (k, json!([mode, number_revisions(&text, seen)]))).collect()
    };
    let py_files = files(&py_side, &mut py_seen);
    failures.extend(difference("files", &py_files, &files(&rs_side, &mut rs_seen)));
    assert_none(&failures);
    // The fixtures reach Python's refusals and its saved file, not only defaults.
    let errors = answer["results"].as_array().unwrap().iter().filter(|r| r.get("error").is_some()).count();
    assert!(errors >= 20, "only {errors} operations failed in Python");
    assert_eq!(py_files["a/b/data/user-profile.json"][0], json!(0o600));
}

fn privacy_ops() -> Vec<Json> {
    let payload = json!({
        "meeting_id": "m1",
        "recent_segments": [
            {"speaker": "Anna", "speaker_id": "p1", "text": "hi", "meeting_id": "m1"},
            {"speaker": "Bob", "text": null},
            {"speaker": "", "text": "x"},
            {"speaker": 7, "text": 5},
            {"speaker": "Система", "event_type": "session_pause", "text": "p"},
            {"speaker": "Anna", "event_type": "", "text": "again"},
        ],
        "new_segments": [{"speaker": "Carl", "text": "c"}],
        "recent_context": "stale",
        "local_user_name": "Anna",
        "local_user_aliases": ["Anna"],
        "local_user_identity_known": true,
        "local_user_profile_revision": "r1",
    });
    let mut ops = vec![
        json!({"op": "read"}),
        json!({"op": "apply", "payload": payload}),
        json!({"op": "write", "enabled": true}),
        json!({"op": "read"}),
        json!({"op": "apply", "payload": payload}),
        json!({"op": "apply", "payload": {"segments": [{"speaker": "Bob", "meeting_id": "m2"}, {"speaker": "Anna"}]}}),
        json!({"op": "apply", "payload": {"segments": [{"speaker": "Dan"}], "recent_context": "Dan: hello\r\nno separator\u{2028}Anna: : x\n"}}),
        json!({"op": "apply", "payload": {"local_user_name": null, "local_user_profile_revision": 5}}),
        json!({"op": "apply", "payload": {"meeting_id": null, "local_user_name": "Eve", "local_user_profile_revision": null}}),
        json!({"op": "apply", "payload": {"meeting_id": 1, "new_segments": [{"speaker": "One"}]}}),
        json!({"op": "apply", "payload": {"meeting_id": 1.0, "new_segments": [{"speaker": "Two"}]}}),
    ];
    for i in 0..17 {
        ops.push(
            json!({"op": "apply", "payload": {"meeting_id": format!("lru{i}"), "new_segments": [{"speaker": "Zed"}]}}),
        );
    }
    ops.extend([
        json!({"op": "apply", "payload": {"meeting_id": "m1", "new_segments": [{"speaker": "Bob"}, {"speaker": "Anna"}]}}),
        json!({"op": "apply", "payload": {"meeting_id": "lru16", "new_segments": [{"speaker": "Yan"}, {"speaker": "Zed"}]}}),
        json!({"op": "write_raw", "text": "[]"}),
        json!({"op": "apply", "payload": payload}),
        json!({"op": "write", "enabled": false}),
        json!({"op": "write_raw", "text": "{\"enabled\": 1}"}),
        json!({"op": "read"}),
        json!({"op": "write_raw", "text": "{\"enabled\": true"}),
        json!({"op": "read"}),
        json!({"op": "write_raw", "text": "{\"enabled\": false} "}),
        json!({"op": "read"}),
        json!({"op": "write", "enabled": true}),
    ]);
    ops
}

fn run_privacy_ops(data_dir: &Path, ops: &[Json]) -> Vec<Json> {
    let store = PrivacyStore::new(data_dir);
    let mut privacy = CopilotPrivacy::new(PrivacyStore::new(data_dir));
    let privacy_json = |p: Privacy| json!({"enabled": p.enabled, "configured": p.configured});
    let privacy_error = |e: PrivacyError| match e {
        PrivacyError::Invalid(m) if m.starts_with("Invalid privacy") => json!({"error": ["Value", m]}),
        PrivacyError::Invalid(m) => json!({"error": ["Json", m]}),
        PrivacyError::Io(m) => json!({"error": ["Os", m]}),
    };
    ops.iter()
        .map(|op| match op["op"].as_str().unwrap() {
            "read" => store.read().map_or_else(privacy_error, privacy_json),
            "write" => store.write(op["enabled"].as_bool().unwrap()).map_or_else(privacy_error, privacy_json),
            "write_raw" => {
                fs::create_dir_all(data_dir).unwrap();
                fs::write(&store.path, op["text"].as_str().unwrap()).unwrap();
                Json::Null
            }
            "apply" => {
                let payload = from_json(&op["payload"]);
                privacy
                    .apply(payload.as_object().unwrap())
                    .map_or_else(privacy_error, |m| to_json(&recog_core::pyjson::Value::Object(m)))
            }
            other => panic!("unknown op {other}"),
        })
        .collect()
}

#[test]
fn privacy_setting_and_masking_answer_and_write_the_same_as_python() {
    // given
    let Some(python) = python() else { return };
    let root = tempfile::tempdir().unwrap();
    let (py_side, rs_side) = (root.path().join("python"), root.path().join("rust"));
    let ops = privacy_ops();
    // when
    let answer = run_python(
        &python,
        "privacy",
        &json!({"data_dir": py_side.join("data"), "ops": ops}),
        &root.path().join("sandbox"),
        "UTC",
    );
    let got = run_privacy_ops(&rs_side.join("data"), &ops);
    // then
    let mut failures = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        failures.extend(difference(&format!("op {i} {op}"), &answer["results"][i], &got[i]));
    }
    let files = |side: &Path| -> Json { snapshot(side).into_iter().map(|(k, v)| (k, json!(v))).collect() };
    let py_files = files(&py_side);
    failures.extend(difference("files", &py_files, &files(&rs_side)));
    assert_none(&failures);
    assert_eq!(py_files["data/privacy.json"], json!([0o600, "{\"enabled\": true}"]));
    let masked = answer["results"].as_array().unwrap().iter().filter(|r| r.get("privacy_mode").is_some()).count();
    assert!(masked >= 20, "only {masked} payloads were masked in Python");
}
