//! `RecogZoomHelper.app`: the Swift helper built into its own signed bundle.
//!
//! macOS records the Accessibility permission under the signature of the application
//! responsible for the process, so the helper is its own bundle with a stable
//! identifier. An ad-hoc signature is the hash of the bundle: the bundle is rebuilt
//! only when the source's content changes, and is built byte for byte as
//! `recog/macos_bundle.py` builds it (the same compiler call, the same `Info.plist`
//! bytes, the same `codesign` call), so either implementation keeps the grant.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::log::Logger;
use crate::py;

pub const HELPER_APP: &str = "RecogZoomHelper.app";
/// What the permission is recorded under: it must not change.
pub const HELPER_BUNDLE_ID: &str = "com.recog.zoom-helper";
pub const HELPER_NAME: &str = "Recog Zoom Helper";
pub const HELPER_SOURCE: &str = "crates/recog-zoom/macos/zoom_ax_probe.swift";
/// Hash of the source the bundle was built from. Kept next to the bundle, not in it:
/// everything inside is sealed by the signature, so a stamp there would make any edit
/// of the source, even a comment, change the signature and lose the permission.
pub const HELPER_STAMP: &str = "RecogZoomHelper.source-sha256";
pub const HELPER_EXECUTABLE: &str = "zoom_ax_probe";
pub const HELPER_FRAMEWORKS: [&str; 2] = ["ApplicationServices", "AppKit"];

/// A bundle could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildError(pub String);

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BuildError {}

/// What a finished command reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub success: bool,
    pub stderr: String,
}

/// The compiler and the signing tool; tests replace them.
pub trait Toolchain {
    /// `shutil.which(name)`.
    fn which(&self, name: &str) -> Option<PathBuf>;
    /// Run a command to completion with its output captured.
    fn run(&self, command: &[OsString]) -> std::io::Result<CommandOutcome>;
}

/// The tools on `PATH`.
pub struct SystemToolchain;

impl Toolchain for SystemToolchain {
    fn which(&self, name: &str) -> Option<PathBuf> {
        which(name, std::env::var_os("PATH").as_deref())
    }

    fn run(&self, command: &[OsString]) -> std::io::Result<CommandOutcome> {
        let output = std::process::Command::new(&command[0]).args(&command[1..]).output()?;
        Ok(CommandOutcome { success: output.status.success(), stderr: String::from_utf8_lossy(&output.stderr).into() })
    }
}

/// `shutil.which`: an executable file in one of the `PATH` directories (`/bin:/usr/bin`
/// without `PATH`, the current directory for an empty entry).
pub fn which(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = path.map_or_else(|| OsString::from("/bin:/usr/bin"), OsString::from);
    std::env::split_paths(&path).find_map(|directory| {
        let directory = if directory.as_os_str().is_empty() { PathBuf::from(".") } else { directory };
        let candidate = directory.join(name);
        let metadata = std::fs::metadata(&candidate).ok()?;
        (!metadata.is_dir() && metadata.permissions().mode() & 0o111 != 0).then_some(candidate)
    })
}

/// The helper's `Info.plist` entries, sorted by key as `plistlib` writes them.
pub fn helper_info() -> Vec<(&'static str, PlistValue)> {
    vec![
        ("CFBundleDisplayName", PlistValue::String(HELPER_NAME)),
        ("CFBundleExecutable", PlistValue::String(HELPER_EXECUTABLE)),
        ("CFBundleIdentifier", PlistValue::String(HELPER_BUNDLE_ID)),
        ("CFBundleName", PlistValue::String(HELPER_NAME)),
        ("CFBundlePackageType", PlistValue::String("APPL")),
        ("CFBundleShortVersionString", PlistValue::String("1.0")),
        // No Dock icon, no menu bar: it only reads another application's tree.
        ("LSBackgroundOnly", PlistValue::Bool(true)),
    ]
}

/// A value of the flat dictionaries a bundle's `Info.plist` holds here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlistValue {
    String(&'static str),
    Bool(bool),
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The bytes `plistlib.dump(info, handle)` writes: XML, keys sorted, tab indentation.
/// They are sealed by the signature, so they must not differ by a byte.
pub fn info_plist(entries: &[(&str, PlistValue)]) -> String {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in sorted {
        let _ = writeln!(xml, "\t<key>{}</key>", xml_escape(key));
        match value {
            PlistValue::String(text) => {
                let _ = writeln!(xml, "\t<string>{}</string>", xml_escape(text));
            }
            PlistValue::Bool(flag) => xml.push_str(if flag { "\t<true/>\n" } else { "\t<false/>\n" }),
        }
    }
    xml.push_str("</dict>\n</plist>\n");
    xml
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn write_icon(contents: &Path, info: &[(&str, PlistValue)], icon: Option<&[u8]>) -> Result<(), BuildError> {
    let Some(bytes) = icon else { return Ok(()) };
    let name = info_string(info, "CFBundleIconFile").ok_or_else(|| BuildError("Missing CFBundleIconFile".into()))?;
    let resources = contents.join("Resources");
    std::fs::create_dir_all(&resources).map_err(|e| BuildError(format!("cannot create Resources: {e}")))?;
    std::fs::write(resources.join(name), bytes).map_err(|e| BuildError(format!("cannot write bundle icon: {e}")))
}

/// Compile `source` into the executable of `app`, write its `Info.plist` and sign it.
///
/// Built aside, in `<app>.build`, and swapped in on success: a failed build must not
/// take a working application away. `identity` `"-"` signs ad hoc.
pub fn build_app(
    app: &Path,
    source: &Path,
    info: &[(&str, PlistValue)],
    icon: Option<&[u8]>,
    frameworks: &[&str],
    identity: &str,
    toolchain: &impl Toolchain,
) -> Result<(), BuildError> {
    let fail = |what: &str, error: std::io::Error| BuildError(format!("{what}: {error}"));
    let staging = app.with_file_name(format!("{}.build", file_name(app)));
    let _ = std::fs::remove_dir_all(&staging);
    let contents = staging.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS")).map_err(|e| fail("cannot create the bundle", e))?;
    write_icon(&contents, info, icon)?;
    let executable_name = info_string(info, "CFBundleExecutable")
        .ok_or_else(|| BuildError("Info.plist names no CFBundleExecutable".into()))?;
    let executable = contents.join("MacOS").join(executable_name);
    let mut compile: Vec<OsString> =
        vec!["swiftc".into(), "-O".into(), "-o".into(), executable.into(), source.as_os_str().into()];
    for framework in frameworks {
        compile.extend(["-framework".into(), OsString::from(framework)]);
    }
    std::fs::write(contents.join("Info.plist"), info_plist(info)).map_err(|e| fail("cannot write Info.plist", e))?;
    // macOS recognizes the permission holder by its signature. "-" is an ad-hoc
    // signature, the hash of the bundle: it stays the same while the code does. A real
    // identity keeps the grant across code changes too.
    let bundle_id = info_string(info, "CFBundleIdentifier").unwrap_or_default();
    let sign: Vec<OsString> = vec![
        "codesign".into(),
        "--force".into(),
        "--sign".into(),
        identity.into(),
        "--identifier".into(),
        bundle_id.into(),
        staging.clone().into(),
    ];
    for command in [compile, sign] {
        run_build_step(toolchain, &command, app)?;
    }
    let _ = std::fs::remove_dir_all(app);
    std::fs::rename(&staging, app).map_err(|e| fail("cannot move the built bundle into place", e))
}

/// The text of a string entry `key` of `info`.
fn info_string(info: &[(&str, PlistValue)], key: &str) -> Option<&'static str> {
    info.iter().find_map(|(entry, value)| match value {
        PlistValue::String(text) if *entry == key => Some(*text),
        _ => None,
    })
}

/// Run one step of building `app`; a failed step names its tool and what it said.
fn run_build_step(toolchain: &impl Toolchain, command: &[OsString], app: &Path) -> Result<(), BuildError> {
    let tool = command[0].to_string_lossy().into_owned();
    let outcome = toolchain.run(command).map_err(|error| BuildError(format!("cannot run {tool}: {error}")))?;
    if !outcome.success {
        return Err(BuildError(format!("Failed to build {} ({tool}): {}", file_name(app), py::strip(&outcome.stderr))));
    }
    Ok(())
}

fn source_digest(source: &Path, icon: Option<&[u8]>) -> Result<String, BuildError> {
    let bytes = std::fs::read(source).map_err(|e| BuildError(format!("cannot read {}: {e}", source.display())))?;
    let mut hash = Sha256::new();
    hash.update(bytes);
    if let Some(icon) = icon {
        hash.update(b"\0");
        hash.update(icon);
    }
    Ok(hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The application; `build(app, source)` runs when it is missing or its source changed.
///
/// Freshness hashes the source and optional icon, independent of mtime, so a checkout that merely
/// touches the file does not rebuild. The stamp is written last: a build that failed
/// half-way is repeated on the next call. Without a compiler a stale application is
/// better than none.
pub fn find_or_build_app(
    app: &Path,
    source: &Path,
    stamp: &Path,
    icon: Option<&[u8]>,
    toolchain: &impl Toolchain,
    build: impl FnOnce(&Path, &Path) -> Result<(), BuildError>,
) -> Result<PathBuf, BuildError> {
    let digest = source_digest(source, icon)?;
    let built = app.join("Contents/_CodeSignature/CodeResources").exists();
    if built && stamp.exists() {
        let recorded =
            std::fs::read_to_string(stamp).map_err(|e| BuildError(format!("cannot read {}: {e}", stamp.display())))?;
        if py::strip(&recorded) == digest {
            return Ok(app.to_path_buf());
        }
    }
    if toolchain.which("swiftc").is_none() {
        if built {
            return Ok(app.to_path_buf());
        }
        return Err(BuildError(format!("swiftc not found — cannot build {} (xcode-select --install)", file_name(app))));
    }
    build(app, source)?;
    std::fs::write(stamp, digest).map_err(|e| BuildError(format!("cannot write {}: {e}", stamp.display())))?;
    Ok(app.to_path_buf())
}

/// The helper application under `repo_root`; built when missing or when its source
/// changed, signed with `identity` (`"-"` for ad hoc).
pub fn find_or_build_helper(
    repo_root: &Path,
    identity: &str,
    toolchain: &impl Toolchain,
    logger: &Logger,
) -> Result<PathBuf, BuildError> {
    let app = repo_root.join(HELPER_APP);
    find_or_build_app(
        &app,
        &repo_root.join(HELPER_SOURCE),
        &repo_root.join(HELPER_STAMP),
        None,
        toolchain,
        |app, source| {
            logger.line(&format!("Building {} from {}...", file_name(app), source.display()));
            build_app(app, source, &helper_info(), None, &HELPER_FRAMEWORKS, identity, toolchain)?;
            logger.line(&format!(
                "Built {}: allow \"{HELPER_NAME}\" in System Settings → Privacy & Security → Accessibility",
                file_name(app)
            ));
            Ok(())
        },
    )
}
