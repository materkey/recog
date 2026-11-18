//! Snapshot user data, keeping private files private even during the copy.

use recog_core::{data_dir, paths};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Backup {
    pub destination: PathBuf,
    pub remote: Option<String>,
    pub with_logs: bool,
}

impl Backup {
    pub fn new(home: &Path) -> Self {
        Self {
            destination: home.join("backups").join(format!("recog-{}", chrono::Local::now().format("%Y%m%d-%H%M"))),
            remote: None,
            with_logs: false,
        }
    }
}

fn total_bytes(path: &Path) -> io::Result<u64> {
    let mut size = 0;
    for item in fs::read_dir(path)? {
        let item = item?;
        let kind = item.file_type()?;
        if kind.is_dir() {
            size += total_bytes(&item.path())?;
        } else if kind.is_file() {
            size += item.metadata()?.len();
        }
    }
    Ok(size)
}

fn rsync(source: &Path, dest: &std::ffi::OsStr, remote: bool) -> io::Result<()> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let mut path = source.as_os_str().as_bytes().to_vec();
    path.push(b'/');
    let mut command = Command::new("rsync");
    command.arg("-a");
    if remote {
        command.arg("--partial");
    }
    let status = command.arg("--").arg(std::ffi::OsString::from_vec(path)).arg(dest).status()?;
    if status.success() { Ok(()) } else { Err(io::Error::other(format!("rsync failed: {status}"))) }
}

fn private_copy(source: &Path, dest: &Path) -> io::Result<()> {
    let mut input = fs::File::open(source)?;
    let mut temp = tempfile::NamedTempFile::new_in(dest.parent().expect("snapshot parent"))?;
    io::copy(&mut input, &mut temp)?;
    temp.persist(dest)?;
    Ok(())
}

pub fn snapshot(repo: &Path, data: &Path, config: &Path, opts: &Backup) -> io::Result<()> {
    let dest = paths::resolve(&opts.destination);
    let data = paths::resolve(data);
    if dest == data
        || dest == paths::resolve(repo)
        || ["meetings", "logs"].iter().any(|name| dest.starts_with(data.join(name)))
    {
        return Err(io::Error::other("Choose a backup destination outside the data being copied"));
    }
    paths::mkdir_parents(&dest, 0o700)?;
    for path in data_dir::data_entries(&data) {
        let name = path.file_name().expect("data entry");
        let target = dest.join(name);
        if path.is_dir() && (name == "meetings" || (name == "logs" && opts.with_logs)) {
            if target.is_symlink() {
                return Err(io::Error::other(format!("Refusing linked backup destination {}", target.display())));
            }
            rsync(&path, target.as_os_str(), false)?;
        } else if path.is_file() && name != "meetings" && name != "logs" {
            private_copy(&path, &target)?;
        }
    }
    for (src, name) in [
        (repo.join(".env"), "env.backup"),
        (repo.join("recog/web/.env"), "frontend-env.backup"),
        (config.to_owned(), "config.toml"),
    ] {
        if src.is_file() {
            private_copy(&src, &dest.join(name))?;
        }
    }
    if data.join("meetings").is_dir() {
        let source_bytes = total_bytes(&data.join("meetings"))?;
        let copied_bytes = total_bytes(&dest.join("meetings"))?;
        if source_bytes != copied_bytes {
            eprintln!("Meeting byte counts differ ({source_bytes} -> {copied_bytes}); retry after recording finishes");
        }
    }
    if let Some(remote) = &opts.remote {
        rsync(&dest, remote.as_ref(), true)?;
    }
    println!("Snapshot: {}", dest.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    #[test]
    fn snapshot_with_old_destination_when_updated_then_private_files_are_replaced_privately() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let data = dir.path().join("data");
        let dest = dir.path().join("backup");
        for p in [&repo, &data, &dest] {
            fs::create_dir_all(p).unwrap();
        }
        let config = repo.join("config.toml");
        fs::write(&config, "[test]").unwrap();
        fs::write(data.join("user-profile.json"), "private").unwrap();
        let original = dir.path().join("unrelated");
        fs::write(&original, "original").unwrap();
        std::os::unix::fs::symlink(&original, dest.join("user-profile.json")).unwrap();
        let opts = Backup { destination: dest.clone(), remote: None, with_logs: false };
        snapshot(&repo, &data, &config, &opts).unwrap();
        assert_eq!(fs::read_to_string(original).unwrap(), "original");
        let profile = dest.join("user-profile.json");
        assert_eq!(fs::metadata(&profile).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::read_to_string(profile).unwrap(), "private");
        assert_eq!(fs::read_to_string(dest.join("config.toml")).unwrap(), "[test]");
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn optional_settings_when_backed_up_then_confidential_files_are_private_and_logs_follow_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let data = dir.path().join("data");
        fs::create_dir_all(repo.join("recog/web")).unwrap();
        fs::create_dir_all(data.join("meetings")).unwrap();
        fs::create_dir_all(data.join("logs")).unwrap();
        let config = repo.join("config.toml");
        for (file, text) in [
            (repo.join(".env"), "fixture"),
            (repo.join("recog/web/.env"), "web"),
            (config.clone(), "config"),
            (data.join("speakers_custom.json"), "names"),
            (data.join("privacy.json"), "privacy"),
            (data.join("ai-settings.lock"), "lock"),
            (data.join("logs/live.log"), "log"),
            (data.join("meetings/transcript.jsonl"), "words"),
        ] {
            fs::write(file, text).unwrap();
        }
        for profile in [false, true] {
            for ai in [false, true] {
                if profile {
                    fs::write(data.join("user-profile.json"), "profile").unwrap();
                }
                if ai {
                    fs::write(data.join("ai-settings.json"), "ai").unwrap();
                }
                for logs in [false, true] {
                    let dest = dir.path().join(format!("backup-{profile}-{ai}-{logs}"));
                    snapshot(
                        &repo,
                        &data,
                        &config,
                        &Backup { destination: dest.clone(), remote: None, with_logs: logs },
                    )
                    .unwrap();
                    assert_eq!(dest.join("logs/live.log").is_file(), logs);
                    assert_eq!(dest.join("user-profile.json").is_file(), profile);
                    assert_eq!(dest.join("ai-settings.json").is_file(), ai);
                    assert!(!dest.join("ai-settings.lock").exists());
                    for (name, text) in [
                        ("meetings/transcript.jsonl", "words"),
                        ("env.backup", "fixture"),
                        ("frontend-env.backup", "web"),
                        ("config.toml", "config"),
                        ("privacy.json", "privacy"),
                        ("speakers_custom.json", "names"),
                    ] {
                        assert_eq!(fs::read_to_string(dest.join(name)).unwrap(), text);
                    }
                    for name in [
                        "env.backup",
                        "frontend-env.backup",
                        "config.toml",
                        "privacy.json",
                        "user-profile.json",
                        "ai-settings.json",
                    ] {
                        if dest.join(name).is_file() {
                            assert_eq!(fs::metadata(dest.join(name)).unwrap().mode() & 0o777, 0o600);
                        }
                    }
                }
                if ai {
                    fs::remove_file(data.join("ai-settings.json")).unwrap();
                }
            }
        }
        assert_eq!(fs::read_to_string(repo.join(".env")).unwrap(), "fixture");
        assert_eq!(fs::read_to_string(repo.join("recog/web/.env")).unwrap(), "web");
    }
}
