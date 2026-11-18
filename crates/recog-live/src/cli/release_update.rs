//! Update an unpacked release from the same assets the Homebrew formula uses.
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const PROJECT: &str = "https://github.com/materkey/recog/releases";
const LATEST: &str = "https://api.github.com/repos/materkey/recog/releases/latest";

fn download(url: &str, limit: u64) -> Result<Vec<u8>> {
    let agent =
        ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(120))).build().new_agent();
    let mut response = agent.get(url).header("User-Agent", "recog-update").call()?;
    let mut bytes = Vec::new();
    response.body_mut().as_reader().take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("Release asset exceeds its size limit".into());
    }
    Ok(bytes)
}

pub(super) fn update(root: &Path, args: &[String]) -> Result<()> {
    let metadata = recog_core::pyjson::loads(std::str::from_utf8(&download(LATEST, 2 << 20)?)?)?;
    let tag =
        metadata.as_object().and_then(|m| m.get("tag_name")).and_then(|v| v.as_str()).ok_or("Missing release tag")?;
    if !tag.starts_with('v') || !tag.bytes().all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b)) {
        return Err("Invalid release tag".into());
    }
    if fs::read_to_string(root.join("recog-release"))?.trim() == tag {
        println!("Already on {tag}");
        return Ok(());
    }
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-arm64",
        ("linux", "x86_64") => "linux-x86_64",
        _ => return Err("No release archive for this platform".into()),
    };
    let name = format!("recog-{tag}-{platform}.tar.gz");
    let base = format!("{PROJECT}/download/{tag}");
    let sums = download(&format!("{base}/SHA256SUMS"), 1 << 20)?;
    let hash = std::str::from_utf8(&sums)?
        .lines()
        .find_map(|line| {
            let (hash, file) = line.split_once(' ')?;
            (file.trim().trim_start_matches("./") == name && hash.len() == 64).then_some(hash)
        })
        .ok_or("Release checksum is missing")?;
    let archive = download(&format!("{base}/{name}"), 256 << 20)?;
    if format!("{:x}", Sha256::digest(&archive)) != hash {
        return Err("Release checksum mismatch".into());
    }
    let staging = tempfile::tempdir()?;
    let path = staging.path().join(name);
    fs::write(&path, archive)?;
    let result = Command::new("tar").arg("-xzf").arg(path).arg("-C").arg(staging.path()).status()?;
    if !result.success() {
        return Err(io::Error::other("Could not unpack the verified release").into());
    }
    let next = staging.path().join("recog");
    if fs::read_to_string(next.join("recog-release"))?.trim() != tag {
        return Err("Archive version does not match release".into());
    }
    let outcome = Command::new(next.join("bin/recog")).arg("setup").args(args).status()?;
    if !outcome.success() {
        return Err(format!("Release setup exited with {outcome}").into());
    }
    Ok(())
}
