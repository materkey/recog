//! Shared by the golden-data tests: where the reference is, and how to read it.

use std::fs;
use std::path::{Path, PathBuf};

/// A directory of the golden data: under `SPEEXDSP_AEC_GOLDEN` if set (the mutation
/// workers run from an overlay elsewhere), else under `../golden` beside the crate.
///
/// # Panics
/// When it is missing: a parity test without its reference would pass having
/// checked nothing.
pub fn golden(sub: &str) -> PathBuf {
    let root = std::env::var_os("SPEEXDSP_AEC_GOLDEN")
        .map_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../golden"), PathBuf::from);
    let dir = root.join(sub);
    assert!(
        dir.is_dir(),
        "{} is missing: generate it with harness/gen_cases.py and harness/gen_fft_golden.py \
         (with pyaec's interpreter), or point SPEEXDSP_AEC_GOLDEN at a copy",
        dir.display()
    );
    dir
}

/// Little-endian int16 samples.
pub fn read_i16(path: &Path) -> Vec<i16> {
    fs::read(path).unwrap().chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}
