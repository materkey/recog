//! Shared by the differential tests: running the Python implementation through one of
//! the scripts under `tests/parity/`, and moving sample arrays across as base64.
#![allow(dead_code)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value as Json;

pub fn repo_root() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

/// The interpreter of the Python side: `RECOG_LIVE_PARITY_PYTHON` (scripts/check.sh
/// sets it, and then the tests must run), else the repository's venv. None skips.
pub fn python() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RECOG_LIVE_PARITY_PYTHON") {
        return Some(PathBuf::from(path));
    }
    let venv = repo_root().join("venv-gigaam/bin/python");
    if venv.exists() {
        return Some(venv);
    }
    eprintln!("differential test skipped: no venv-gigaam/bin/python and RECOG_LIVE_PARITY_PYTHON is not set");
    None
}

/// Runs `tests/parity/<script>` with the repository root as its first argument and
/// `request` as JSON on stdin, and returns the JSON it prints.
///
/// HOME, the installation configuration and the data directory point into `sandbox`
/// (a temp directory), so nothing reads or writes the user's real settings or data.
pub fn run_python(python: &Path, script: &str, request: &Json, sandbox: &Path) -> Json {
    let output = run_python_raw(python, script, &[], request, sandbox);
    serde_json::from_slice(&output)
        .unwrap_or_else(|e| panic!("{script} printed no JSON ({e}):\n{}", String::from_utf8_lossy(&output)))
}

/// As [`run_python`] with extra arguments after the repository root, returning stdout.
pub fn run_python_raw(python: &Path, script: &str, args: &[&str], request: &Json, sandbox: &Path) -> Vec<u8> {
    fs::create_dir_all(sandbox.join("home")).unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity").join(script);
    let mut child = Command::new(python)
        .arg(&path)
        .arg(repo_root())
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", sandbox.join("home"))
        .env("RECOG_CONFIG_FILE", sandbox.join("no-config.toml"))
        .env("RECOG_CONFERENCE_HOSTS_FILE", sandbox.join("no-hosts.json"))
        .env("RECOG_DATA_DIR", sandbox.join("data"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONUTF8", "1")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", python.display()));
    child.stdin.take().unwrap().write_all(request.to_string().as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{script} failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn unb64(text: &str) -> Vec<u8> {
    STANDARD.decode(text).unwrap()
}

/// float32 samples as base64 of their little-endian bytes (`np.frombuffer(..., "<f4")`).
pub fn f32_b64(samples: &[f32]) -> String {
    b64(&samples.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>())
}

pub fn f32_from_b64(text: &str) -> Vec<f32> {
    unb64(text).as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// float64 samples as base64 of their little-endian bytes (`"<f8"`).
pub fn f64_b64(samples: &[f64]) -> String {
    b64(&samples.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>())
}

pub fn f64_from_b64(text: &str) -> Vec<f64> {
    unb64(text).as_chunks::<8>().0.iter().map(|b| f64::from_le_bytes(*b)).collect()
}

/// Int16 samples as base64 of their little-endian bytes (`"<i2"`).
pub fn i16_b64(samples: &[i16]) -> String {
    b64(&samples.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>())
}

pub fn i16_from_b64(text: &str) -> Vec<i16> {
    unb64(text).as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

/// A deterministic generator for test input (xorshift64*), the same on every host.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in [lo, hi).
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % (hi - lo) as u64) as i64
    }
}
