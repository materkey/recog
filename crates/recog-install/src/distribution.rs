//! An archive has its own resource root. Installed services, permission-bearing
//! helpers and the unpacked extension need user-owned paths stable across upgrades.
use std::path::{Path, PathBuf};
use std::{fs, io};

#[cfg(target_os = "macos")]
mod macos;

pub fn runtime(home: &Path) -> PathBuf {
    home.join(".local/share/recog/runtime")
}
pub fn is_release(root: &Path) -> bool {
    root.join("recog-release").is_file()
}

fn copy_tree(source: &Path, dest: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.is_dir() {
        fs::create_dir_all(dest)?;
        for item in fs::read_dir(source)? {
            let item = item?;
            copy_tree(&item.path(), &dest.join(item.file_name()))?;
        }
    } else if meta.is_file() {
        let parent = dest.parent().ok_or_else(|| io::Error::other("No installation directory"))?;
        fs::create_dir_all(parent)?;
        let tmp = tempfile::NamedTempFile::new_in(parent)?;
        fs::copy(source, tmp.path())?;
        tmp.persist(dest)?;
    } else {
        return Err(io::Error::other(format!("Unexpected release entry: {}", source.display())));
    }
    Ok(())
}

/// Only declared program resources are installed. Local settings and recordings
/// belong to the user.
pub fn prepare(source: &Path, dest: &Path, extension: bool) -> io::Result<()> {
    if source == dest {
        return Ok(());
    }
    fs::create_dir_all(dest)?;
    let lock = fs::File::open(dest)?;
    lock.lock()?;
    #[cfg(target_os = "macos")]
    macos::install_apps(source, dest)?;
    for path in [
        "bin",
        "crates/recog-web/static",
        "crates/recog-ai/prompts",
        "crates/speexdsp-aec/LICENSE",
        "crates/recog-live/macos",
        "crates/recog-zoom/macos",
        ".claude",
        "CHANGELOG.md",
        "LICENSE",
        ".env.example",
    ] {
        copy_tree(&source.join(path), &dest.join(path))?;
    }
    if cfg!(target_os = "macos") {
        copy_tree(&source.join("sck_audio_capture"), &dest.join("sck_audio_capture"))?;
    }
    if extension {
        copy_tree(&source.join("browser-extension/dist"), &dest.join("browser-extension/dist"))?;
    }
    if source.join("recog-homebrew").is_file() {
        copy_tree(&source.join("recog-homebrew"), &dest.join("recog-homebrew"))?;
    }
    copy_tree(&source.join("recog-release"), &dest.join("recog-release"))?;
    Ok(())
}

/// The checked-in manifest contains built-in hosts. Custom matches are scoped
/// exactly like the checkout build, including Element's call iframe.
pub fn configure_extension(root: &Path, jitsi: &[String], matrix: &[String]) -> io::Result<()> {
    use serde_json::{Value, json};
    let mut manifest: Value = serde_json::from_str(include_str!("../../../browser-extension/manifest.json"))?;
    let jitsi: Vec<_> = jitsi.iter().map(|h| format!("{h}/*")).collect();
    let matrix_frames: Vec<_> = matrix.iter().map(|h| format!("{h}/widgets/element-call/*")).collect();
    manifest["host_permissions"].as_array_mut().unwrap().extend(jitsi.iter().chain(&matrix_frames).map(|s| json!(s)));
    manifest["content_scripts"][0]["matches"].as_array_mut().unwrap().extend(jitsi.iter().map(|s| json!(s)));
    if !matrix.is_empty() {
        manifest["content_scripts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"js":["content.js"],"all_frames":true,"matches":matrix_frames,"run_at":"document_idle"}));
    }
    manifest["web_accessible_resources"][0]["matches"]
        .as_array_mut()
        .unwrap()
        .extend(jitsi.iter().map(|s| json!(s)).chain(matrix.iter().map(|h| json!(format!("{h}/*")))));
    let dir = root.join("browser-extension/dist");
    let temp = tempfile::NamedTempFile::new_in(&dir)?;
    serde_json::to_writer_pretty(temp.as_file(), &manifest)?;
    temp.persist(dir.join("manifest.json"))?;
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn given_an_archive_when_installed_twice_then_resources_and_notices_are_kept_with_the_runtime() {
        let package = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let root = runtime(home.path());
        for path in [
            "bin/recog",
            "crates/recog-web/static/index.html",
            "crates/recog-ai/prompts/test",
            "crates/recog-live/macos/recog_launcher.swift",
            "crates/recog-zoom/macos/zoom_ax_probe.swift",
            "crates/speexdsp-aec/LICENSE",
            ".claude/agents/copilot-session.md",
            "CHANGELOG.md",
            "LICENSE",
            ".env.example",
            "browser-extension/dist/manifest.json",
            "recog-homebrew",
            "recog-release",
        ] {
            let file = package.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, path).unwrap();
        }
        prepare(package.path(), &root, false).unwrap();
        assert!(!root.join("browser-extension/dist").exists());
        prepare(package.path(), &root, true).unwrap();
        assert!(is_release(&root));
        assert!(root.join("browser-extension/dist/manifest.json").is_file());
        assert_eq!(fs::read(root.join("crates/speexdsp-aec/LICENSE")).unwrap(), b"crates/speexdsp-aec/LICENSE");
        assert!(!root.join("Recog.app").exists());
        prepare(&root, &root, true).unwrap();
    }
}
