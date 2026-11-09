//! processes.rs on the inputs tests/differential.rs gave both implementations, with
//! the answers Python gave stated: selected paths, application bundles, `app.asar`
//! archives, fuse wires, recorded targets and candidate processes.
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use recog_core::pyjson::Value;
use recog_desktop::host::{Host, Platform};
use recog_desktop::processes::{self, FUSE_SENTINEL, Target};
use recog_desktop::py::{self, PyErr};

fn s(text: &str) -> Value {
    Value::from(text)
}

fn described<T: std::fmt::Debug>(result: Result<T, PyErr>) -> String {
    result.unwrap_err().described()
}

struct Fixture {
    dir: tempfile::TempDir,
    world: World,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let world = World::new(&dir.path().join("world"));
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self { dir, world, home }
    }

    /// The temporary directory, canonical, so that resolved paths compare equal.
    fn path(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    fn host(&self, platform: Platform) -> Host {
        let mut host = host(&self.world, free_port(), free_port(), &self.home);
        host.platform = platform;
        host
    }
}

/// A macOS bundle laid out on any system: `Info.plist` holding `info`, the main
/// executable `Contents/MacOS/<name>` and `Contents/Resources/app.asar` of KTalk.
fn bundle(dir: &Path, name: &str, info: &str) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    std::fs::write(app.join("Contents/Info.plist"), info).unwrap();
    std::fs::write(app.join("Contents/MacOS").join(name), "").unwrap();
    std::fs::write(app.join("Contents/Resources/app.asar"), asar(KTALK_PACKAGE)).unwrap();
    app
}

fn info_plist(entries: &str) -> String {
    format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>{entries}</dict></plist>")
}

const KTALK_PACKAGE: &str = r#"{"name": "ktalk", "main": "build/main.js"}"#;

// --- selected application --------------------------------------------------------

#[test]
fn selected_paths_when_resolved_on_linux_then_python_answers() {
    // given
    let fixture = Fixture::new();
    let base = fixture.path();
    let host = fixture.host(Platform::Linux);
    let app = fake_app(&base, "FakeTalk").app;
    std::os::unix::fs::symlink(&app, base.join("link")).unwrap();
    std::fs::write(base.join("plain-file"), "").unwrap();
    std::os::unix::fs::symlink(base.join("loop-b"), base.join("loop-a")).unwrap();
    std::os::unix::fs::symlink(base.join("loop-a"), base.join("loop-b")).unwrap();
    std::fs::create_dir_all(fixture.home.join("Apps")).unwrap();
    std::os::unix::fs::symlink(&app, fixture.home.join("Apps/FakeTalk")).unwrap();
    let wrong = "ValueError: Select an executable KTalk AppImage or binary";
    let cases: Vec<(String, Result<PathBuf, String>)> = vec![
        (app.to_string_lossy().into_owned(), Ok(app.clone())),
        (format!("{}/", app.display()), Ok(app.clone())),
        (format!("{}/../FakeTalk", app.display()), Ok(app.clone())),
        (base.join("link").to_string_lossy().into_owned(), Ok(app.clone())),
        ("~/Apps/FakeTalk".to_owned(), Ok(app.clone())),
        (base.join("plain-file").to_string_lossy().into_owned(), Err(wrong.to_owned())),
        (base.to_string_lossy().into_owned(), Err(wrong.to_owned())),
        ("~".to_owned(), Err(wrong.to_owned())),
        (String::new(), Err(wrong.to_owned())),
        ("/".to_owned(), Err(wrong.to_owned())),
        (
            base.join("missing/deeper/x.app").to_string_lossy().into_owned(),
            Err(format!(
                "FileNotFoundError: [Errno 2] No such file or directory: '{}'",
                base.join("missing").display()
            )),
        ),
        (
            base.join("loop-a").to_string_lossy().into_owned(),
            Err(format!("RuntimeError: Symlink loop from '{}'", base.join("loop-a").display())),
        ),
    ];
    for (raw, expected) in cases {
        // when
        let result = processes::application_path(&host, &raw).map_err(|e| e.described());
        // then
        assert_eq!(result, expected, "{raw}");
    }
}

#[test]
fn selected_bundles_when_resolved_on_macos_then_python_answers() {
    // given
    let fixture = Fixture::new();
    let base = fixture.path();
    let host = fixture.host(Platform::Darwin);
    let entry = |key: &str, value: &str| format!("<key>{key}</key>{value}");
    let identifier = entry("CFBundleIdentifier", "<string>kontur.talk</string>");
    let valid = bundle(&base, "KTalk", &plist("kontur.talk", "KTalk"));
    let wrong = bundle(&base, "Wrong", &plist("other", "Wrong"));
    let numbered = bundle(&base, "Numbered", &info_plist(&entry("CFBundleIdentifier", "<integer>1</integer>")));
    let missing = bundle(&base, "NoExe", &plist("kontur.talk", "Missing"));
    let broken = bundle(&base, "Broken", "not a plist");
    let array = bundle(&base, "Array", "<?xml version=\"1.0\"?><plist version=\"1.0\"><array/></plist>");
    let no_key = bundle(&base, "NoKey", &info_plist(&identifier));
    let int_exe = bundle(
        &base,
        "IntExe",
        &info_plist(&format!("{identifier}{}", entry("CFBundleExecutable", "<integer>5</integer>"))),
    );
    let bare = base.join("Bare.app");
    std::fs::create_dir_all(&bare).unwrap();
    let official = "ValueError: Select the official KTalk application bundle";
    let cases: Vec<(&Path, Result<PathBuf, String>)> = vec![
        (&valid, Ok(valid.clone())),
        (&wrong, Err(official.to_owned())),
        (&numbered, Err(official.to_owned())),
        (&missing, Err("ValueError: Application executable is missing".to_owned())),
        (&broken, Err("InvalidFileException: Invalid file".to_owned())),
        (&array, Err("AttributeError: 'list' object has no attribute 'get'".to_owned())),
        (&no_key, Err("KeyError: 'CFBundleExecutable'".to_owned())),
        (&int_exe, Err("TypeError: unsupported operand type(s) for /: 'PosixPath' and 'int'".to_owned())),
        (
            &bare,
            Err(format!(
                "FileNotFoundError: [Errno 2] No such file or directory: '{}'",
                bare.join("Contents/Info.plist").display()
            )),
        ),
    ];
    for (app, expected) in cases {
        // when
        let result = processes::application_path(&host, &app.to_string_lossy()).map_err(|e| e.described());
        // then
        assert_eq!(result, expected, "{}", app.display());
    }
}

#[test]
fn another_operating_system_when_application_selected_then_refused() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host(Platform::Other("win32".into()));
    let app = fake_app(&fixture.path(), "FakeTalk").app;
    // when / then
    assert_eq!(
        described(processes::application_path(&host, &app.to_string_lossy())),
        "ValueError: Desktop integration supports Linux and macOS"
    );
}

#[test]
fn relative_paths_and_parents_when_resolved_strictly_then_like_realpath() {
    // given: the tests run in the crate directory
    let cwd = std::env::current_dir().unwrap();
    let fixture = Fixture::new();
    let base = fixture.path();
    std::fs::create_dir_all(base.join("d/e")).unwrap();
    std::os::unix::fs::symlink(base.join("d"), base.join("link")).unwrap();
    // when / then
    assert_eq!(processes::resolve_strict(Path::new("Cargo.toml")).unwrap(), cwd.join("Cargo.toml"));
    assert_eq!(processes::resolve_strict(Path::new("..")).unwrap(), cwd.parent().unwrap());
    assert_eq!(processes::resolve_strict(Path::new("../..")).unwrap(), cwd.parent().unwrap().parent().unwrap());
    assert_eq!(processes::resolve_strict(&base.join("d/./e/..//e")).unwrap(), base.join("d/e"));
    // the link resolved once, then again from its resolution
    assert_eq!(processes::resolve_strict(&base.join("link/../link/e")).unwrap(), base.join("d/e"));
    assert_eq!(processes::resolve_strict(&base.join("link/e/../../link")).unwrap(), base.join("d"));
}

// --- the packaged application ------------------------------------------------------

#[test]
fn asar_archives_when_read_then_python_answers() {
    // given
    let fixture = Fixture::new();
    let base = fixture.path();
    let host = fixture.host(Platform::Linux);
    let body = KTALK_PACKAGE;
    let len = body.len().to_string();
    let package = |size: &str, offset: &str| {
        format!(r#"{{"files": {{"package.json": {{"size": {size}, "offset": {offset}}}}}}}"#)
    };
    let header = |tree: &str, declared: Option<u32>| {
        let tree = tree.as_bytes();
        let size = declared.unwrap_or(u32::try_from(tree.len()).unwrap());
        let mut out = Vec::new();
        for word in [4u32, 8 + u32::try_from(tree.len()).unwrap(), 4, size] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(tree);
        out.extend_from_slice(body.as_bytes());
        out
    };
    let cases: Vec<(&str, Vec<u8>, Result<bool, String>)> = vec![
        ("ktalk", asar(body), Ok(true)),
        ("impostor", asar(r#"{"name": "impostor", "main": "build/main.js"}"#), Ok(false)),
        ("no main", asar(r#"{"name": "ktalk"}"#), Ok(false)),
        ("list", asar("[1, 2]"), Err("AttributeError: 'list' object has no attribute 'get'".into())),
        ("text", asar("\"text\""), Err("AttributeError: 'str' object has no attribute 'get'".into())),
        ("broken", asar("{broken"), Ok(false)),
        ("truncated", b"truncated".to_vec(), Ok(false)),
        ("empty", Vec::new(), Ok(false)),
        ("no tree", header(&package(&len, "\"0\""), Some(0)), Ok(false)),
        ("huge tree", header(&package(&len, "\"0\""), Some(9 * 1024 * 1024)), Ok(false)),
        ("spaced offset", header(&package(&len, "\" 0 \""), None), Ok(true)),
        ("float size", header(&package(&format!("{len}.9"), "0"), None), Ok(true)),
        ("underscored offset", header(&package(&format!("\"{len}\""), "\"1_0\""), None), Ok(false)),
        ("text size", header(&package("\"abc\"", "0"), None), Ok(false)),
        (
            "infinite size",
            header(&package("Infinity", "0"), None),
            Err("OverflowError: cannot convert float infinity to integer".into()),
        ),
        ("negative position", header(&package(&len, "-100"), None), Ok(false)),
        ("zero size", header(&package("0", "0"), None), Ok(false)),
        ("big package", header(&package("70000", "0"), None), Ok(false)),
        ("null offset", header(&package(&len, "null"), None), Ok(false)),
        ("files list", header(r#"{"files": []}"#, None), Ok(false)),
        ("package text", header(r#"{"files": {"package.json": "x"}}"#, None), Ok(false)),
        ("tree list", header("[]", None), Ok(false)),
    ];
    for (i, (name, contents, expected)) in cases.into_iter().enumerate() {
        let dir = base.join(format!("asar-{i}"));
        std::fs::create_dir_all(dir.join("resources")).unwrap();
        std::fs::write(dir.join("resources/app.asar"), contents).unwrap();
        // when
        let result = processes::is_ktalk_executable(&host, &dir.join("ktalk")).map_err(|e| e.described());
        // then
        assert_eq!(result, expected, "{name}");
    }
    assert!(!processes::is_ktalk_executable(&host, &base.join("asar-missing/ktalk")).unwrap());
}

#[test]
fn macos_executable_when_read_then_archive_beside_the_bundle_contents() {
    // given
    let fixture = Fixture::new();
    let app = bundle(&fixture.path(), "KTalk", &plist("kontur.talk", "KTalk"));
    let executable = app.join("Contents/MacOS/KTalk");
    // when / then
    assert!(processes::is_ktalk_executable(&fixture.host(Platform::Darwin), &executable).unwrap());
    assert!(!processes::is_ktalk_executable(&fixture.host(Platform::Linux), &executable).unwrap());
}

#[test]
fn fuse_wires_when_inspected_then_python_answers() {
    // given
    let fixture = Fixture::new();
    let binary = fixture.path().join("Electron Framework");
    let enabled = fuse_wire(true, 1, 8);
    let cases: Vec<(&str, Vec<u8>, bool)> = vec![
        ("version 2", fuse_wire(true, 2, 8), false),
        ("three fuses", fuse_wire(true, 1, 3), false),
        ("four fuses", fuse_wire(true, 1, 4), true),
        ("three architectures", enabled.repeat(3), false),
        ("end of a chunk", [vec![b'x'; 65536 - 20], enabled.clone()].concat(), true),
        ("across a chunk", [vec![b'x'; 65536 - 33], enabled.clone(), vec![b'y'; 100]].concat(), true),
        ("short wire", [b"prefix".to_vec(), FUSE_SENTINEL.to_vec(), b"\x01\x08000".to_vec()].concat(), false),
        ("no length", [b"prefix".to_vec(), FUSE_SENTINEL.to_vec(), b"\x01".to_vec()].concat(), false),
        ("sentinel only", FUSE_SENTINEL.to_vec(), false),
    ];
    for (name, wire, expected) in cases {
        // when
        std::fs::write(&binary, &wire).unwrap();
        // then
        assert_eq!(processes::inspect_fuses(&binary).unwrap(), expected, "{name}");
    }
    assert_eq!(
        described(processes::inspect_fuses(&fixture.path().join("missing"))),
        format!(
            "FileNotFoundError: [Errno 2] No such file or directory: '{}'",
            fixture.path().join("missing").display()
        )
    );
}

// --- targets -------------------------------------------------------------------------

#[test]
fn recorded_targets_when_read_then_python_conversions() {
    // given
    type Expected = Result<(i64, f64, &'static str), &'static str>;
    let cases: Vec<(&str, Expected)> = vec![
        (r#"{"pid": 42, "started": 12.0, "executable": "/ktalk"}"#, Ok((42, 12.0, "/ktalk"))),
        (r#"{"pid": "42", "started": "12.5", "executable": 7}"#, Ok((42, 12.5, "7"))),
        (r#"{"pid": 42.9, "started": 12, "executable": null}"#, Ok((42, 12.0, "None"))),
        (r#"{"pid": true, "started": false, "executable": [1]}"#, Ok((1, 0.0, "[1]"))),
        (r#"{"pid": 1, "started": " 1_0.5 ", "executable": "/x"}"#, Ok((1, 10.5, "/x"))),
        (r#"{"pid": 42, "started": 12.0}"#, Err("KeyError: 'executable'")),
        (r#"{"started": 12.0, "executable": "/x"}"#, Err("KeyError: 'pid'")),
        (
            r#"{"pid": "x", "started": 1, "executable": "/x"}"#,
            Err("ValueError: invalid literal for int() with base 10: 'x'"),
        ),
        (
            r#"{"pid": null, "started": 1, "executable": "/x"}"#,
            Err("TypeError: int() argument must be a string, a bytes-like object or a real number, not 'NoneType'"),
        ),
        (
            r#"{"pid": 1, "started": [], "executable": "/x"}"#,
            Err("TypeError: float() argument must be a string or a real number, not 'list'"),
        ),
        (
            r#"{"pid": 99999999999999999999, "started": 1, "executable": "/x"}"#,
            Err("OverflowError: Python int too large to convert"),
        ),
        ("[1]", Err("TypeError: list indices must be integers or slices, not str")),
        ("\"text\"", Err("TypeError: string indices must be integers, not 'str'")),
        ("null", Err("TypeError: 'NoneType' object is not subscriptable")),
    ];
    for (data, expected) in cases {
        // when
        let target = Target::from_value(&obj(data));
        // then
        let expected =
            expected.map(|(pid, started, executable)| Target { pid, started, executable: executable.into() });
        assert_eq!(target.map_err(|e| e.described()), expected.map_err(str::to_owned), "{data}");
    }
    let nan = Target::from_value(&obj(r#"{"pid": 1, "started": "nan", "executable": "/x"}"#)).unwrap();
    assert!(nan.started.is_nan());
}

fn process(pid: i64, name: &str, exe: &Path) -> Value {
    py::obj([
        ("pid", Value::Int(i128::from(pid))),
        ("name", s(name)),
        ("started", Value::Float(12.0)),
        ("exe", s(&exe.to_string_lossy())),
        ("cmdline", Value::Array(vec![s(name)])),
    ])
}

fn with(mut process: Value, key: &str, value: Value) -> Value {
    process.as_object_mut().unwrap().insert(key, value);
    process
}

fn pids(targets: &[Target]) -> Vec<i64> {
    targets.iter().map(|t| t.pid).collect()
}

#[test]
fn candidate_processes_when_searched_then_only_the_selected_main_processes() {
    // given
    let fixture = Fixture::new();
    let base = fixture.path();
    let host = fixture.host(Platform::Linux);
    let app = fake_app(&base, "FakeTalk");
    let unpackaged = base.join("unpackaged/ktalk");
    std::fs::create_dir_all(unpackaged.parent().unwrap()).unwrap();
    std::fs::write(&unpackaged, "").unwrap();
    let other_uid = Value::Int(i128::from(host.uid) + 1);
    fixture.world.set(&world_of(vec![
        process(40, "bash", &app.executable),
        process(41, "FakeTalk", &app.executable),
        with(process(42, "ktalk", &app.executable), "uid", other_uid),
        with(process(43, "ktalk", &app.executable), "cmdline", obj(r#"["ktalk", "--type=renderer"]"#)),
        process(44, "ktalk", &unpackaged),
        with(process(45, "ktalk", &app.executable), "errors", obj(r#"{"uid": "AccessDenied"}"#)),
        with(process(46, "Толк", &app.executable), "errors", obj(r#"{"name": "AccessDenied"}"#)),
        with(process(47, "ktalk", &app.executable), "errors", obj(r#"{"name": "NoSuchProcess"}"#)),
        with(process(48, "Толк", &app.executable), "errors", obj(r#"{"open": "NoSuchProcess"}"#)),
        process(49, "Толк", &app.executable),
    ]));
    // when
    let targets = processes::find_targets(&host, &app.app).unwrap();
    // then
    assert_eq!(pids(&targets), [41, 49]);
    assert_eq!(targets[0], Target { pid: 41, started: 12.0, executable: app.executable.to_string_lossy().into() });
}

#[test]
fn ktalk_process_with_a_malformed_package_when_searched_then_the_error_raised() {
    // given
    let fixture = Fixture::new();
    let base = fixture.path();
    let host = fixture.host(Platform::Linux);
    let app = fake_app(&base, "FakeTalk");
    std::fs::write(base.join("FakeTalk/resources/app.asar"), asar("[1]")).unwrap();
    fixture.world.set(&world_of(vec![process(41, "ktalk", &app.executable)]));
    // when / then
    assert_eq!(
        described(processes::find_targets(&host, &app.app)),
        "AttributeError: 'list' object has no attribute 'get'"
    );
}

#[test]
fn macos_main_process_when_searched_then_only_the_bundle_executable() {
    // given
    let fixture = Fixture::new();
    let base = fixture.path();
    let host = fixture.host(Platform::Darwin);
    let app = bundle(&base, "KTalk", &plist("kontur.talk", "KTalk"));
    let copy = bundle(&base.join("copy"), "KTalk", &plist("kontur.talk", "KTalk"));
    fixture.world.set(&world_of(vec![
        process(41, "KTalk", &app.join("Contents/MacOS/KTalk")),
        process(42, "KTalk", &copy.join("Contents/MacOS/KTalk")),
        process(43, "KTalk.app", &app.join("Contents/MacOS/KTalk")),
    ]));
    // when
    let targets = processes::find_targets(&host, &app).unwrap();
    // then
    assert_eq!(pids(&targets), [41, 43]);
    let target = &targets[0];
    assert!(processes::same_process(&host, target, Some(&app)).unwrap());
    assert!(!processes::same_process(&host, target, Some(&copy)).unwrap());
}

#[test]
fn recorded_process_when_its_identity_changed_then_not_the_same() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host(Platform::Linux);
    let target = Target { pid: 42, started: 12.0, executable: "/ktalk".into() };
    let cases = [
        (process(42, "ktalk", Path::new("/ktalk")), true),
        (with(process(42, "ktalk", Path::new("/ktalk")), "uid", Value::Int(i128::from(host.uid) + 1)), false),
        (with(process(42, "ktalk", Path::new("/ktalk")), "started", Value::Float(13.0)), false),
        (process(42, "ktalk", Path::new("/other")), false),
        (with(process(42, "ktalk", Path::new("/ktalk")), "errors", obj(r#"{"exe": "AccessDenied"}"#)), false),
        (process(43, "ktalk", Path::new("/ktalk")), false),
    ];
    for (entry, expected) in cases {
        // when
        fixture.world.set(&world_of(vec![entry.clone()]));
        // then
        assert_eq!(processes::same_process(&host, &target, None).unwrap(), expected, "{}", py::dumps(&entry));
    }
}

#[test]
fn fuse_runtime_unreadable_or_malformed_when_matched_then_skipped() {
    // given: three runtimes of the selected file, none of which can be read through
    let fixture = Fixture::new();
    let base = fixture.path();
    let mut host = fixture.host(Platform::Linux);
    let app = base.join("KTalk.AppImage");
    std::fs::write(&app, "").unwrap();
    let runtime = |pid: i64| process(pid, "KTalk", &app);
    fixture.world.set(&world_of(vec![
        with(runtime(901), "errors", obj(r#"{"uid": "AccessDenied"}"#)),
        runtime(902),
        runtime(903),
        runtime(904),
        process(905, "", &app),
    ]));
    for pid in [902, 903] {
        std::fs::create_dir_all(base.join(format!("proc/{pid}/fdinfo"))).unwrap();
    }
    std::fs::write(base.join("proc/902/fdinfo/7"), b"fuse_connection:\t\xff\n").unwrap();
    std::fs::write(base.join("proc/903/fdinfo/7"), "fuse_connection:\tx\n").unwrap();
    host.proc_root = base.join("proc");
    host.device = Arc::new(|_| Ok(processes::makedev(0, 17)));
    // when
    let matched = processes::fuse_source_matches(&host, Path::new("/mounted/ktalk"), &app);
    // then: 904 has no fdinfo at all
    assert!(!matched.unwrap());
    // another connection, then also the one of the mount, in a second descriptor
    std::fs::write(base.join("proc/903/fdinfo/7"), "pos:\t0\nfuse_connection:\t18\n").unwrap();
    assert!(!processes::fuse_source_matches(&host, Path::new("/mounted/ktalk"), &app).unwrap());
    std::fs::write(base.join("proc/903/fdinfo/8"), "fuse_connection:\t17\n").unwrap();
    assert!(processes::fuse_source_matches(&host, Path::new("/mounted/ktalk"), &app).unwrap());
}

#[test]
fn unreadable_mount_when_fuse_source_checked_then_its_error() {
    // given
    let fixture = Fixture::new();
    let host = fixture.host(Platform::Linux);
    let missing = fixture.path().join("missing");
    // when / then
    assert_eq!(
        described(processes::fuse_source_matches(&host, &missing, Path::new("/KTalk.AppImage"))),
        format!("FileNotFoundError: [Errno 2] No such file or directory: '{}'", missing.display())
    );
}

#[test]
fn appimage_process_without_a_source_when_matched_then_by_its_fuse_runtime_only() {
    // given: a mounted `ktalk` whose APPIMAGE names another file
    let fixture = Fixture::new();
    let base = fixture.path();
    let mut host = fixture.host(Platform::Linux);
    let app = base.join("KTalk.AppImage");
    std::fs::write(&app, "").unwrap();
    let mounted = fake_app(&base.join("mount"), "ktalk");
    let entry = with(process(42, "ktalk", &mounted.executable), "environ", obj(r#"{"APPIMAGE": "/elsewhere"}"#));
    fixture.world.set(&world_of(vec![entry]));
    host.device = Arc::new(|_| Ok(processes::makedev(0, 17)));
    // when / then
    assert!(!processes::matches(&host, 42, &app).unwrap());
    let unpackaged = with(process(42, "ktalk", &base.join("nothing")), "environ", obj("{}"));
    fixture.world.set(&world_of(vec![unpackaged]));
    assert!(!processes::matches(&host, 42, &app).unwrap());
}
