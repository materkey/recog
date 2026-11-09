//! The ScreenCaptureKit helper `sck_audio_capture`: found at the repository root, or
//! compiled from `recog/native/macos/sck_audio_capture.swift`, in place of
//! `find_or_build_sck_utility`.
//!
//! A helper older than its source is rebuilt; when that fails for any reason (no
//! swiftc, a broken SDK) the existing helper is reused with a rate-limited notice,
//! since a stale helper beats a live service in a crash loop. Pure file logic, built
//! and tested on every system; only running the helper needs macOS.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::log::Logger;

/// Why there is no helper to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SckError {
    /// `MissingSwiftc`: a build is needed and swiftc is not on PATH.
    MissingSwiftc(String),
    /// `RuntimeError`: swiftc failed.
    CompileFailed(String),
    /// `FileNotFoundError`: neither the helper nor its source exists.
    NotFound(String),
}

impl fmt::Display for SckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SckError::MissingSwiftc(m) | SckError::CompileFailed(m) | SckError::NotFound(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for SckError {}

/// Finding and running swiftc (injectable, as Python's `shutil.which`/`subprocess.run`).
pub trait Toolchain {
    fn which(&self, name: &str) -> Option<PathBuf>;
    /// Runs `argv`; its exit code and stderr.
    fn run(&self, argv: &[String]) -> (i32, String);
}

/// PATH and real processes.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsToolchain;

impl Toolchain for OsToolchain {
    fn which(&self, name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| {
            use std::os::unix::fs::PermissionsExt as _;
            candidate.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    }

    fn run(&self, argv: &[String]) -> (i32, String) {
        let Some((program, args)) = argv.split_first() else { return (-1, String::new()) };
        match Command::new(program).args(args).output() {
            Ok(out) => (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned()),
            Err(e) => (-1, e.to_string()),
        }
    }
}

fn compile(
    utility: &Path,
    source: &Path,
    prebuilt: bool,
    tools: &dyn Toolchain,
    log: &Logger,
) -> Result<String, SckError> {
    if tools.which("swiftc").is_none() {
        let detail = if prebuilt {
            String::new()
        } else {
            format!(", and no prebuilt helper to fall back on at {}", utility.display())
        };
        return Err(SckError::MissingSwiftc(format!(
            "swiftc not found — cannot build sck_audio_capture (xcode-select --install){detail}"
        )));
    }
    log.line(&format!("[screencapture] Compiling {}...", source.display()));
    let argv: Vec<String> = [
        "swiftc",
        "-o",
        &utility.display().to_string(),
        &source.display().to_string(),
        "-framework",
        "ScreenCaptureKit",
        "-framework",
        "CoreMedia",
        "-framework",
        "AVFoundation",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let (code, stderr) = tools.run(&argv);
    if code == 0 && utility.exists() {
        return Ok(utility.display().to_string());
    }
    Err(SckError::CompileFailed(format!("Failed to compile utility: {stderr}")))
}

/// The path of `sck_audio_capture` under `repo_root`, compiled when missing or older
/// than its source.
///
/// # Errors
/// No helper and no way to build one ([`SckError`]).
pub fn find_or_build_sck_utility(repo_root: &Path, tools: &dyn Toolchain, log: &Logger) -> Result<String, SckError> {
    let utility = repo_root.join("sck_audio_capture");
    let source = repo_root.join("recog/native/macos/sck_audio_capture.swift");
    if utility.exists() {
        if source.exists() {
            let stale =
                match (source.metadata().and_then(|m| m.modified()), utility.metadata().and_then(|m| m.modified())) {
                    (Ok(s), Ok(u)) => s > u,
                    _ => true,
                };
            if stale && let Err(e) = compile(&utility, &source, true, tools, log) {
                log.rate_limited("sck-helper-stale", &format!("[screencapture] {e}; reusing the existing helper"));
            }
        }
        return Ok(utility.display().to_string());
    }
    if source.exists() {
        return compile(&utility, &source, false, tools, log);
    }
    Err(SckError::NotFound(format!("sck_audio_capture utility not found. Expected at: {}", utility.display())))
}
