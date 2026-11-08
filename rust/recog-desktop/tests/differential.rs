//! Differential tests: `recog/desktop/` (tests/parity/desktop_driver.py) and this
//! crate on the same input, the answers compared exactly after masking clock
//! readings and tokens and naming each side's own directory and port alike.
//!
//! - the functions one by one, on fixed and seeded random inputs: values, error
//!   classes and messages;
//! - the worker on scripted processes and a fake inspector: result, files, the
//!   signals sent and every frame the inspector received;
//! - the observer over a timeline of ticks with real worker children (the Python
//!   worker through `python -m recog.desktop.worker`, the Rust one through this test
//!   binary): every file, `status()`, logs and notifications after each step;
//! - the CLI binaries on temporary data directories: output, exit status, files;
//! - the real process table against psutil on a process this test starts.
//!
//! The interpreter is `RECOG_DESKTOP_PARITY_PYTHON` (`scripts/check.sh` passes its
//! own), else the checkout's `venv-gigaam`; without either the tests are skipped.
#![forbid(unsafe_code)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use recog_core::pyjson::{Map, Value};
use recog_desktop::host::Host;
use recog_desktop::processes::{self, Target};
use recog_desktop::py::{self, PyErr};
use recog_desktop::service::{
    self, BoxFuture, Context, DesktopIntegration, Log, Notifier, NotifyOutcome, WorkerCommand,
};
use recog_desktop::{cli, storage, worker};

/// A seeded xorshift, so a failing input can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn string(&mut self, alphabet: &[&str], max: usize) -> String {
        (0..self.below(max + 1)).map(|_| alphabet[self.below(alphabet.len())]).collect()
    }
}

fn s(text: &str) -> Value {
    Value::from(text)
}

fn call(function: &str, args: Vec<Value>) -> Value {
    py::obj([("fn", s(function)), ("args", Value::Array(args))])
}

fn path_value(path: &Path) -> Value {
    s(&path.to_string_lossy())
}

/// Each side's directory, canonical (macOS temp paths are reached through /private).
fn side_dir(base: &Path, name: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

// --- functions -------------------------------------------------------------------

/// What this crate answers for one driver call (see `functions` in the driver).
fn rust_call(host: &Host, item: &Value) -> Value {
    let name = text(item, "fn").unwrap();
    let Value::Array(args) = field(item, "args") else { panic!("args") };
    let arg = |i: usize| args[i].clone();
    let path = |i: usize| PathBuf::from(args[i].py_str());
    let path_result = |r: Result<PathBuf, PyErr>| outcome(r, |p| path_value(&p));
    let bool_result = |r: Result<bool, PyErr>| outcome(r, Value::Bool);
    let target = |i: usize| Target::from_value(&args[i]).unwrap();
    match name.as_str() {
        "shlex" => outcome(recog_desktop::shlex::split(&arg(0).py_str()), |w| {
            Value::Array(w.into_iter().map(Value::from).collect())
        }),
        "inspector_url" => match worker::inspector_url(host, &arg(0)) {
            Ok(url) => py::obj([("value", s(&url))]),
            Err(recog_desktop::inspector::Failure::Attach(reason)) => {
                py::obj([("error", py::obj([("kind", s("AttachError")), ("message", s(&reason))]))])
            }
            Err(recog_desktop::inspector::Failure::Other(described)) => {
                let (kind, message) = described.split_once(": ").unwrap();
                py::obj([("error", py::obj([("kind", s(kind)), ("message", s(message))]))])
            }
            Err(recog_desktop::inspector::Failure::Timeout) => panic!("timeout"),
        },
        "status" => {
            let Value::Float(now) = arg(1) else { panic!("now") };
            py::obj([("value", service::status_at(&path(0), now))])
        }
        "application_path" => path_result(processes::application_path(host, &arg(0).py_str())),
        "is_ktalk_executable" => bool_result(processes::is_ktalk_executable(host, &path(0))),
        "inspect_fuses" => bool_result(processes::inspect_fuses(&path(0))),
        "find_targets" => outcome(processes::find_targets(host, &path(0)), |targets| {
            Value::Array(targets.iter().map(Target::to_value).collect())
        }),
        "ready" => bool_result(processes::ready(host, &target(0))),
        "same_process" => {
            let app = args.get(1).filter(|a| a.truthy()).map(|a| PathBuf::from(a.py_str()));
            bool_result(processes::same_process(host, &target(0), app.as_deref()))
        }
        "owns_inspector" => bool_result(processes::owns_inspector(host, &target(0))),
        "inspector_supported" => {
            host.fuse_cache.lock().unwrap().clear();
            bool_result(processes::inspector_supported(host, &target(0)))
        }
        "target_from_dict" => outcome(Target::from_value(&arg(0)), |t| t.to_value()),
        "read_json" => outcome(storage::read(&path(0)), |v| v),
        "write_status" => outcome(storage::write_status(&path(0), &arg(1)), |v| v),
        "bundle_digest" => outcome(storage::bundle_digest(&path(0)), |d| s(&d)),
        "prepare_bundle" => outcome(storage::prepare_bundle(&path(0), &path(1)), |(digest, at)| {
            Value::Array(vec![s(&digest), path_value(&at)])
        }),
        "validate_bundle" => path_result(storage::validate_bundle(&path(0), &arg(1))),
        other => panic!("{other}"),
    }
}

fn compare(label: &str, calls: &[Value], python: &Value, rust: &[Value], replacements: &[(String, String)]) -> usize {
    let Value::Array(python) = python else { panic!("driver answered {python:?}") };
    assert_eq!(python.len(), rust.len(), "{label}: answer count");
    let mut failures = Vec::new();
    for ((item, theirs), mine) in calls.iter().zip(python).zip(rust) {
        let (theirs, mine) = (localize(theirs, replacements), localize(mine, replacements));
        if py::dumps(&theirs) != py::dumps(&mine) {
            failures.push(format!(
                "{}\n  python: {}\n  rust:   {}",
                py::dumps(item),
                py::dumps(&theirs),
                py::dumps(&mine)
            ));
        }
    }
    assert!(failures.is_empty(), "{label}: {} of {} differ:\n{}", failures.len(), calls.len(), failures.join("\n"));
    calls.len()
}

fn url_inputs(rng: &mut Rng) -> Vec<Value> {
    let mut urls: Vec<String> = [
        "ws://127.0.0.1:9229/x",
        "ws://127.0.0.1:9229",
        "WS://127.0.0.1:9229/x",
        "ws://127.0.0.1:09229/x",
        "ws://127.0.0.1:99999/x",
        "ws://127.0.0.1:92a9/x",
        "ws://127.0.0.1:/x",
        "ws://@127.0.0.1:9229/x",
        "ws://u:@127.0.0.1:9229/x",
        "ws://[::1]:9229/x",
        "ws://[127.0.0.1]:9229/x",
        "ws://[abc]:9229/x",
        "ws://[v1.x]:9229/x",
        "ws://[v1.]:9229/x",
        "ws://x[::1]:9229/x",
        "ws://[::1]x:9229/x",
        "ws://[::1:9229/x",
        "ws://::1]:9229/x",
        "ws://a[b]@127.0.0.1:9229/x",
        "  \tws://127.0.0.1:9229/x",
        "ws://127.0.0.1:9229/x\n",
        "ws://127.0.0.1:9229/x?",
        "ws://127.0.0.1:9229/x#",
        "ws://127.0.0.1:9229/x?a#b",
        "ws:127.0.0.1:9229/x",
        "1ws://127.0.0.1:9229/x",
        "ws://LOCALHOST:9229/x",
        "",
    ]
    .iter()
    .map(|u| (*u).to_owned())
    .collect();
    let pieces =
        ["ws", "://", "127.0.0.1", ":9229", "/x", "@", ":", "[", "]", "?q", "#f", "::1", "v1.a", "u", "%25", " ", "\t"];
    for _ in 0..300 {
        urls.push(rng.string(&pieces, 7));
    }
    let mut out = vec![obj("{}"), obj("[]"), obj("[{}, {}]"), obj("[{}]")];
    out.extend(urls.into_iter().map(|u| Value::Array(vec![py::obj([("webSocketDebuggerUrl", s(&u))])])));
    out
}

fn shlex_inputs(rng: &mut Rng) -> Vec<String> {
    let mut inputs: Vec<String> = [
        "--max-old-space-size=512 --inspect-port=9333",
        r#"a"b c"d 'e\f' "g\"h\i" \j '' """#,
        "\"unterminated",
        "trailing\\",
        "  \t\r\n ",
        "a\\ b",
        r#""a\\b" "a\$b""#,
        "'it''s'",
        "x=\"1 2\"y",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let alphabet = ["a", "b", " ", "\t", "\n", "\"", "'", "\\", "=", "-", "$", "#", "é"];
    for _ in 0..400 {
        inputs.push(rng.string(&alphabet, 12));
    }
    inputs
}

/// Status directories: every combination of a few configuration, status and
/// heartbeat files, each in its own directory.
fn status_roots(base: &Path) -> Vec<(PathBuf, f64)> {
    let configs = [
        None,
        Some("{\"enabled\": true}"),
        Some("{\"enabled\": false}"),
        Some("{ invalid"),
        Some("[1]"),
        Some("{\"enabled\": 1}"),
    ];
    let statuses = [
        None,
        Some(r#"{"state": "attached", "app_version": "1", "target": {"pid": 42}}"#),
        Some(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed", "inspector_closed": false}"#),
        Some(r#"{"state": "inspector_unclosed", "app_version": "9"}"#),
        Some("[]"),
        Some("{\"reason\": \"x\"}"),
        Some("\u{feff}{\"state\": \"waiting\"}"),
    ];
    let heartbeats = [
        None,
        Some(r#"{"running": true, "updated_at": 99}"#),
        Some(r#"{"running": true, "updated_at": 91.5}"#),
        Some(r#"{"running": false, "updated_at": 99}"#),
        Some(r#"{"running": true, "updated_at": "99"}"#),
        Some(r#"{"running": true}"#),
        Some(r#"{"running": true, "updated_at": true}"#),
        Some("\"x\""),
    ];
    let mut out = Vec::new();
    for (c, config) in configs.iter().enumerate() {
        for (st, status) in statuses.iter().enumerate() {
            for (h, heartbeat) in heartbeats.iter().enumerate() {
                let root = base.join(format!("status-{c}-{st}-{h}"));
                std::fs::create_dir_all(&root).unwrap();
                for (name, contents) in
                    [("config.json", config), ("status.json", status), ("heartbeat.json", heartbeat)]
                {
                    if let Some(contents) = contents {
                        std::fs::write(root.join(name), contents).unwrap();
                    }
                }
                out.push((root, 100.0));
            }
        }
    }
    out.push((base.join("status-missing"), 100.0));
    out
}

fn asar_cases(base: &Path) -> Vec<PathBuf> {
    let package = |size: &str, offset: &str| {
        format!(r#"{{"files": {{"package.json": {{"size": {size}, "offset": {offset}}}}}}}"#)
    };
    let body = KTALK_PACKAGE_BODY;
    let len = body.len().to_string();
    let header = |tree: &str, declared: Option<u32>| {
        let tree = tree.as_bytes();
        let size = declared.unwrap_or(u32::try_from(tree.len()).unwrap());
        let mut out = Vec::new();
        for word in [4u32, 8 + u32::try_from(tree.len()).unwrap(), 4, size] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(tree);
        out
    };
    let cases: Vec<Vec<u8>> = vec![
        asar(KTALK_PACKAGE_BODY),
        asar(r#"{"name": "impostor", "main": "build/main.js"}"#),
        asar(r#"{"name": "ktalk"}"#),
        asar("[1, 2]"),
        asar("\"text\""),
        asar("{broken"),
        b"truncated".to_vec(),
        Vec::new(),
        [header(&package(&len, "\"0\""), Some(0)), body.as_bytes().to_vec()].concat(),
        [header(&package(&len, "\"0\""), Some(9 * 1024 * 1024)), body.as_bytes().to_vec()].concat(),
        [header(&package(&len, "\" 0 \""), None), body.as_bytes().to_vec()].concat(),
        [header(&package(&format!("{len}.9"), "0"), None), body.as_bytes().to_vec()].concat(),
        [header(&package(&format!("\"{len}\""), "\"1_0\""), None), body.as_bytes().to_vec()].concat(),
        [header(&package("\"abc\"", "0"), None), body.as_bytes().to_vec()].concat(),
        [header(&package("Infinity", "0"), None), body.as_bytes().to_vec()].concat(),
        [header(&package(&len, "-100"), None), body.as_bytes().to_vec()].concat(),
        [header(&package("0", "0"), None), body.as_bytes().to_vec()].concat(),
        [header(&package("70000", "0"), None), body.as_bytes().to_vec()].concat(),
        [header(&package(&len, "null"), None), body.as_bytes().to_vec()].concat(),
        [header(r#"{"files": []}"#, None), body.as_bytes().to_vec()].concat(),
        [header(r#"{"files": {"package.json": "x"}}"#, None), body.as_bytes().to_vec()].concat(),
        [header("[]", None), body.as_bytes().to_vec()].concat(),
    ];
    let mut out = Vec::new();
    for (i, contents) in cases.into_iter().enumerate() {
        let dir = base.join(format!("asar-{i}"));
        let executable = if cfg!(target_os = "macos") {
            std::fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
            std::fs::create_dir_all(dir.join("Contents/Resources")).unwrap();
            std::fs::write(dir.join("Contents/Resources/app.asar"), contents).unwrap();
            dir.join("Contents/MacOS/ktalk")
        } else {
            std::fs::create_dir_all(dir.join("resources")).unwrap();
            std::fs::write(dir.join("resources/app.asar"), contents).unwrap();
            dir.join("ktalk")
        };
        std::fs::write(&executable, "").unwrap();
        out.push(executable);
    }
    out.push(base.join("asar-missing/Contents/MacOS/ktalk"));
    out
}

const KTALK_PACKAGE_BODY: &str = r#"{"name": "ktalk", "main": "build/main.js"}"#;

fn fuse_inputs(base: &Path, rng: &mut Rng) -> Vec<PathBuf> {
    let enabled = fuse_wire(true, 1, 8);
    let mut blobs = vec![
        enabled.clone(),
        fuse_wire(false, 1, 8),
        fuse_wire(true, 2, 8),
        fuse_wire(true, 1, 3),
        fuse_wire(true, 1, 4),
        [enabled.clone(), b"gap".to_vec(), enabled.clone()].concat(),
        enabled.repeat(3),
        [b"padding".repeat(9360), enabled.clone()].concat(),
        [vec![b'x'; 65536 - 20], enabled.clone()].concat(),
        [vec![b'x'; 65536 - 33], enabled.clone(), vec![b'y'; 100]].concat(),
        [b"prefix".to_vec(), recog_desktop::processes::FUSE_SENTINEL.to_vec(), b"\x01\x08000".to_vec()].concat(),
        [b"prefix".to_vec(), recog_desktop::processes::FUSE_SENTINEL.to_vec(), b"\x01".to_vec()].concat(),
        recog_desktop::processes::FUSE_SENTINEL.to_vec(),
        b"ordinary binary".to_vec(),
    ];
    for _ in 0..40 {
        let mut blob = Vec::new();
        for _ in 0..rng.below(5) {
            match rng.below(4) {
                0 => blob.extend(fuse_wire(rng.below(3) > 0, 1, 8)),
                1 => blob.extend(vec![b'z'; rng.below(70_000)]),
                2 => blob.extend(recog_desktop::processes::FUSE_SENTINEL),
                _ => blob.extend(fuse_wire(
                    true,
                    u8::try_from(rng.below(3)).unwrap(),
                    u8::try_from(rng.below(10)).unwrap(),
                )),
            }
        }
        blobs.push(blob);
    }
    let mut out = Vec::new();
    for (i, blob) in blobs.into_iter().enumerate() {
        let path = base.join(format!("fuses-{i}"));
        std::fs::write(&path, blob).unwrap();
        out.push(path);
    }
    out.push(base.join("fuses-missing"));
    out
}

fn application_inputs(base: &Path, home: &Path) -> Vec<String> {
    let valid = fake_app(base, "FakeTalk");
    let wrong = base.join("Wrong.app/Contents/MacOS");
    std::fs::create_dir_all(&wrong).unwrap();
    std::fs::write(base.join("Wrong.app/Contents/Info.plist"), plist("other", "Wrong")).unwrap();
    std::fs::create_dir_all(base.join("NoExe.app/Contents")).unwrap();
    std::fs::write(base.join("NoExe.app/Contents/Info.plist"), plist("kontur.talk", "Missing")).unwrap();
    std::fs::create_dir_all(base.join("Broken.app/Contents")).unwrap();
    std::fs::write(base.join("Broken.app/Contents/Info.plist"), "not a plist").unwrap();
    std::fs::create_dir_all(base.join("Array.app/Contents")).unwrap();
    std::fs::write(
        base.join("Array.app/Contents/Info.plist"),
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><array/></plist>",
    )
    .unwrap();
    std::fs::create_dir_all(base.join("NoKey.app/Contents")).unwrap();
    std::fs::write(
        base.join("NoKey.app/Contents/Info.plist"),
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>kontur.talk</string></dict></plist>",
    )
    .unwrap();
    let executable = base.join("plain-exe");
    std::fs::write(&executable, "").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let not_executable = base.join("plain-file");
    std::fs::write(&not_executable, "").unwrap();
    std::os::unix::fs::symlink(&valid.app, base.join("link.app")).unwrap();
    std::os::unix::fs::symlink(base.join("loop-b"), base.join("loop-a")).unwrap();
    std::os::unix::fs::symlink(base.join("loop-a"), base.join("loop-b")).unwrap();
    std::fs::create_dir_all(home.join("Apps")).unwrap();
    std::os::unix::fs::symlink(&valid.app, home.join("Apps/FakeTalk.app")).unwrap();
    [
        valid.app.to_string_lossy().into_owned(),
        format!("{}/", valid.app.display()),
        format!("{}/../{}", valid.app.display(), valid.app.file_name().unwrap().to_string_lossy()),
        base.join("link.app").to_string_lossy().into_owned(),
        base.join("Wrong.app").to_string_lossy().into_owned(),
        base.join("NoExe.app").to_string_lossy().into_owned(),
        base.join("Broken.app").to_string_lossy().into_owned(),
        base.join("Array.app").to_string_lossy().into_owned(),
        base.join("NoKey.app").to_string_lossy().into_owned(),
        executable.to_string_lossy().into_owned(),
        not_executable.to_string_lossy().into_owned(),
        base.join("missing/deeper/x.app").to_string_lossy().into_owned(),
        base.join("loop-a").to_string_lossy().into_owned(),
        "~/Apps/FakeTalk.app".to_owned(),
        "~".to_owned(),
        String::new(),
        "/".to_owned(),
    ]
    .into_iter()
    .collect()
}

fn target_dicts() -> Vec<Value> {
    [
        r#"{"pid": 42, "started": 12.0, "executable": "/ktalk"}"#,
        r#"{"pid": "42", "started": "12.5", "executable": 7}"#,
        r#"{"pid": 42.9, "started": 12, "executable": null}"#,
        r#"{"pid": true, "started": false, "executable": [1]}"#,
        r#"{"pid": 42, "started": 12.0}"#,
        r#"{"started": 12.0, "executable": "/x"}"#,
        r#"{"pid": "x", "started": 1, "executable": "/x"}"#,
        r#"{"pid": 1, "started": "nan", "executable": "/x"}"#,
        r#"{"pid": 1, "started": " 1_0.5 ", "executable": "/x"}"#,
        r#"{"pid": null, "started": 1, "executable": "/x"}"#,
        r#"{"pid": 1, "started": [], "executable": "/x"}"#,
        "[1]",
        "\"text\"",
        "null",
    ]
    .iter()
    .map(|t| obj(t))
    .collect()
}

#[test]
fn functions_given_the_same_inputs_when_called_on_both_sides_then_same_answers() {
    let Some(python) = python() else { return };
    let base_dir = tempfile::tempdir().unwrap();
    let base = side_dir(base_dir.path(), "shared");
    let home = side_dir(base_dir.path(), "home");
    let world = World::new(&base.join("world"));
    let (inspector_port, ready_port) = (free_port(), free_port());
    let host = host(&world, inspector_port, ready_port, &home);
    let mut rng = Rng(0x00de_5c70_9a11);

    // Read-only calls on shared inputs.
    let mut calls = Vec::new();
    for words in shlex_inputs(&mut rng) {
        calls.push(call("shlex", vec![s(&words)]));
    }
    // The port follows urlsplit's bracketed-netloc check, which an older 3.12 patch
    // release lacks; with such an interpreter bracketed hosts are not compared.
    let info = ask_python(&python, &py::obj([("kind", s("python")), ("repo", path_value(&repo_root()))]), &[]);
    let current_urlsplit = field(&info, "bracketed_netloc").truthy();
    for targets in url_inputs(&mut rng) {
        let url = match &targets {
            Value::Array(items) if items.len() == 1 => field(&items[0], "webSocketDebuggerUrl").py_str(),
            _ => String::new(),
        };
        let bracketed = url.contains('[') && url.contains(']');
        if bracketed && !current_urlsplit {
            continue;
        }
        calls.push(call("inspector_url", vec![targets]));
    }
    for (root, now) in status_roots(&base) {
        calls.push(call("status", vec![path_value(&root), Value::Float(now)]));
    }
    for raw in application_inputs(&base, &home) {
        calls.push(call("application_path", vec![s(&raw)]));
    }
    for executable in asar_cases(&base) {
        calls.push(call("is_ktalk_executable", vec![path_value(&executable)]));
    }
    for binary in fuse_inputs(&base, &mut rng) {
        calls.push(call("inspect_fuses", vec![path_value(&binary)]));
    }
    for data in target_dicts() {
        calls.push(call("target_from_dict", vec![data]));
    }
    for (i, contents) in
        ["{\"a\": 1}", "\u{feff}{}", "{\"a\": \"\r\n\"}", "{\r\n\"a\":\r1}", "", "[1, 2.5, NaN]", "\u{ff}"]
            .iter()
            .enumerate()
    {
        let path = base.join(format!("read-{i}.json"));
        if i == 6 {
            std::fs::write(&path, [0xffu8, 0xfe]).unwrap();
        } else {
            std::fs::write(&path, contents).unwrap();
        }
        calls.push(call("read_json", vec![path_value(&path)]));
    }
    calls.push(call("read_json", vec![path_value(&base.join("read-missing.json"))]));
    calls.push(call("read_json", vec![path_value(&base)]));

    // The process checks over one scripted world.
    let app = fake_app(&base, "FakeTalk");
    let mut processes_list = vec![ktalk_process(42, &app, ready_port)];
    let mut helper = ktalk_process(43, &app, ready_port);
    helper.as_object_mut().unwrap().insert("cmdline", obj(r#"["ktalk", "--type=renderer"]"#));
    processes_list.push(helper);
    let mut foreign = ktalk_process(44, &app, ready_port);
    foreign.as_object_mut().unwrap().insert("uid", Value::Int(i128::from(host.uid) + 1));
    processes_list.push(foreign);
    let mut denied = ktalk_process(45, &app, ready_port);
    denied.as_object_mut().unwrap().insert("errors", obj(r#"{"cmdline": "AccessDenied"}"#));
    processes_list.push(denied);
    let mut unnamed = ktalk_process(46, &app, ready_port);
    unnamed.as_object_mut().unwrap().insert("errors", obj(r#"{"name": "AccessDenied"}"#));
    processes_list.push(unnamed);
    let mut zombie = ktalk_process(47, &app, ready_port);
    zombie.as_object_mut().unwrap().insert("errors", obj(r#"{"name": "ZombieProcess"}"#));
    processes_list.push(zombie);
    let mut other_name = ktalk_process(48, &app, ready_port);
    other_name.as_object_mut().unwrap().insert("name", s("Other"));
    processes_list.push(other_name);
    let options = [
        r#"{"NODE_OPTIONS": "--inspect"}"#,
        r#"{"NODE_OPTIONS": "\"bad"}"#,
        r#"{"ELECTRON_RUN_AS_NODE": "1"}"#,
        r#"{"NODE_OPTIONS": "--max-old-space-size=1 --disable_sigusr1"}"#,
        r#"{"NODE_OPTIONS": "--max-old-space-size=512"}"#,
    ];
    for (i, environ) in options.iter().enumerate() {
        let mut process = ktalk_process(50 + i64::try_from(i).unwrap(), &app, ready_port);
        process.as_object_mut().unwrap().insert("environ", obj(environ));
        process.as_object_mut().unwrap().insert("name", s("Other"));
        processes_list.push(process);
    }
    let mut listening = ktalk_process(60, &app, ready_port);
    listening.as_object_mut().unwrap().insert(
        "listeners",
        obj(&format!(r#"[["127.0.0.1", {inspector_port}], ["::1", {inspector_port}], ["127.0.0.1", {ready_port}]]"#)),
    );
    listening.as_object_mut().unwrap().insert("name", s("Other"));
    processes_list.push(listening);
    let mut wildcard = ktalk_process(61, &app, ready_port);
    wildcard
        .as_object_mut()
        .unwrap()
        .insert("listeners", obj(&format!(r#"[["0.0.0.0", {inspector_port}], ["127.0.0.1", {inspector_port}]]"#)));
    wildcard.as_object_mut().unwrap().insert("name", s("Other"));
    processes_list.push(wildcard);
    let mut gone = ktalk_process(62, &app, ready_port);
    gone.as_object_mut().unwrap().insert("errors", obj(r#"{"environ": "NoSuchProcess", "listeners": "AccessDenied"}"#));
    gone.as_object_mut().unwrap().insert("name", s("Other"));
    processes_list.push(gone);
    world.set(&world_of(processes_list.clone()));
    calls.push(call("find_targets", vec![path_value(&app.app)]));
    calls.push(call("find_targets", vec![path_value(&base.join("Elsewhere.app"))]));
    for process in &processes_list {
        let target = py::obj([
            ("pid", field(process, "pid").clone()),
            ("started", field(process, "started").clone()),
            ("executable", field(process, "exe").clone()),
        ]);
        calls.push(call("ready", vec![target.clone()]));
        calls.push(call("same_process", vec![target.clone(), path_value(&app.app)]));
        calls.push(call("same_process", vec![target.clone(), Value::Null]));
        calls.push(call("owns_inspector", vec![target.clone()]));
        calls.push(call("inspector_supported", vec![target]));
    }
    let stale =
        py::obj([("pid", Value::Int(42)), ("started", Value::Float(1.0)), ("executable", path_value(&app.executable))]);
    calls.push(call("same_process", vec![stale.clone(), path_value(&app.app)]));
    calls.push(call("inspector_supported", vec![stale]));
    for bundle in [s("bad"), Value::Int(1), s(&"a".repeat(64)), s(&"A".repeat(64)), Value::Null] {
        calls.push(call("validate_bundle", vec![path_value(&base.join("data")), py::obj([("bundle", bundle)])]));
    }
    calls.push(call("validate_bundle", vec![path_value(&base.join("data")), obj("[]")]));

    let env = world.env(inspector_port, ready_port, &home);
    let request =
        py::obj([("kind", s("functions")), ("repo", path_value(&repo_root())), ("calls", Value::Array(calls.clone()))]);
    let answers = ask_python(&python, &request, &env);
    let mine: Vec<Value> = calls.iter().map(|c| rust_call(&host, c)).collect();
    let compared = compare("functions", &calls, &answers, &mine, &[]);

    // Calls that write: each side on its own copy, its directory named alike.
    let py_dir = side_dir(base_dir.path(), "py");
    let rs_dir = side_dir(base_dir.path(), "rs");
    let writes = |dir: &Path| {
        let source = dist(dir);
        let ordered = dir.join("ordered");
        for name in ["a/b", "a-b", "a.b", "ab", "a/c/d", "Z", "é", ".hidden"] {
            std::fs::create_dir_all(ordered.join(name).parent().unwrap()).unwrap();
            std::fs::write(ordered.join(name), name).unwrap();
        }
        let linked = dir.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join("a"), "a").unwrap();
        std::os::unix::fs::symlink(linked.join("a"), linked.join("z")).unwrap();
        let root = dir.join("data");
        let statuses = [
            r#"{"state": "attached", "inspector_closed": true}"#,
            r#"{"state": "error", "reason": "worker_failed", "inspector_closed": null}"#,
            r#"{"state": "inspector_unclosed", "inspector_closed": false}"#,
            r#"{"state": "waiting", "inspector_closed": true}"#,
            r#"{"state": "error", "inspector_closed": 1}"#,
            r#"{"state": "waiting"}"#,
        ];
        let mut calls = vec![
            call("bundle_digest", vec![path_value(&source)]),
            call("bundle_digest", vec![path_value(&ordered)]),
            call("bundle_digest", vec![path_value(&linked)]),
            call("bundle_digest", vec![path_value(&dir.join("absent"))]),
            call("prepare_bundle", vec![path_value(&source), path_value(&root)]),
            call("prepare_bundle", vec![path_value(&source), path_value(&root)]),
            call("prepare_bundle", vec![path_value(&ordered), path_value(&root)]),
            call("prepare_bundle", vec![path_value(&linked), path_value(&root)]),
        ];
        for status in statuses {
            calls.push(call("write_status", vec![path_value(&root), obj(status)]));
        }
        calls.push(call("read_json", vec![path_value(&root.join("status.json"))]));
        calls
    };
    let py_calls = writes(&py_dir);
    let rs_calls = writes(&rs_dir);
    let request = py::obj([
        ("kind", s("functions")),
        ("repo", path_value(&repo_root())),
        ("calls", Value::Array(py_calls.clone())),
    ]);
    let answers = ask_python(&python, &request, &env);
    let mine: Vec<Value> = rs_calls.iter().map(|c| rust_call(&host, c)).collect();
    let replacements = [
        (py_dir.to_string_lossy().into_owned(), "<side>".to_owned()),
        (rs_dir.to_string_lossy().into_owned(), "<side>".to_owned()),
    ];
    let written = compare("writes", &py_calls, &answers, &mine, &replacements);
    let (py_tree, rs_tree) = (tree(&py_dir.join("data")), tree(&rs_dir.join("data")));
    assert_eq!(py_tree, rs_tree, "snapshot trees differ");
    eprintln!("functions: {compared} calls and {written} writing calls agree");
}

// --- worker ----------------------------------------------------------------------

struct Side {
    dir: PathBuf,
    world: World,
    home: PathBuf,
    inspector_port: u16,
    ready_port: u16,
    app: FakeApp,
}

impl Side {
    fn new(base: &Path, name: &str) -> Self {
        let dir = side_dir(base, name);
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let world = World::new(&dir.join("world"));
        let app = fake_app(&dir, "FakeTalk");
        Self { world, home, inspector_port: free_port(), ready_port: free_port(), app, dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.join("data/desktop")
    }

    fn host(&self) -> Host {
        host(&self.world, self.inspector_port, self.ready_port, &self.home)
    }

    fn env(&self) -> Vec<(String, String)> {
        self.world.env(self.inspector_port, self.ready_port, &self.home)
    }

    fn target(&self) -> Target {
        let world = self.world.get();
        let Value::Array(list) = field(&world, "processes") else { panic!() };
        let process = list.iter().find(|p| field(p, "pid") == &Value::Int(42)).unwrap();
        Target::from_value(&py::obj([
            ("pid", Value::Int(42)),
            ("started", field(process, "started").clone()),
            ("executable", field(process, "exe").clone()),
        ]))
        .unwrap()
    }

    fn replacements(&self) -> Vec<(String, String)> {
        vec![
            (self.dir.to_string_lossy().into_owned(), "<side>".into()),
            (format!(":{}/", self.inspector_port), ":<port>/".into()),
        ]
    }

    /// What the worker leaves behind: files, signals, the inspector's log.
    fn traces(&self, inspector: &Inspector) -> Value {
        let mut files = Map::new();
        for name in ["config", "status", "operation", "runtime"] {
            let path = self.root().join(format!("{name}.json"));
            files.insert(name, if path.exists() { storage::read(&path).unwrap() } else { Value::Null });
        }
        let signals = Value::Array(self.world.signalled().into_iter().map(|p| Value::Int(i128::from(p))).collect());
        py::obj([
            ("files", Value::Object(files)),
            ("signals", signals),
            ("inspector", Value::Array(inspector.entries())),
        ])
    }
}

/// A worker scenario: how the world and the inspector are set up, and how `run` is
/// called.
struct Scenario {
    name: &'static str,
    behaviour: Behaviour,
    setup: fn(&Side),
    recover: bool,
    with_target: bool,
}

fn installed(side: &Side) {
    side.world.set(&world_of(vec![ktalk_process(42, &side.app, side.ready_port)]));
    let (digest, _) = storage::prepare_bundle(&dist(&side.dir), &side.root()).unwrap();
    let config = py::obj([("enabled", Value::Bool(true)), ("app", path_value(&side.app.app)), ("bundle", s(&digest))]);
    storage::write_json(&side.root().join("config.json"), &config).unwrap();
}

fn scenarios() -> Vec<Scenario> {
    let plain = |name, behaviour| Scenario { name, behaviour, setup: installed, recover: false, with_target: true };
    vec![
        plain("attached", Behaviour::default()),
        plain("attached without events", Behaviour { events: false, ..Behaviour::default() }),
        plain("identity mismatch", Behaviour { identity: "mismatch".into(), ..Behaviour::default() }),
        plain("identity throws", Behaviour { identity: "throw".into(), ..Behaviour::default() }),
        plain("identity error reply", Behaviour { identity: "error".into(), ..Behaviour::default() }),
        plain("identity garbage", Behaviour { identity: "garbage".into(), ..Behaviour::default() }),
        plain("websocket opens late", Behaviour { identity: "slow_open".into(), ..Behaviour::default() }),
        plain("bootstrap throws", Behaviour { bootstrap: "throw".into(), ..Behaviour::default() }),
        plain("bootstrap invalid", Behaviour { bootstrap: "invalid".into(), ..Behaviour::default() }),
        plain("bootstrap names state", Behaviour { bootstrap: "state".into(), ..Behaviour::default() }),
        plain("two inspector targets", Behaviour { list: "two".into(), ..Behaviour::default() }),
        plain("foreign inspector host", Behaviour { list: "foreign".into(), ..Behaviour::default() }),
        plain("oversized target list", Behaviour { list: "big".into(), ..Behaviour::default() }),
        plain("target list fails", Behaviour { list: "status500".into(), ..Behaviour::default() }),
        plain("listener lost while listing", Behaviour { drop_on_list: true, ..Behaviour::default() }),
        plain(
            "process exits on signal",
            Behaviour { exit_on_signal: true, open_on_signal: false, ..Behaviour::default() },
        ),
        plain("close ignored", Behaviour { close: "ignore".into(), ..Behaviour::default() }),
        plain("port busy", Behaviour { listen_from_start: true, ..Behaviour::default() }),
        Scenario {
            name: "not ready",
            behaviour: Behaviour::default(),
            setup: |side| {
                installed(side);
                side.world.update(|list| {
                    list[0].as_object_mut().unwrap().insert("listeners", Value::Array(vec![]));
                });
            },
            recover: false,
            with_target: true,
        },
        Scenario {
            name: "inspector unsupported",
            behaviour: Behaviour::default(),
            setup: |side| {
                installed(side);
                std::fs::write(&side.app.fuses, fuse_wire(false, 1, 8)).unwrap();
            },
            recover: false,
            with_target: true,
        },
        Scenario {
            name: "process replaced",
            behaviour: Behaviour::default(),
            setup: |side| {
                installed(side);
                side.world.update(|list| {
                    list[0].as_object_mut().unwrap().insert("started", Value::Float(5.0));
                });
            },
            recover: false,
            with_target: true,
        },
        Scenario {
            name: "corrupt snapshot",
            behaviour: Behaviour::default(),
            setup: |side| {
                installed(side);
                let config = storage::read(&side.root().join("config.json")).unwrap();
                let bundle = text(&config, "bundle").unwrap();
                std::fs::write(side.root().join("extensions").join(bundle).join("injected.js"), "tampered").unwrap();
            },
            recover: false,
            with_target: true,
        },
        Scenario {
            name: "previous operation of an exited process",
            behaviour: Behaviour::default(),
            setup: |side| {
                installed(side);
                let old = obj(
                    r#"{"target": {"pid": 41, "started": 11.0, "executable": "/old"}, "phase": "opened", "token": "t"}"#,
                );
                storage::write_json(&side.root().join("operation.json"), &old).unwrap();
            },
            recover: false,
            with_target: true,
        },
        Scenario {
            name: "previous operation that cannot be closed",
            behaviour: Behaviour::default(),
            setup: |side| {
                installed(side);
                let mut old = ktalk_process(41, &side.app, side.ready_port);
                old.as_object_mut().unwrap().insert("started", Value::Float(11.0));
                side.world.update(|list| list.push(old));
                let operation = py::obj([
                    (
                        "target",
                        py::obj([
                            ("pid", Value::Int(41)),
                            ("started", Value::Float(11.0)),
                            ("executable", path_value(&side.app.executable)),
                        ]),
                    ),
                    ("phase", s("opening")),
                ]);
                storage::write_json(&side.root().join("operation.json"), &operation).unwrap();
            },
            recover: false,
            with_target: true,
        },
        Scenario {
            name: "attachment lock busy",
            behaviour: Behaviour::default(),
            setup: |side| {
                installed(side);
                std::fs::create_dir_all(side.home.join(".cache/recog")).unwrap();
                std::fs::write(side.root().join("status.json"), r#"{"state": "attached"}"#).unwrap();
            },
            recover: false,
            with_target: true,
        },
        Scenario {
            name: "recovery of an open inspector",
            behaviour: Behaviour { listen_from_start: true, ..Behaviour::default() },
            setup: |side| {
                installed(side);
                side.world.set_listening(42, side.inspector_port, true);
                let target = side.target();
                let operation = py::obj([
                    ("target", target.to_value()),
                    ("phase", s("opened")),
                    ("token", s("t")),
                    ("endpoint", s(&format!("ws://127.0.0.1:{}/session", side.inspector_port))),
                ]);
                storage::write_json(&side.root().join("operation.json"), &operation).unwrap();
            },
            recover: true,
            with_target: false,
        },
        Scenario {
            name: "recovery with nothing open",
            behaviour: Behaviour::default(),
            setup: installed,
            recover: true,
            with_target: false,
        },
        Scenario {
            name: "no target",
            behaviour: Behaviour::default(),
            setup: installed,
            recover: false,
            with_target: false,
        },
        Scenario {
            name: "missing configuration",
            behaviour: Behaviour::default(),
            setup: |side| {
                installed(side);
                std::fs::remove_file(side.root().join("config.json")).unwrap();
            },
            recover: false,
            with_target: true,
        },
    ]
}

/// The inspector's log as its distinct frames in order of first arrival.
fn distinct_conversation(traces: &mut Value) {
    let map = traces.as_object_mut().unwrap();
    let Some(Value::Array(entries)) = map.get("inspector").cloned() else { return };
    let mut seen = Vec::new();
    for entry in entries {
        if !seen.iter().any(|e: &Value| py::dumps(e) == py::dumps(&entry)) {
            seen.push(entry);
        }
    }
    map.insert("inspector", Value::Array(seen));
}

/// Holds the attachment lock for the "lock busy" scenario.
fn hold_lock(side: &Side) -> Option<std::fs::File> {
    let path = side.home.join(".cache/recog/desktop-attach.lock");
    path.parent().filter(|p| p.exists())?;
    let file = std::fs::File::create(path).unwrap();
    file.try_lock().unwrap();
    Some(file)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_given_scripted_processes_and_inspector_when_run_then_same_results_files_and_conversation() {
    let Some(python) = python() else { return };
    let base = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    let all = scenarios();
    for (i, scenario) in all.iter().enumerate() {
        let py_side = Side::new(base.path(), &format!("py-{i}"));
        let rs_side = Side::new(base.path(), &format!("rs-{i}"));
        let mut answers = Vec::new();
        for (side, is_python) in [(&py_side, true), (&rs_side, false)] {
            (scenario.setup)(side);
            let _lock = if scenario.name == "attachment lock busy" { hold_lock(side) } else { None };
            let inspector = Inspector::start(side.inspector_port, &side.world, scenario.behaviour.clone(), 42).await;
            let target = scenario.with_target.then(|| side.target());
            let answer = if is_python {
                let request = py::obj([
                    ("kind", s("worker")),
                    ("repo", path_value(&repo_root())),
                    ("root", path_value(&side.root())),
                    ("target", target.as_ref().map_or(Value::Null, Target::to_value)),
                    ("recover", Value::Bool(scenario.recover)),
                ]);
                let (python, env) = (python.clone(), side.env());
                tokio::task::spawn_blocking(move || ask_python(&python, &request, &env)).await.unwrap()
            } else {
                outcome(worker::run(&side.host(), &side.root(), target.as_ref(), scenario.recover).await, |v| v)
            };
            // Let a scheduled close land before the traces are read.
            tokio::time::sleep(Duration::from_millis(250)).await;
            let mut traces = side.traces(&inspector);
            inspector.stop();
            if scenario.name == "close ignored" {
                // How many 200 ms retries fit in the 5 s cleanup depends on timing.
                distinct_conversation(&mut traces);
            }
            answers.push(mask(&localize(&py::obj([("answer", answer), ("traces", traces)]), &side.replacements())));
        }
        if py::dumps(&answers[0]) != py::dumps(&answers[1]) {
            failures.push(format!(
                "{}\n  python: {}\n  rust:   {}",
                scenario.name,
                py::dumps(&answers[0]),
                py::dumps(&answers[1])
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} worker scenarios differ:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}

// --- observer --------------------------------------------------------------------

/// The Rust worker child of the observer scenarios: this test binary run again with
/// the worker's arguments after `--`.
#[test]
fn fake_worker_child() {
    if std::env::var_os("DESKTOP_WORKER_CHILD").is_none() {
        return;
    }
    let args: Vec<String> = std::env::args().skip_while(|a| a != "--").skip(1).collect();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let code = runtime.block_on(worker::main(&host_from_env(), &args));
    std::process::exit(code);
}

#[derive(Default)]
struct RecordingNotifier {
    calls: Mutex<Vec<Value>>,
    notify_send: bool,
}

impl Notifier for RecordingNotifier {
    fn run(&self, args: Vec<String>) -> BoxFuture<'_, std::io::Result<NotifyOutcome>> {
        self.calls.lock().unwrap().push(Value::Array(args.into_iter().map(Value::from).collect()));
        Box::pin(async { Ok(NotifyOutcome::Exited) })
    }

    fn which(&self, _name: &str) -> Option<PathBuf> {
        self.notify_send.then(|| PathBuf::from("/usr/bin/notify-send"))
    }
}

/// One step of an observer timeline: files written or removed, the world replaced,
/// a restart (new observer, lock released), one tick or a bounded run.
fn step(pairs: &[(&str, Value)]) -> Value {
    Value::Object(pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect())
}

fn timelines(side: &Side) -> Vec<(&'static str, Vec<Value>)> {
    let ktalk = ktalk_process(42, &side.app, side.ready_port);
    let config = |enabled: bool| {
        let (digest, _) = storage::prepare_bundle(&dist(&side.dir), &side.root()).unwrap();
        py::obj([("enabled", Value::Bool(enabled)), ("app", path_value(&side.app.app)), ("bundle", s(&digest))])
    };
    let target = py::obj([
        ("pid", Value::Int(42)),
        ("started", field(&ktalk, "started").clone()),
        ("executable", field(&ktalk, "exe").clone()),
    ]);
    let files =
        |pairs: &[(&str, Value)]| Value::Object(pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect());
    let tick = ("tick", Value::Bool(true));
    let mut second = ktalk.clone();
    second.as_object_mut().unwrap().insert("pid", Value::Int(43));
    let mut not_ready = ktalk.clone();
    not_ready.as_object_mut().unwrap().insert("listeners", Value::Array(vec![]));
    vec![
        ("inert", vec![step(std::slice::from_ref(&tick))]),
        (
            "attach once and keep",
            vec![
                step(&[
                    ("files", files(&[("config.json", config(true))])),
                    ("world", world_of(vec![ktalk.clone()])),
                    tick.clone(),
                ]),
                step(std::slice::from_ref(&tick)),
                step(&[("restart", Value::Bool(true)), tick.clone()]),
            ],
        ),
        (
            "no process, then two, then one not ready, then ready",
            vec![
                step(&[("files", files(&[("config.json", config(true))])), tick.clone()]),
                step(&[("world", world_of(vec![ktalk.clone(), second])), tick.clone()]),
                step(&[("world", world_of(vec![not_ready])), tick.clone()]),
                step(&[("world", world_of(vec![ktalk.clone()])), tick.clone()]),
            ],
        ),
        (
            "late runtime incompatibility and page recovery",
            vec![
                step(&[
                    ("files", files(&[("config.json", config(true))])),
                    ("world", world_of(vec![ktalk.clone()])),
                    tick.clone(),
                ]),
                step(&[
                    (
                        "files",
                        files(&[(
                            "runtime.json",
                            py::obj([
                                ("state", s("incompatible")),
                                ("reason", s("page_initialization_failed")),
                                ("target", target.clone()),
                            ]),
                        )]),
                    ),
                    tick.clone(),
                ]),
                step(&[
                    (
                        "files",
                        files(&[(
                            "runtime.json",
                            py::obj([("state", s("ready")), ("reason", Value::Null), ("target", target.clone())]),
                        )]),
                    ),
                    tick.clone(),
                ]),
            ],
        ),
        (
            "interrupted operation recovered, then closed when the process exits",
            vec![
                step(&[
                    (
                        "files",
                        files(&[
                            ("config.json", config(false)),
                            (
                                "operation.json",
                                py::obj([("target", target.clone()), ("phase", s("opened")), ("token", s("t"))]),
                            ),
                        ]),
                    ),
                    ("world", world_of(vec![ktalk.clone()])),
                    tick.clone(),
                ]),
                step(std::slice::from_ref(&tick)),
                step(&[("world", world_of(vec![])), tick.clone()]),
            ],
        ),
        (
            "unsupported inspector",
            vec![step(&[
                ("files", files(&[("config.json", config(true)), ("status.json", py::obj([("state", s("waiting"))]))])),
                ("world", world_of(vec![ktalk.clone()])),
                ("unsupported", Value::Bool(true)),
                tick.clone(),
            ])],
        ),
        (
            "missing application fails the loop",
            vec![
                step(&[
                    ("files", files(&[("config.json", obj(r#"{"enabled": true, "app": "/missing/KTalk.AppImage"}"#))])),
                    ("run", Value::Int(3)),
                ]),
                step(&[("files", files(&[("config.json", obj(r#"{"enabled": true}"#))])), ("run", Value::Int(1))]),
                step(&[("files", files(&[("config.json", obj("[1]"))])), tick.clone()]),
            ],
        ),
    ]
}

fn rust_snapshot(side: &Side, error: Value, logs: Vec<String>) -> Value {
    let mut files = Map::new();
    for name in ["config", "status", "operation", "heartbeat", "runtime"] {
        let path = side.root().join(format!("{name}.json"));
        files.insert(name, if path.exists() { storage::read(&path).unwrap() } else { Value::Null });
    }
    py::obj([
        ("files", Value::Object(files)),
        ("status", service::status(&side.root())),
        ("error", error),
        ("logs", Value::Array(logs.into_iter().map(Value::from).collect())),
    ])
}

async fn rust_timeline(side: &Side, steps: &[Value]) -> Vec<Value> {
    let notifier = Arc::new(RecordingNotifier::default());
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let make = || {
        let host = side.host();
        let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> =
            side.env().into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        env.push(("DESKTOP_WORKER_CHILD".into(), "1".into()));
        let command = WorkerCommand {
            program: std::env::current_exe().unwrap(),
            prefix: ["fake_worker_child", "--exact", "--quiet", "--"].iter().map(Into::into).collect(),
            cwd: None,
            env,
        };
        let sink = Arc::clone(&lines);
        let context = Context {
            launcher: Arc::new(service::ProcessLauncher { spawner: Arc::new(command), host: host.clone() }),
            host,
            notifier: notifier.clone(),
            log: Arc::new(Log::to(move |line| sink.lock().unwrap().push(line.to_owned()))),
            poll: Duration::ZERO,
        };
        DesktopIntegration::new(side.root(), context)
    };
    let mut current = make();
    let mut out = Vec::new();
    for step in steps {
        if let Some(files) = field(step, "files").as_object() {
            for (name, value) in files.iter() {
                let path = side.root().join(name);
                if value.is_null() {
                    let _ = std::fs::remove_file(&path);
                } else {
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, py::dumps(value)).unwrap();
                }
            }
        }
        if !field(step, "world").is_null() {
            side.world.set(field(step, "world"));
        }
        if field(step, "restart").truthy() {
            current.release_lock().await;
            current = make();
        }
        let mut error = Value::Null;
        if field(step, "tick").truthy()
            && let Err(e) = current.tick().await
        {
            error = py::obj([("kind", s(e.kind)), ("message", s(&e.message))]);
        }
        if let Value::Int(n) = field(step, "run") {
            current.run_ticks(Some(usize::try_from(*n).unwrap())).await;
        }
        let logs = std::mem::take(&mut *lines.lock().unwrap());
        out.push(rust_snapshot(side, error, logs));
    }
    out.push(py::obj([("notifications", Value::Array(notifier.calls.lock().unwrap().clone()))]));
    current.release_lock().await;
    out
}

fn python_wrapper(side: &Side, python: &Path) -> PathBuf {
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/desktop_driver.py");
    let wrapper = side.dir.join("python-worker");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nexec \"{}\" -I \"{}\" child \"$@\"\n", python.display(), driver.display()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_given_a_scripted_timeline_when_ticked_then_same_states_logs_notifications_and_workers() {
    let Some(python) = python() else { return };
    let base = tempfile::tempdir().unwrap();
    let names: Vec<&str> = timelines(&Side::new(base.path(), "names")).iter().map(|(n, _)| *n).collect();
    let mut failures = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let mut results = Vec::new();
        for is_python in [true, false] {
            let side = Side::new(base.path(), &format!("{}-{i}", if is_python { "py" } else { "rs" }));
            let steps = timelines(&side).swap_remove(i).1;
            if steps.iter().any(|s| field(s, "unsupported").truthy()) {
                std::fs::write(&side.app.fuses, fuse_wire(false, 1, 8)).unwrap();
            }
            let inspector = Inspector::start(side.inspector_port, &side.world, Behaviour::default(), 42).await;
            let snapshots = if is_python {
                let request = py::obj([
                    ("kind", s("observer")),
                    ("repo", path_value(&repo_root())),
                    ("root", path_value(&side.root())),
                    ("wrapper", path_value(&python_wrapper(&side, &python))),
                    ("steps", Value::Array(steps)),
                ]);
                let (python, env) = (python.clone(), side.env());
                tokio::task::spawn_blocking(move || ask_python(&python, &request, &env)).await.unwrap()
            } else {
                Value::Array(rust_timeline(&side, &steps).await)
            };
            let signals = Value::Array(side.world.signalled().into_iter().map(|p| Value::Int(i128::from(p))).collect());
            let conversation = Value::Array(inspector.entries());
            inspector.stop();
            let all = py::obj([("snapshots", snapshots), ("signals", signals), ("inspector", conversation)]);
            results.push(mask(&localize(&all, &side.replacements())));
        }
        if py::dumps(&results[0]) != py::dumps(&results[1]) {
            failures.push(format!(
                "{name}\n  python: {}\n  rust:   {}",
                py::dumps(&results[0]),
                py::dumps(&results[1])
            ));
        }
        // Agreeing is not enough: each timeline must also do what it scripts.
        let Value::Array(snapshots) = field(&results[1], "snapshots") else { panic!() };
        let states: Vec<String> = snapshots
            .iter()
            .filter_map(|s| {
                text(field(s, "files").as_object().and_then(|f| f.get("status")).unwrap_or(&Value::Null), "state")
            })
            .collect();
        let signals = py::dumps(field(&results[1], "signals"));
        let expected: (&[&str], &str) = match *name {
            "inert" => (&[], "[]"),
            "attach once and keep" => (&["attached", "attached", "attached"], "[42]"),
            "no process, then two, then one not ready, then ready" => {
                (&["waiting", "waiting", "waiting", "attached"], "[42]")
            }
            "late runtime incompatibility and page recovery" => (&["attached", "incompatible", "attached"], "[42]"),
            "interrupted operation recovered, then closed when the process exits" => {
                (&["inspector_unclosed", "inspector_unclosed", "disabled"], "[]")
            }
            "unsupported inspector" => (&["incompatible"], "[]"),
            "missing application fails the loop" => (&["error", "error", "error"], "[]"),
            other => panic!("no expectation for {other}"),
        };
        assert_eq!(
            (states.as_slice(), signals.as_str()),
            (expected.0.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>().as_slice(), expected.1),
            "{name}"
        );
    }
    assert!(
        failures.is_empty(),
        "{} of {} observer timelines differ:\n{}",
        failures.len(),
        names.len(),
        failures.join("\n")
    );
}

// --- cli -------------------------------------------------------------------------

struct CliSide {
    dir: PathBuf,
    fake_repo: PathBuf,
}

impl CliSide {
    fn new(base: &Path, name: &str) -> Self {
        let dir = side_dir(base, name);
        let fake_repo = dir.join("repo");
        std::fs::create_dir_all(fake_repo.join("browser-extension")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        Self { dir, fake_repo }
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("RECOG_DATA_DIR".into(), self.dir.join("data").to_string_lossy().into_owned()),
            ("HOME".into(), self.dir.join("home").to_string_lossy().into_owned()),
            ("RECOG_CONFIG_FILE".into(), self.dir.join("config.toml").to_string_lossy().into_owned()),
            ("LANG".into(), "en_US.UTF-8".into()),
        ]
    }

    fn run_rust(&self, argv: &[String]) -> Value {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_recog-desktop"));
        command.arg("--repo-root").arg(&self.fake_repo).args(&argv[1..]);
        command.env_remove("RECOG_DATA_DIR");
        for (k, v) in self.env() {
            command.env(k, v);
        }
        let output = command.output().unwrap();
        py::obj([
            ("code", Value::Int(i128::from(output.status.code().unwrap()))),
            ("stdout", s(&String::from_utf8(output.stdout).unwrap())),
            ("stderr", s(&String::from_utf8(output.stderr).unwrap())),
        ])
    }

    fn run_python(&self, python: &Path, argv: &[String]) -> Value {
        let request = py::obj([
            ("kind", s("cli")),
            ("repo", path_value(&repo_root())),
            ("fake_repo", path_value(&self.fake_repo)),
            ("argv", Value::Array(argv.iter().map(|a| s(a)).collect())),
        ]);
        ask_python(python, &request, &self.env())
    }

    fn files(&self) -> Value {
        let mut entries: Vec<(String, (u32, Vec<u8>))> = tree(&self.dir.join("data")).into_iter().collect();
        entries.sort();
        Value::Array(
            entries
                .into_iter()
                .map(|(name, (mode, contents))| {
                    Value::Array(vec![s(&name), Value::Int(i128::from(mode)), s(&String::from_utf8_lossy(&contents))])
                })
                .collect(),
        )
    }
}

/// Each CLI case: what to prepare in a side's directory, then the arguments.
type CliSetup = fn(&CliSide) -> Vec<String>;

fn argv(text: &str) -> Vec<String> {
    std::iter::once("desktop").chain(text.split_whitespace()).map(str::to_owned).collect()
}

fn with_app(side: &CliSide, extra: &str) -> Vec<String> {
    let app = fake_app(&side.dir, "FakeTalk");
    std::fs::rename(dist(&side.dir), side.fake_repo.join("browser-extension/dist")).unwrap();
    let mut args = argv("enable --app");
    args.push(app.app.to_string_lossy().into_owned());
    args.extend(extra.split_whitespace().map(str::to_owned));
    args
}

fn write_config(side: &CliSide, contents: &str) {
    std::fs::create_dir_all(side.dir.join("data/desktop")).unwrap();
    std::fs::write(side.dir.join("data/desktop/config.json"), contents).unwrap();
}

fn cli_cases() -> Vec<(&'static str, CliSetup)> {
    vec![
        ("no arguments", |_| argv("")),
        ("help", |_| argv("-h")),
        ("help long", |_| argv("--help")),
        ("bogus", |_| argv("bogus")),
        ("enable without app", |_| argv("enable")),
        ("enable app without value", |_| argv("enable --app")),
        ("enable help", |_| argv("enable -h")),
        ("status help", |_| argv("status -h")),
        ("disable help", |_| argv("disable -h")),
        ("attach help", |_| argv("attach --he")),
        ("status json value", |_| argv("status --json=1")),
        ("unknown arguments", |_| argv("status --js --bogus x")),
        ("extra positional", |_| argv("disable extra")),
        ("option before command", |_| argv("--x status")),
        ("status json", |_| argv("status --json")),
        ("status", |_| argv("status")),
        ("disable fresh", |_| argv("disable")),
        ("attach without configuration", |_| argv("attach")),
        ("enable missing path", |_| argv("enable --app /nonexistent/Ktalk.app")),
        ("enable a directory", |side| {
            let mut args = argv("enable --app");
            args.push(side.dir.to_string_lossy().into_owned());
            args
        }),
        ("enable", |side| with_app(side, "")),
        ("enable with abbreviation", |side| {
            let mut args = with_app(side, "");
            args[1] = "enable".into();
            args[2] = "--ap".into();
            args
        }),
        ("enable without a build", |side| {
            let app = fake_app(&side.dir, "FakeTalk");
            let mut args = argv("enable --app");
            args.push(app.app.to_string_lossy().into_owned());
            args
        }),
        ("enable under home", |side| {
            let app = fake_app(&side.dir.join("home"), "FakeTalk");
            std::fs::rename(dist(&side.dir), side.fake_repo.join("browser-extension/dist")).unwrap();
            let mut args = argv("enable --app");
            args.push(format!("~/{}", app.app.file_name().unwrap().to_string_lossy()));
            args
        }),
        ("enable a build with a link", |side| {
            let mut args = with_app(side, "");
            std::os::unix::fs::symlink("/etc/hosts", side.fake_repo.join("browser-extension/dist/link")).unwrap();
            args.truncate(4);
            args
        }),
        ("disable keeps the rest", |side| {
            write_config(side, r#"{"enabled": true, "app": "/x", "bundle": "d", "extra": [1, "é"]}"#);
            argv("disable")
        }),
        ("disable a list", |side| {
            write_config(side, "[1]");
            argv("disable")
        }),
        ("disable a broken file", |side| {
            write_config(side, "{broken");
            argv("disable")
        }),
        ("attach a missing app", |side| {
            write_config(side, r#"{"enabled": true, "app": "/nonexistent/KTalk.app"}"#);
            argv("attach")
        }),
        ("attach a non-string app", |side| {
            write_config(side, r#"{"enabled": true, "app": 5}"#);
            argv("attach")
        }),
        ("attach a list", |side| {
            write_config(side, "[]");
            argv("attach")
        }),
        ("status unclosed", |side| {
            write_config(side, "{broken");
            std::fs::write(
                side.dir.join("data/desktop/status.json"),
                r#"{"state": "inspector_unclosed", "reason": "cleanup_failed"}"#,
            )
            .unwrap();
            argv("status")
        }),
        ("status offline json", |side| {
            write_config(side, r#"{"enabled": true}"#);
            argv("status --json")
        }),
        ("status invalid", |side| {
            write_config(side, "[]");
            argv("status")
        }),
        ("status without state", |side| {
            write_config(side, r#"{"enabled": true}"#);
            std::fs::write(side.dir.join("data/desktop/status.json"), r#"{"reason": "x"}"#).unwrap();
            std::fs::write(
                side.dir.join("data/desktop/heartbeat.json"),
                format!(r#"{{"running": true, "updated_at": {}}}"#, 4e9),
            )
            .unwrap();
            argv("status")
        }),
        ("status with a number state", |side| {
            write_config(side, r#"{"enabled": true}"#);
            std::fs::write(side.dir.join("data/desktop/status.json"), r#"{"state": 5, "reason": null}"#).unwrap();
            std::fs::write(
                side.dir.join("data/desktop/heartbeat.json"),
                format!(r#"{{"running": true, "updated_at": {}}}"#, 4e9),
            )
            .unwrap();
            argv("status")
        }),
        ("status with a number reason", |side| {
            write_config(side, r#"{"enabled": true}"#);
            std::fs::write(side.dir.join("data/desktop/status.json"), r#"{"state": "error", "reason": 7}"#).unwrap();
            std::fs::write(
                side.dir.join("data/desktop/heartbeat.json"),
                format!(r#"{{"running": true, "updated_at": {}}}"#, 4e9),
            )
            .unwrap();
            argv("status")
        }),
    ]
}

#[test]
fn cli_given_temporary_data_directories_when_run_by_both_then_same_output_status_and_files() {
    let Some(python) = python() else { return };
    let base = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    let cases = cli_cases();
    for (i, (name, setup)) in cases.iter().enumerate() {
        let py_side = CliSide::new(base.path(), &format!("py-{i}"));
        let rs_side = CliSide::new(base.path(), &format!("rs-{i}"));
        let py_args = setup(&py_side);
        let rs_args = setup(&rs_side);
        let theirs = py::obj([("run", py_side.run_python(&python, &py_args)), ("files", py_side.files())]);
        let mine = py::obj([("run", rs_side.run_rust(&rs_args)), ("files", rs_side.files())]);
        let theirs = localize(&theirs, &[(py_side.dir.to_string_lossy().into_owned(), "<side>".into())]);
        let mine = localize(&mine, &[(rs_side.dir.to_string_lossy().into_owned(), "<side>".into())]);
        if py::dumps(&theirs) != py::dumps(&mine) {
            failures.push(format!(
                "{name}: {py_args:?}\n  python: {}\n  rust:   {}",
                py::dumps(&theirs),
                py::dumps(&mine)
            ));
        }
    }
    assert!(failures.is_empty(), "{} of {} CLI cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_attach_given_scripted_processes_when_run_by_both_then_same_output_and_status() {
    let Some(python) = python() else { return };
    let base = tempfile::tempdir().unwrap();
    let mut failures = Vec::new();
    for (count, unsupported) in [(1, false), (0, false), (2, false), (1, true)] {
        let mut results = Vec::new();
        for is_python in [true, false] {
            let side =
                Side::new(base.path(), &format!("{}-{count}-{unsupported}", if is_python { "py" } else { "rs" }));
            installed(&side);
            let list: Vec<Value> = (0..count).map(|i| ktalk_process(42 + i, &side.app, side.ready_port)).collect();
            side.world.set(&world_of(list));
            if unsupported {
                std::fs::write(&side.app.fuses, fuse_wire(false, 1, 8)).unwrap();
            }
            let inspector = Inspector::start(side.inspector_port, &side.world, Behaviour::default(), 42).await;
            let data_dir = side.dir.join("data");
            let answer = if is_python {
                let request = py::obj([
                    ("kind", s("cli_attach")),
                    ("repo", path_value(&repo_root())),
                    ("data_dir", path_value(&data_dir)),
                ]);
                let (python, env) = (python.clone(), side.env());
                tokio::task::spawn_blocking(move || ask_python(&python, &request, &env)).await.unwrap()
            } else {
                let command = cli::Command::Attach;
                let outcome = cli::main(&side.host(), &command, &data_dir, &repo_root()).await;
                py::obj([("code", Value::Int(i128::from(outcome.code))), ("stdout", s(&outcome.stdout))])
            };
            tokio::time::sleep(Duration::from_millis(250)).await;
            let traces = side.traces(&inspector);
            inspector.stop();
            let mut stdout_value = answer.clone();
            if let Some(map) = stdout_value.as_object_mut()
                && let Some(Value::Str(stdout)) = map.get("stdout").cloned()
                && let Ok(parsed) = recog_core::pyjson::loads(&stdout)
            {
                map.insert("stdout", parsed);
            }
            results
                .push(mask(&localize(&py::obj([("answer", stdout_value), ("traces", traces)]), &side.replacements())));
        }
        if py::dumps(&results[0]) != py::dumps(&results[1]) {
            failures.push(format!(
                "{count} {unsupported}\n  python: {}\n  rust:   {}",
                py::dumps(&results[0]),
                py::dumps(&results[1])
            ));
        }
    }
    assert!(failures.is_empty(), "manual attach differs:\n{}", failures.join("\n"));
}

// --- the real process table ------------------------------------------------------

#[test]
fn real_process_table_given_a_process_this_test_started_when_read_then_psutil_agrees() {
    let Some(python) = python() else { return };
    // A process of our own: a Python child listening on an ephemeral loopback port.
    let code = "import socket, sys, time\ns = socket.socket()\ns.bind(('127.0.0.1', 0))\ns.listen()\n\
                print(s.getsockname()[1], flush=True)\ntime.sleep(60)";
    let mut child = std::process::Command::new(&python)
        .args(["-I", "-c", code, "an argument"])
        .env("RECOG_DESKTOP_PROBE", "value=with=equals")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.as_mut().unwrap()), &mut line).unwrap();
    let port: u16 = line.trim().parse().unwrap();
    let pid = i64::from(child.id());
    let request =
        py::obj([("kind", s("psutil")), ("repo", path_value(&repo_root())), ("pid", Value::Int(i128::from(pid)))]);
    let theirs = ask_python(&python, &request, &[]);
    let table = recog_desktop::system::table();
    let environ = table.environ(pid).unwrap();
    let probe = environ.iter().find(|(k, _)| k == "RECOG_DESKTOP_PROBE").map(|(_, v)| s(v)).unwrap_or(Value::Null);
    let mut listeners: Vec<Value> = table
        .tcp_listeners(pid)
        .unwrap()
        .into_iter()
        .map(|(host, port)| Value::Array(vec![s(&host), Value::Int(i128::from(port))]))
        .collect();
    listeners.sort_by_key(py::dumps);
    let mine = py::obj([
        ("name", s(&table.name(pid).unwrap())),
        ("uid", Value::Int(i128::from(table.uid_real(pid).unwrap()))),
        ("started", Value::Float(table.create_time(pid).unwrap())),
        ("exe", s(&table.exe(pid).unwrap())),
        ("cmdline", Value::Array(table.cmdline(pid).unwrap().into_iter().map(Value::from).collect())),
        ("probe", probe),
        ("environ_count", Value::Int(i128::try_from(environ.len()).unwrap())),
        ("listeners", Value::Array(listeners)),
    ]);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(py::dumps(&theirs), py::dumps(&mine));
    assert!(py::dumps(&mine).contains(&format!("[\"127.0.0.1\", {port}]")));
}
