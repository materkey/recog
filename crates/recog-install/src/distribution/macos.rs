use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use rustix::fs::{CWD, RenameFlags, renameat_with};

fn check_signature(app: &Path, identifier: &str) -> io::Result<()> {
    let result = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "-R"])
        .arg(format!("=identifier \"{identifier}\""))
        .arg(app)
        .output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{}: {}", app.display(), String::from_utf8_lossy(&result.stderr))))
    }
}

fn install_app(source: &Path, dest: &Path, name: &str, identifier: &str) -> io::Result<()> {
    let bundle = format!("{name}.app");
    let stamp = format!("{name}.source-sha256");
    let incoming = source.join(&bundle);
    let installed = dest.join(&bundle);
    let digest = fs::read(source.join(&stamp))?;
    let plist = "Contents/Info.plist";
    let info = fs::read(incoming.join(plist))?;
    check_signature(&incoming, identifier)?;
    // A previous release may have compiled the same sources locally, producing
    // a different valid signature. Keep that exact bundle and its grants.
    if fs::read(dest.join(&stamp)).ok().as_ref() == Some(&digest)
        && fs::read(installed.join(plist)).ok().as_ref() == Some(&info)
        && check_signature(&installed, identifier).is_ok()
    {
        return Ok(());
    }

    let stage = tempfile::tempdir_in(dest)?;
    let staged = stage.path().join(&bundle);
    let copy = Command::new("/usr/bin/ditto").arg("--qtn").arg(&incoming).arg(&staged).status()?;
    if !copy.success() {
        return Err(io::Error::other(format!("Cannot install {bundle}: ditto exited with {copy}")));
    }
    check_signature(&staged, identifier)?;
    // An interrupted update must never leave an old stamp describing a new
    // bundle. The directory exchange keeps the entire signed app intact.
    match fs::remove_file(dest.join(&stamp)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        result => result?,
    }
    if installed.try_exists()? {
        renameat_with(CWD, &staged, CWD, &installed, RenameFlags::EXCHANGE)?;
    } else {
        fs::rename(&staged, &installed)?;
    }
    super::copy_tree(&source.join(&stamp), &dest.join(&stamp))
}

pub(super) fn install_apps(source: &Path, dest: &Path) -> io::Result<()> {
    install_app(source, dest, "Recog", "com.recog.live")?;
    install_app(source, dest, "RecogZoomHelper", recog_zoom::helper_app::HELPER_BUNDLE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn signed_app(root: &Path, resource: &[u8], stamp: &str) -> std::path::PathBuf {
        let app = root.join("Recog.app");
        let contents = app.join("Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        fs::create_dir_all(contents.join("Resources")).unwrap();
        fs::copy("/usr/bin/true", contents.join("MacOS/launcher")).unwrap();
        fs::write(contents.join("Resources/test"), resource).unwrap();
        fs::write(contents.join("Info.plist"), r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.recog.live</string>
<key>CFBundlePackageType</key><string>APPL</string><key>CFBundleExecutable</key><string>launcher</string></dict></plist>"#).unwrap();
        assert!(Command::new("/usr/bin/codesign").args(["-f", "-s", "-"]).arg(&app).status().unwrap().success());
        fs::write(root.join("Recog.source-sha256"), stamp).unwrap();
        app
    }

    #[test]
    fn given_a_locally_signed_app_when_upgraded_then_only_a_valid_changed_release_replaces_it() {
        let local = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        let app = signed_app(local.path(), b"previous local build", "unchanged");
        let incoming = signed_app(package.path(), b"release build", "unchanged");
        let inode = || fs::metadata(app.join("Contents/MacOS/launcher")).unwrap().ino();
        let original = inode();
        let install = || install_app(package.path(), local.path(), "Recog", "com.recog.live");
        install().unwrap();
        assert_eq!(inode(), original);
        fs::write(package.path().join("Recog.source-sha256"), "updated").unwrap();
        fs::write(incoming.join("Contents/Resources/test"), "damaged").unwrap();
        assert!(install().is_err());
        assert_eq!(inode(), original);
        signed_app(package.path(), b"new release", "updated");
        install().unwrap();
        let updated = inode();
        assert_ne!(updated, original);
        assert_eq!(fs::read(app.join("Contents/Resources/test")).unwrap(), b"new release");
        install().unwrap();
        assert_eq!(inode(), updated);
    }
}
