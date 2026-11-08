//! Shared by the parity tests: running the Python implementation through
//! `tests/parity/harness.py`, and comparing what both sides wrote.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use recog_core::pyjson::{self, Value};
use serde_json::{Value as Json, json};

pub fn repo_root() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

/// The interpreter of the Python side: `RECOG_PARITY_PYTHON` (scripts/check.sh sets
/// it, and then the tests must run), else the repository's venv. None skips.
pub fn python() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RECOG_PARITY_PYTHON") {
        return Some(PathBuf::from(path));
    }
    let venv = repo_root().join("venv-gigaam/bin/python");
    if venv.exists() {
        return Some(venv);
    }
    eprintln!("parity test skipped: no venv-gigaam/bin/python and RECOG_PARITY_PYTHON is not set");
    None
}

/// Run one harness module on `request`. `sandbox` holds the HOME, configuration and
/// data paths the interpreter sees, all temporary.
pub fn run_python(python: &Path, module: &str, request: &Json, sandbox: &Path, tz: &str) -> Json {
    fs::create_dir_all(sandbox.join("home")).unwrap();
    let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/harness.py");
    let mut child = Command::new(python)
        .arg(&harness)
        .arg(repo_root())
        .arg(module)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", sandbox.join("home"))
        .env("RECOG_CONFIG_FILE", sandbox.join("no-config.toml"))
        .env("RECOG_CONFERENCE_HOSTS_FILE", sandbox.join("no-hosts.json"))
        .env("RECOG_DATA_DIR", sandbox.join("data"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONUTF8", "1")
        .env("TZ", tz)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", python.display()));
    child.stdin.take().unwrap().write_all(request.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "the Python harness failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // RECOG_PARITY_DUMP=<dir> keeps each request and Python's answer for a look.
    if let Some(dir) = std::env::var_os("RECOG_PARITY_DUMP") {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir).unwrap();
        let name = sandbox
            .parent()
            .and_then(Path::file_name)
            .map_or_else(|| module.to_owned(), |n| n.to_string_lossy().into_owned());
        fs::write(dir.join(format!("{module}-{name}.request.json")), request.to_string()).unwrap();
        fs::write(dir.join(format!("{module}-{name}.answer.json")), &output.stdout).unwrap();
    }
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A pyjson value as serde_json sees it, for comparing with the harness's answer.
pub fn to_json(value: &Value) -> Json {
    serde_json::from_str(&pyjson::dumps(value, None)).unwrap()
}

/// serde_json to pyjson, for handing the same request to the port.
pub fn from_json(value: &Json) -> Value {
    pyjson::loads(&value.to_string()).unwrap()
}

/// Every file and directory under `root`: its mode and, for a file, its bytes with
/// `root` itself written as `<root>` (each side ran in its own directory).
pub fn snapshot(root: &Path) -> BTreeMap<String, (u32, String)> {
    let mut out = BTreeMap::new();
    let prefix = root.to_string_lossy().into_owned();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let mode = meta.permissions().mode() & 0o7777;
            if meta.is_dir() {
                out.insert(format!("{rel}/"), (mode, String::new()));
                stack.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                let text = match String::from_utf8(bytes) {
                    Ok(text) => text.replace(&prefix, "<root>"),
                    Err(e) => format!("bytes:{:?}", e.into_bytes()),
                };
                out.insert(rel, (mode, text));
            }
        }
    }
    out
}

/// `text` with every occurrence of each root replaced by `<root>`.
pub fn unroot(value: &Json, roots: &[&Path]) -> Json {
    let mut text = value.to_string();
    for root in roots {
        let escaped = serde_json::to_string(&root.to_string_lossy()).unwrap();
        text = text.replace(escaped.trim_matches('"'), "<root>");
    }
    serde_json::from_str(&text).unwrap()
}

/// Both sides printed in full when they differ.
pub fn difference(what: &str, python: &Json, rust: &Json) -> Option<String> {
    (python != rust).then(|| {
        format!(
            "{what} differs\n--- python\n{}\n--- rust\n{}",
            serde_json::to_string_pretty(python).unwrap(),
            serde_json::to_string_pretty(rust).unwrap()
        )
    })
}

/// Assert equality with both sides printed in full on failure.
pub fn assert_same(what: &str, python: &Json, rust: &Json) {
    if let Some(message) = difference(what, python, rust) {
        panic!("{message}");
    }
}

/// Fail with every collected difference.
pub fn assert_none(failures: &[String]) {
    assert!(failures.is_empty(), "{} difference(s):\n{}", failures.len(), failures.join("\n\n"));
}

pub fn error(kind: &str, message: impl Into<String>) -> Json {
    json!({"error": [kind, message.into()]})
}
