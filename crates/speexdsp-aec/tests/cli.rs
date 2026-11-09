//! The `speex-aec` command line, run as a process.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use speexdsp_aec::{Config, EchoCanceller};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_speex-aec"))
}

/// A fresh case directory with `frames` frames of 8 samples.
fn case(name: &str, frames: usize, far_frames: usize, meta: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("speex-aec-cli-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let bytes =
        |count: usize| -> Vec<u8> { (0..count * 8).flat_map(|i| ((i as i16 * 97) % 3000).to_le_bytes()).collect() };
    fs::write(dir.join("meta.json"), meta).unwrap();
    fs::write(dir.join("mic.i16"), bytes(frames)).unwrap();
    fs::write(dir.join("ref.i16"), bytes(far_frames)).unwrap();
    dir
}

const META: &str = r#"{"frame_size": 8, "filter_length": 64}"#;

/// The samples of a 16-bit little-endian file.
fn samples(path: &Path) -> Vec<i16> {
    fs::read(path).unwrap().as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_case_runs_and_writes_its_output() {
    let dir = case("ok", 5, 5, META);
    let output = bin().arg(&dir).args(["--write", "out.i16", "--rate", "16000"]).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("\"cases\": 1") && report.contains("\"resets\": [0]"), "{report}");
    assert_eq!(fs::read(dir.join("out.i16")).unwrap().len(), 5 * 8 * 2);
    // without --write nothing is written
    fs::remove_file(dir.join("out.i16")).unwrap();
    assert!(bin().arg(&dir).output().unwrap().status.success());
    assert!(!dir.join("out.i16").exists());
}

#[test]
fn the_written_output_is_what_the_library_gives_for_the_same_samples() {
    let dir = case("same", 5, 5, META);
    let output = bin().arg(&dir).args(["--write", "out.i16", "--rate", "16000"]).output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let (mic, far) = (samples(&dir.join("mic.i16")), samples(&dir.join("ref.i16")));
    let mut aec = EchoCanceller::new(Config { sampling_rate: 16_000, ..Config::new(8, 64) }).unwrap();
    let mut expected = Vec::new();
    for (m, f) in mic.as_chunks::<8>().0.iter().zip(far.as_chunks::<8>().0.iter()) {
        let mut out = [0i16; 8];
        aec.cancel(m, f, &mut out).unwrap();
        expected.extend(out);
    }
    assert!(expected.iter().any(|&v| v != 0));
    assert_eq!(samples(&dir.join("out.i16")), expected);
}

#[test]
fn usage_errors_exit_with_2() {
    for args in [vec![], vec!["--bogus"], vec!["dir", "--write"], vec!["dir", "--rate"], vec!["dir", "--rate", "fast"]]
    {
        let output = bin().args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", stderr(&output));
        assert!(!stderr(&output).is_empty());
    }
}

#[test]
fn bad_cases_exit_with_1_and_say_why() {
    let short_ref = case("short", 3, 2, META);
    let no_key = case("nokey", 1, 1, r#"{"frame_size": 8}"#);
    let no_value = case("novalue", 1, 1, r#"{"frame_size" 8, "filter_length": 64}"#);
    let bad_value = case("badvalue", 1, 1, r#"{"frame_size": "x", "filter_length": 64}"#);
    let bad_config = case("badconfig", 1, 1, r#"{"frame_size": 19, "filter_length": 64}"#);
    let missing = std::env::temp_dir().join("speex-aec-cli-does-not-exist");
    let no_mic = case("nomic", 1, 1, META);
    fs::remove_file(no_mic.join("mic.i16")).unwrap();
    let unwritable = case("unwritable", 1, 1, META);
    fs::create_dir_all(unwritable.join("out.i16")).unwrap();
    let cases = [
        (short_ref, vec![], "shorter"),
        (no_key, vec![], "no filter_length"),
        (no_value, vec![], "no value"),
        (bad_value, vec![], "bad frame_size"),
        (bad_config, vec![], "prime factor"),
        (missing, vec![], "speex-aec-cli-does-not-exist"),
        (no_mic, vec![], "mic.i16"),
        (unwritable, vec!["--write", "out.i16"], "out.i16"),
    ];
    for (dir, extra, expected) in cases {
        let output = bin().arg(&dir).args(&extra).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", dir.display());
        assert!(stderr(&output).contains(expected), "{}: {}", dir.display(), stderr(&output));
    }
}
