//! The pinned GigaAM v4 weights. wavo's hub entry names a different repository.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

struct PinnedModel {
    repo: &'static str,
    revision: &'static str,
    file: &'static str,
    size: u64,
    sha256: &'static str,
}

const V4: PinnedModel = PinnedModel {
    repo: "70sh1/gigaam-v4-e2e-rnnt-gguf",
    revision: "2a312d783963ca3433768281faea00cdcf2569c5",
    file: "gigaam-v4-e2e-rnnt-Q8_0.gguf",
    size: 255_076_352,
    sha256: "debd6179b9948beba5c60c0944285e577f732610dd50dee1233884e4b46e3e29",
};

fn verified(path: &Path, size: u64, sha256: &str) -> io::Result<bool> {
    let mut file = match File::open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        other => other?,
    };
    if file.metadata()?.len() != size {
        return Ok(false);
    }
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()) == sha256)
}

pub(super) fn cached(root: &Path) -> io::Result<Option<PathBuf>> {
    V4.cached(root)
}

pub(super) fn download(root: &Path) -> io::Result<PathBuf> {
    let endpoint = std::env::var("HF_ENDPOINT").unwrap_or_else(|_| "https://huggingface.co".into());
    V4.download(root, &endpoint)
}

impl PinnedModel {
    fn repo_folder(&self) -> String {
        format!("models--{}", self.repo.replace('/', "--"))
    }

    fn cached(&self, root: &Path) -> io::Result<Option<PathBuf>> {
        let snapshot = root.join(self.repo_folder()).join("snapshots").join(self.revision).join(self.file);
        Ok(verified(&snapshot, self.size, self.sha256)?.then_some(snapshot))
    }

    /// Hold Hugging Face's per-blob lock until the verified file is linked into its pinned snapshot.
    fn download(&self, root: &Path, endpoint: &str) -> io::Result<PathBuf> {
        let folder = self.repo_folder();
        let _lock = lock_blob(&root.join(".locks").join(&folder), self.sha256)?;
        let repo = root.join(folder);
        let blobs = repo.join("blobs");
        let blob = blobs.join(self.sha256);
        if !verified(&blob, self.size, self.sha256)? {
            self.download_blob(&blobs, &blob, endpoint)?;
        }
        link_snapshot(&repo.join("snapshots").join(self.revision), self.file, self.sha256)
    }

    fn download_blob(&self, blobs: &Path, blob: &Path, endpoint: &str) -> io::Result<()> {
        fs::create_dir_all(blobs)?;
        let mut tmp = tempfile::NamedTempFile::new_in(blobs)?;
        let url = format!("{}/{}/resolve/{}/{}", endpoint.trim_end_matches('/'), self.repo, self.revision, self.file);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(30)))
            .build()
            .into();
        let response = agent.get(&url).call().map_err(|e| io::Error::other(format!("{url}: {e}")))?;
        io::copy(&mut response.into_body().into_reader().take(self.size + 1), &mut tmp)?;
        if !verified(tmp.path(), self.size, self.sha256)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: expected {} bytes with SHA-256 {}", self.file, self.size, self.sha256),
            ));
        }
        tmp.persist(blob)?;
        Ok(())
    }
}

fn lock_blob(locks: &Path, sha256: &str) -> io::Result<File> {
    fs::create_dir_all(locks)?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(locks.join(format!("{sha256}.lock")))?;
    lock.lock()?;
    Ok(lock)
}

fn link_snapshot(dir: &Path, file: &str, sha256: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let tmp = tempfile::tempdir_in(dir)?;
    let link = tmp.path().join(file);
    std::os::unix::fs::symlink(Path::new("../../blobs").join(sha256), &link)?;
    let path = dir.join(file);
    fs::rename(link, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    const FIXTURE: PinnedModel = PinnedModel {
        repo: "test/weights",
        revision: "abc123",
        file: "weights.gguf",
        size: 5,
        sha256: "9372c470eeadd5ecd9c3c74c2b3cb633f8e2f2fad799250a0f70d652b6b825e4",
    };

    fn serve_once(bytes: &'static [u8]) -> (String, thread::JoinHandle<()>) {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", server.local_addr().unwrap());
        let handler = thread::spawn(move || {
            let (mut client, _) = server.accept().unwrap();
            let mut lines = BufReader::new(&client).lines();
            assert_eq!(lines.next().unwrap().unwrap(), "GET /test/weights/resolve/abc123/weights.gguf HTTP/1.1");
            for header in lines {
                if header.unwrap().is_empty() {
                    break;
                }
            }
            write!(client, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
            client.write_all(bytes).unwrap();
        });
        (endpoint, handler)
    }

    #[test]
    fn verified_download_when_the_snapshot_is_missing_then_the_cached_blob_restores_it_offline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (endpoint, server) = serve_once(b"model");
        let path = FIXTURE.download(root, &endpoint).unwrap();
        server.join().unwrap();
        assert_eq!(path, root.join("models--test--weights/snapshots/abc123/weights.gguf"));
        assert_eq!(fs::read(&path).unwrap(), b"model");
        assert_eq!(fs::read_link(&path).unwrap(), Path::new("../../blobs").join(FIXTURE.sha256));
        assert_eq!(FIXTURE.cached(root).unwrap().as_ref(), Some(&path));

        fs::remove_file(&path).unwrap();
        assert!(FIXTURE.cached(root).unwrap().is_none());
        assert_eq!(FIXTURE.download(root, &endpoint).unwrap(), path);
        assert_eq!(fs::read(path).unwrap(), b"model");
    }

    #[test]
    fn wrong_digest_at_the_expected_size_when_downloaded_then_no_blob_or_snapshot_is_published() {
        let dir = tempfile::tempdir().unwrap();
        let (endpoint, server) = serve_once(b"other");
        let error = FIXTURE.download(dir.path(), &endpoint).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        server.join().unwrap();
        assert!(FIXTURE.cached(dir.path()).unwrap().is_none());
        let repo = dir.path().join("models--test--weights");
        assert_eq!(fs::read_dir(repo.join("blobs")).unwrap().count(), 0);
        assert!(!repo.join("snapshots").exists());
    }

    #[test]
    fn cached_bytes_when_checked_then_both_size_and_digest_must_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(V4.file);
        let digest = format!("{:x}", Sha256::digest(b"model"));
        assert!(!verified(&path, 5, &digest).unwrap());
        fs::write(&path, b"model").unwrap();
        assert!(verified(&path, 5, &digest).unwrap());
        assert!(!verified(&path, V4.size, &digest).unwrap());
        fs::write(&path, b"other").unwrap();
        assert!(!verified(&path, 5, &digest).unwrap());
    }
}
