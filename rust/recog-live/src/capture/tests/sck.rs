//! The Swift helper cases of tests/test_system_audio.py.
//!
//! Not ported: `repo_root_points_at_the_repository_not_the_package` (the root is an
//! argument here; the caller passes it).

use std::cell::RefCell;
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::capture::sck::{SckError, Toolchain, find_or_build_sck_utility};
use crate::clock::ManualClock;
use crate::log::Logger;

struct FakeTools {
    swiftc: bool,
    result: (i32, &'static str),
    rebuild: Option<PathBuf>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl Toolchain for FakeTools {
    fn which(&self, name: &str) -> Option<PathBuf> {
        self.swiftc.then(|| PathBuf::from("/usr/bin").join(name))
    }

    fn run(&self, argv: &[String]) -> (i32, String) {
        assert!(self.swiftc, "swiftc must not be executed when it is absent");
        self.calls.borrow_mut().push(argv.to_vec());
        if let Some(path) = &self.rebuild {
            fs::write(path, b"rebuilt").unwrap();
        }
        (self.result.0, self.result.1.to_owned())
    }
}

fn tools(swiftc: bool, result: (i32, &'static str), rebuild: Option<PathBuf>) -> FakeTools {
    FakeTools { swiftc, result, rebuild, calls: RefCell::new(Vec::new()) }
}

/// A prebuilt helper older than its source.
fn stale(root: &Path) -> (PathBuf, PathBuf) {
    let binary = root.join("sck_audio_capture");
    let source = root.join("recog/native/macos/sck_audio_capture.swift");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&binary, b"prebuilt").unwrap();
    fs::write(&source, "// newer than the binary").unwrap();
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    File::options().write(true).open(&binary).unwrap().set_times(FileTimes::new().set_modified(old)).unwrap();
    (binary, source)
}

fn log() -> (Logger, crate::log::Captured) {
    Logger::capture(ManualClock::new(0.0).shared())
}

#[test]
fn helper_at_the_root_find_or_build_gives_its_path() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("sck_audio_capture");
    fs::write(&binary, b"").unwrap();
    let path = find_or_build_sck_utility(root.path(), &tools(false, (0, ""), None), &log().0).unwrap();
    assert_eq!(path, binary.display().to_string());
}

#[test]
fn neither_helper_nor_source_find_or_build_fails_naming_the_helper() {
    let root = tempfile::tempdir().unwrap();
    let e = find_or_build_sck_utility(root.path(), &tools(false, (0, ""), None), &log().0).unwrap_err();
    assert!(matches!(&e, SckError::NotFound(m) if m.contains("sck_audio_capture")));
}

#[test]
fn stale_helper_find_or_build_recompiles_it_from_the_source_at_the_root() {
    let root = tempfile::tempdir().unwrap();
    let (binary, source) = stale(root.path());
    let t = tools(true, (0, ""), Some(binary.clone()));
    let (log, captured) = log();
    assert_eq!(find_or_build_sck_utility(root.path(), &t, &log).unwrap(), binary.display().to_string());
    let calls = t.calls.borrow();
    assert_eq!(
        calls[0][..4],
        ["swiftc".to_owned(), "-o".into(), binary.display().to_string(), source.display().to_string()]
    );
    assert_eq!(captured.lines(), vec![format!("[screencapture] Compiling {}...", source.display())]);
}

#[test]
fn stale_helper_without_swiftc_find_or_build_reuses_it() {
    let root = tempfile::tempdir().unwrap();
    let (binary, _) = stale(root.path());
    let (log, captured) = log();
    assert_eq!(
        find_or_build_sck_utility(root.path(), &tools(false, (0, ""), None), &log).unwrap(),
        binary.display().to_string()
    );
    let text = captured.text();
    assert!(text.contains("reusing the existing helper"));
    assert!(!text.contains("no prebuilt helper"));
}

#[test]
fn stale_helper_with_a_broken_swiftc_find_or_build_reuses_it_untouched() {
    let root = tempfile::tempdir().unwrap();
    let (binary, _) = stale(root.path());
    let t = tools(true, (1, "error: cannot load module 'ScreenCaptureKit'"), None);
    let (log, captured) = log();
    assert_eq!(find_or_build_sck_utility(root.path(), &t, &log).unwrap(), binary.display().to_string());
    assert_eq!(fs::read(&binary).unwrap(), b"prebuilt");
    assert!(captured.text().contains(
        "[screencapture] Failed to compile utility: error: cannot load module 'ScreenCaptureKit'; reusing the existing helper"
    ));
}

#[test]
fn source_only_without_swiftc_find_or_build_names_swiftc() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("recog/native/macos/sck_audio_capture.swift");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "// source only").unwrap();
    let e = find_or_build_sck_utility(root.path(), &tools(false, (0, ""), None), &log().0).unwrap_err();
    let SckError::MissingSwiftc(m) = e else { panic!("{e:?}") };
    assert!(m.contains("swiftc not found"));
    assert!(m.contains("no prebuilt helper to fall back on"));
}

#[test]
fn a_program_that_fails_when_the_os_toolchain_runs_it_then_its_exit_code_and_stderr() {
    // given
    let argv: Vec<String> = ["sh", "-c", "echo broken SDK >&2; exit 3"].map(str::to_owned).to_vec();
    // when
    let failed = crate::capture::sck::OsToolchain.run(&argv);
    let nothing = crate::capture::sck::OsToolchain.run(&[]);
    let missing = crate::capture::sck::OsToolchain.run(&["/nonexistent/swiftc".to_owned()]);
    // then
    assert_eq!(failed, (3, "broken SDK\n".to_owned()));
    assert_eq!(nothing, (-1, String::new()));
    assert_eq!(missing.0, -1);
    assert!(!missing.1.is_empty());
}

#[test]
fn programs_on_path_and_off_it_when_the_os_toolchain_looks_them_up_then_found_or_none() {
    // given / when
    let sh = crate::capture::sck::OsToolchain.which("sh");
    let none = crate::capture::sck::OsToolchain.which("no-such-program-of-recog");
    // then
    assert!(sh.as_ref().is_some_and(|p| p.ends_with("sh") && p.is_absolute()), "{sh:?}");
    assert_eq!(none, None);
}
