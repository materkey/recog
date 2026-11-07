//! Bit-exact parity with pyaec (speexdsp 1.2.1 fixed point) on the harness cases.
//!
//! `golden/cases/seed_NNN/expected.i16` is pyaec's output for `mic.i16` and
//! `ref.i16`, produced by `harness/gen_cases.py`; see `common::golden` for where it
//! is looked up. Missing data fails the test.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{golden, read_i16};
use speexdsp_aec::{Config, EchoCanceller};

fn golden_cases() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(golden("cases"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("expected.i16").exists())
        .collect();
    dirs.sort();
    dirs
}

fn meta_value(meta: &str, key: &str) -> usize {
    let at = meta.find(&format!("\"{key}\"")).unwrap() + key.len() + 2;
    meta[at..].trim_start_matches([':', ' ']).chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap()
}

/// First (frame, sample, expected, got) where the outputs differ.
fn first_mismatch(dir: &Path) -> Option<(usize, usize, i16, i16)> {
    let meta = fs::read_to_string(dir.join("meta.json")).unwrap();
    let (frame, tail) = (meta_value(&meta, "frame_size"), meta_value(&meta, "filter_length"));
    let (mic, far, expected) = (read_i16(&dir.join("mic.i16")), read_i16(&dir.join("ref.i16")), read_i16(&dir.join("expected.i16")));
    let mut aec = EchoCanceller::new(Config::new(frame, tail)).unwrap();
    let mut out = vec![0i16; frame];
    for (k, (m, f)) in mic.chunks_exact(frame).zip(far.chunks_exact(frame)).enumerate() {
        aec.cancel(m, f, &mut out).unwrap();
        let want = &expected[k * frame..(k + 1) * frame];
        if let Some(i) = (0..frame).find(|&i| out[i] != want[i]) {
            return Some((k, i, want[i], out[i]));
        }
    }
    None
}

#[test]
fn every_golden_case_is_bit_exact_with_pyaec() {
    let cases = golden_cases();
    assert!(!cases.is_empty(), "no case under golden/cases has an expected.i16: run harness/gen_cases.py");
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|dir| {
            first_mismatch(dir).map(|(k, i, want, got)| format!("{}: frame {k} sample {i}: pyaec {want}, port {got}", dir.display()))
        })
        .collect();
    assert!(failures.is_empty(), "{} of {} cases diverge:\n{}", failures.len(), cases.len(), failures.join("\n"));
}
