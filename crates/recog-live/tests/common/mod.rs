//! Shared by the integration tests: the interpreter of stub children, sample arrays
//! as base64.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

pub fn repo_root() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

/// The built `recog` placed at `program` as a file of its own (on APFS a clone, no
/// bytes copied). Not a hard link: every test's link would share one inode, and
/// macOS's execution policy, scanning a fresh build on its first runs, may read that
/// inode through another test's path; once that test has removed its directory the
/// scan fails and the waiting process is killed (status -1, "Terminating process due
/// to Malware rejection" in the system log).
pub fn place_recog(program: &Path) {
    fs::copy(env!("CARGO_BIN_EXE_recog"), program).unwrap();
}

/// An interpreter for the stub children of the process tests (plain stdlib
/// scripts): the executable behind `python3` on PATH, so that a version manager's
/// shim does not slow every child's start past the tests' timing. None skips them.
pub fn python() -> Option<PathBuf> {
    static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| {
            let found = std::process::Command::new("python3")
                .args(["-c", "import sys; print(sys.executable)"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()));
            if found.is_none() {
                eprintln!("process test skipped: no python3 on PATH for its stub children");
            }
            found
        })
        .clone()
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
