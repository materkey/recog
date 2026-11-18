//! Update an unpacked release from the same assets the Homebrew formula uses.
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const REPO: &str = "github.com/materkey/recog";

/// gh owns authentication and redirect handling. Bound stdout before decoding
/// metadata or unpacking an asset, including when the remote sends no length.
fn gh(args: &[&str], limit: u64) -> Result<Vec<u8>> {
    let mut child = Command::new("gh")
        .args(["release"])
        .args(args)
        .args(["--repo", REPO])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot run gh ({e}). Install GitHub CLI and use gh auth login."))?;
    let mut bytes = Vec::new();
    let result = child.stdout.take().unwrap().take(limit + 1).read_to_end(&mut bytes);
    if result.is_err() || bytes.len() as u64 > limit {
        let _ = child.kill();
        let _ = child.wait();
        result?;
        return Err("Release asset exceeds its size limit".into());
    }
    if !child.wait()?.success() {
        return Err("Could not read the private Recog release. Check gh auth status and repository access.".into());
    }
    Ok(bytes)
}

fn asset(tag: &str, name: &str, limit: u64) -> Result<Vec<u8>> {
    gh(&["download", tag, "--pattern", name, "--output", "-"], limit)
}

pub(super) fn update(root: &Path, args: &[String]) -> Result<()> {
    let latest = gh(&["view", "--json", "tagName", "--jq", ".tagName"], 1024)?;
    let tag = std::str::from_utf8(&latest)?.trim();
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
    let sums = asset(tag, "SHA256SUMS", 1 << 20)?;
    let hash = std::str::from_utf8(&sums)?
        .lines()
        .find_map(|line| {
            let (hash, file) = line.split_once(' ')?;
            (file.trim().trim_start_matches("./") == name && hash.len() == 64).then_some(hash)
        })
        .ok_or("Release checksum is missing")?;
    let archive = asset(tag, &name, 256 << 20)?;
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
