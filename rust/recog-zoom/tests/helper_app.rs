//! Building `RecogZoomHelper.app` with a stand-in compiler and signing tool. Ported
//! from the helper-application part of `tests/test_zoom_app.py`.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use recog_zoom::helper_app::{
    CommandOutcome, HELPER_SOURCE, HELPER_STAMP, Toolchain, find_or_build_helper, helper_info, info_plist, which,
};
use recog_zoom::log::Logger;

/// swiftc and codesign that write what the real ones would, and record their calls.
#[derive(Default)]
struct FakeToolchain {
    calls: RefCell<Vec<Vec<OsString>>>,
    fail: Option<&'static str>,
    no_compiler: bool,
}

impl FakeToolchain {
    fn failing(tool: &'static str) -> Self {
        Self { fail: Some(tool), ..Self::default() }
    }

    fn without_compiler() -> Self {
        Self { no_compiler: true, ..Self::default() }
    }

    fn tools(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|call| call[0].to_string_lossy().into_owned()).collect()
    }

    fn argument_after(&self, call: usize, flag: &str) -> String {
        let calls = self.calls.borrow();
        let position = calls[call].iter().position(|arg| arg == flag).unwrap();
        calls[call][position + 1].to_string_lossy().into_owned()
    }
}

impl Toolchain for FakeToolchain {
    fn which(&self, name: &str) -> Option<PathBuf> {
        (!self.no_compiler).then(|| PathBuf::from("/usr/bin").join(name))
    }

    fn run(&self, command: &[OsString]) -> std::io::Result<CommandOutcome> {
        self.calls.borrow_mut().push(command.to_vec());
        let tool = command[0].to_string_lossy();
        if self.fail == Some(&*tool) {
            return Ok(CommandOutcome { success: false, stderr: "boom\n".into() });
        }
        if tool == "swiftc" {
            let output = command.iter().position(|arg| arg == "-o").unwrap() + 1;
            std::fs::write(&command[output], "binary")?;
        }
        if tool == "codesign" {
            let signature = Path::new(command.last().unwrap()).join("Contents/_CodeSignature");
            std::fs::create_dir_all(&signature)?;
            std::fs::write(signature.join("CodeResources"), "seal")?;
        }
        Ok(CommandOutcome { success: true, stderr: String::new() })
    }
}

fn repo(source: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(HELPER_SOURCE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, source).unwrap();
    root
}

fn build(root: &Path, identity: &str, toolchain: &FakeToolchain) -> Result<PathBuf, String> {
    find_or_build_helper(root, identity, toolchain, &Logger::buffer().0).map_err(|error| error.0)
}

#[test]
fn given_no_helper_when_located_then_a_signed_bundle_with_a_stable_identifier_is_built() {
    let root = repo("// source");
    let toolchain = FakeToolchain::default();

    let app = build(root.path(), "-", &toolchain).unwrap();

    assert_eq!(app, root.path().join("RecogZoomHelper.app"));
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
    assert!(plist.contains("<key>CFBundleIdentifier</key>\n\t<string>com.recog.zoom-helper</string>"));
    assert!(plist.contains("<key>CFBundleName</key>\n\t<string>Recog Zoom Helper</string>"));
    assert!(plist.contains("<key>LSBackgroundOnly</key>\n\t<true/>"));
    assert_eq!(toolchain.tools(), ["swiftc", "codesign"]);
    // The stamp sits beside the bundle: inside, the signature would seal it.
    assert!(root.path().join(HELPER_STAMP).exists());
    assert!(!app.join("Contents/Resources").exists());
    assert_eq!(toolchain.argument_after(1, "--sign"), "-");
    assert_eq!(toolchain.argument_after(1, "--identifier"), "com.recog.zoom-helper");
}

#[test]
fn given_signing_identity_configured_when_built_then_the_bundle_is_signed_with_it() {
    let root = repo("// source");
    let toolchain = FakeToolchain::default();

    build(root.path(), "Developer ID Application: Example", &toolchain).unwrap();

    assert_eq!(toolchain.argument_after(1, "--sign"), "Developer ID Application: Example");
}

#[test]
fn given_built_helper_when_the_source_is_only_touched_then_it_is_not_rebuilt() {
    let root = repo("// source");
    let toolchain = FakeToolchain::default();
    build(root.path(), "-", &toolchain).unwrap();
    let source = std::fs::File::options().write(true).open(root.path().join(HELPER_SOURCE)).unwrap();

    // What a checkout does; the content is the same.
    source.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600)).unwrap();
    build(root.path(), "-", &toolchain).unwrap();

    assert_eq!(toolchain.calls.borrow().len(), 2); // a rebuild would lose the granted permission
}

#[test]
fn given_built_helper_when_the_source_content_changes_then_it_is_rebuilt() {
    let root = repo("// source");
    let toolchain = FakeToolchain::default();
    build(root.path(), "-", &toolchain).unwrap();

    std::fs::write(root.path().join(HELPER_SOURCE), "// changed").unwrap();
    build(root.path(), "-", &toolchain).unwrap();

    assert_eq!(toolchain.tools(), ["swiftc", "codesign", "swiftc", "codesign"]);
}

#[test]
fn given_stale_helper_and_no_compiler_when_located_then_the_stale_helper_is_used() {
    let root = repo("// source");
    let app = build(root.path(), "-", &FakeToolchain::default()).unwrap();
    std::fs::write(root.path().join(HELPER_SOURCE), "// changed").unwrap();

    assert_eq!(build(root.path(), "-", &FakeToolchain::without_compiler()).unwrap(), app);
}

#[test]
fn given_working_helper_when_a_rebuild_fails_then_the_working_helper_is_still_there() {
    let root = repo("// source");
    let app = build(root.path(), "-", &FakeToolchain::default()).unwrap();
    let binary = app.join("Contents/MacOS/zoom_ax_probe");
    std::fs::write(&binary, "working build").unwrap();
    std::fs::write(root.path().join(HELPER_SOURCE), "// changed").unwrap();

    let error = build(root.path(), "-", &FakeToolchain::failing("codesign")).unwrap_err();

    assert!(error.contains("codesign"), "{error}");
    assert_eq!(std::fs::read_to_string(binary).unwrap(), "working build");
}

#[test]
fn given_no_helper_and_no_compiler_when_located_then_error_names_the_fix() {
    let root = repo("// source");

    let error = build(root.path(), "-", &FakeToolchain::without_compiler()).unwrap_err();

    assert!(error.contains("xcode-select --install"), "{error}");
}

#[test]
fn given_a_build_step_fails_when_building_then_the_error_names_it_and_the_next_call_rebuilds() {
    for failing in ["swiftc", "codesign"] {
        let root = repo("// source");

        let error = build(root.path(), "-", &FakeToolchain::failing(failing)).unwrap_err();

        assert_eq!(error, format!("Failed to build RecogZoomHelper.app ({failing}): boom"));
        let toolchain = FakeToolchain::default();
        build(root.path(), "-", &toolchain).unwrap();
        assert_eq!(toolchain.tools(), ["swiftc", "codesign"]);
    }
}

#[test]
fn given_a_build_when_compiling_then_the_command_is_the_python_one() {
    let root = repo("// source");
    let toolchain = FakeToolchain::default();

    let app = build(root.path(), "-", &toolchain).unwrap();

    let staging = root.path().join("RecogZoomHelper.app.build");
    let expected: Vec<OsString> = vec![
        "swiftc".into(),
        "-O".into(),
        "-o".into(),
        staging.join("Contents/MacOS/zoom_ax_probe").into(),
        root.path().join(HELPER_SOURCE).into(),
        "-framework".into(),
        "ApplicationServices".into(),
        "-framework".into(),
        "AppKit".into(),
    ];
    assert_eq!(toolchain.calls.borrow()[0], expected);
    let sign: Vec<OsString> = ["codesign", "--force", "--sign", "-", "--identifier", "com.recog.zoom-helper"]
        .into_iter()
        .map(OsString::from)
        .chain([staging.into_os_string()])
        .collect();
    assert_eq!(toolchain.calls.borrow()[1], sign);
    assert!(app.exists());
}

/// `plistlib.dump(_helper_info(), handle)` of `recog/zoom_app.py`, byte for byte; the
/// bundle installed with the Python module carries exactly this file.
const PYTHON_INFO_PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
\t<key>CFBundleDisplayName</key>
\t<string>Recog Zoom Helper</string>
\t<key>CFBundleExecutable</key>
\t<string>zoom_ax_probe</string>
\t<key>CFBundleIdentifier</key>
\t<string>com.recog.zoom-helper</string>
\t<key>CFBundleName</key>
\t<string>Recog Zoom Helper</string>
\t<key>CFBundlePackageType</key>
\t<string>APPL</string>
\t<key>CFBundleShortVersionString</key>
\t<string>1.0</string>
\t<key>LSBackgroundOnly</key>
\t<true/>
</dict>
</plist>
";

#[test]
fn given_the_helper_info_when_written_then_the_bytes_are_plistlibs() {
    assert_eq!(info_plist(&helper_info()), PYTHON_INFO_PLIST);
}

fn tool_file(directory: &Path, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join("tool");
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[test]
fn given_path_directories_when_a_tool_is_looked_up_then_the_first_executable_file_wins() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("a/tool")).unwrap(); // a directory, not a tool
    tool_file(&root.path().join("b"), 0o644); // not executable
    let found = tool_file(&root.path().join("c"), 0o755);
    tool_file(&root.path().join("d"), 0o755);
    let dir = |name: &str| root.path().join(name).display().to_string();
    // The empty entry is the current directory, which holds no `tool`.
    let path = OsString::from(format!("{}:{}::{}:{}", dir("a"), dir("b"), dir("c"), dir("d")));

    assert_eq!(which("tool", Some(&path)), Some(found));
    assert_eq!(which("absent-tool", Some(&path)), None);
}

#[test]
fn given_no_path_when_a_tool_is_looked_up_then_bin_and_usr_bin_are_searched() {
    assert_eq!(which("sh", None), Some(PathBuf::from("/bin/sh")));
    assert_eq!(which("recog-no-such-tool", None), None);
}
