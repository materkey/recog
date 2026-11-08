//! psutil's Linux process table, read from `/proc` as `psutil/_pslinux.py` reads it:
//! the name from `stat`, the real uid from `status`, the start time as `starttime`
//! clock ticks after `btime`, `exe` through the link, `cmdline`, `environ`, and the
//! listening TCP sockets by matching the process's socket inodes against
//! `net/tcp` and `net/tcp6`.
//!
//! It only reads files, so it builds everywhere and its tests run on a scripted
//! `/proc` tree on any Unix.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use crate::host::ProcessTable;
use crate::py::PyErr;

/// The table over a `/proc` mount.
pub struct LinuxTable {
    root: PathBuf,
    clock_ticks: f64,
}

impl LinuxTable {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let ticks = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK).ok().flatten().unwrap_or(100);
        #[allow(clippy::cast_precision_loss)]
        Self { root: root.into(), clock_ticks: ticks as f64 }
    }

    /// The table with a fixed tick rate (tests).
    #[must_use]
    pub fn with_ticks(root: impl Into<PathBuf>, clock_ticks: f64) -> Self {
        Self { root: root.into(), clock_ticks }
    }

    fn dir(&self, pid: i64) -> PathBuf {
        self.root.join(pid.to_string())
    }

    /// psutil's `wrap_exceptions`: a missing file is a gone process, a refused one
    /// is `AccessDenied`.
    fn failure(&self, pid: i64, error: &io::Error) -> PyErr {
        match error.kind() {
            io::ErrorKind::PermissionDenied => PyErr::access_denied(pid),
            io::ErrorKind::NotFound if !self.dir(pid).exists() => PyErr::no_such_process(pid),
            io::ErrorKind::NotFound => PyErr::zombie(pid),
            _ => PyErr::os(error, None),
        }
    }

    fn read(&self, pid: i64, name: &str) -> Result<Vec<u8>, PyErr> {
        fs::read(self.dir(pid).join(name)).map_err(|e| self.failure(pid, &e))
    }

    /// `_parse_stat_file()`: the name between the first `(` and the last `)`, and the
    /// fields after it.
    fn stat(&self, pid: i64) -> Result<(String, Vec<String>), PyErr> {
        let raw = self.read(pid, "stat")?;
        let text = String::from_utf8_lossy(&raw);
        let (Some(open), Some(close)) = (text.find('('), text.rfind(')')) else {
            return Err(PyErr::value(format!("malformed stat file of pid {pid}")));
        };
        let name = text[open + 1..close].to_owned();
        let fields = text[close + 1..].split_whitespace().map(str::to_owned).collect();
        Ok((name, fields))
    }

    fn boot_time(&self) -> Result<f64, PyErr> {
        let path = self.root.join("stat");
        let text = fs::read_to_string(&path).map_err(|e| PyErr::os(&e, Some(&path)))?;
        text.lines()
            .find(|line| line.starts_with("btime"))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<f64>().ok())
            .ok_or_else(|| PyErr::new("RuntimeError", format!("line 'btime' not found in {}", path.display())))
    }

    /// The inodes of the process's sockets (`socket:[N]` links under `fd/`).
    fn socket_inodes(&self, pid: i64) -> Result<HashSet<String>, PyErr> {
        let directory = self.dir(pid).join("fd");
        let entries = fs::read_dir(&directory).map_err(|e| self.failure(pid, &e))?;
        let mut inodes = HashSet::new();
        for entry in entries.flatten() {
            // A descriptor closed meanwhile is skipped, as psutil does.
            let Ok(link) = fs::read_link(entry.path()) else { continue };
            let link = link.to_string_lossy();
            if let Some(inode) = link.strip_prefix("socket:[").and_then(|rest| rest.strip_suffix(']')) {
                inodes.insert(inode.to_owned());
            }
        }
        Ok(inodes)
    }
}

/// `decode_address` of a `net/tcp` (`v6 == false`) or `net/tcp6` local address:
/// hex in the kernel's byte order, which is little-endian words on every host this
/// runs on.
fn decode_address(text: &str, v6: bool) -> Option<(String, u16)> {
    let (ip, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let bytes: Vec<u8> =
        (0..ip.len() / 2).map(|i| u8::from_str_radix(&ip[i * 2..i * 2 + 2], 16)).collect::<Result<_, _>>().ok()?;
    let address = if v6 {
        let words: [u8; 16] = bytes.try_into().ok()?;
        let mut octets = [0u8; 16];
        for (i, word) in words.as_chunks::<4>().0.iter().enumerate() {
            let value = u32::from_le_bytes(*word);
            octets[i * 4..i * 4 + 4].copy_from_slice(&value.to_be_bytes());
        }
        Ipv6Addr::from(octets).to_string()
    } else {
        let word: [u8; 4] = bytes.try_into().ok()?;
        Ipv4Addr::from(u32::from_le_bytes(word)).to_string()
    };
    Some((address, port))
}

/// `cmdline()`: NUL-separated arguments, or space-separated for a process that
/// rewrote its title.
fn split_cmdline(data: &str) -> Vec<String> {
    if data.is_empty() {
        return Vec::new();
    }
    let separator = if data.ends_with('\0') { '\0' } else { ' ' };
    let data = data.strip_suffix(separator).unwrap_or(data);
    let parts: Vec<String> = data.split(separator).map(str::to_owned).collect();
    if separator == '\0' && parts.len() == 1 && data.contains(' ') {
        return data.split(' ').map(str::to_owned).collect();
    }
    parts
}

impl ProcessTable for LinuxTable {
    fn pids(&self) -> Vec<i64> {
        let mut pids: Vec<i64> = fs::read_dir(&self.root)
            .map(|entries| entries.flatten().filter_map(|e| e.file_name().to_str()?.parse::<i64>().ok()).collect())
            .unwrap_or_default();
        pids.sort_unstable();
        pids
    }

    fn open(&self, pid: i64) -> Result<(), PyErr> {
        if pid < 0 {
            return Err(PyErr::value("pid must be a positive integer (got -1)"));
        }
        self.stat(pid).map(|_| ())
    }

    fn name(&self, pid: i64) -> Result<String, PyErr> {
        let (name, _) = self.stat(pid)?;
        let cmdline = if name.chars().count() >= 15 {
            match self.cmdline(pid) {
                Ok(args) => Some(args),
                Err(e) if e.is("AccessDenied") || e.is("ZombieProcess") => None,
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        Ok(super::extend_name(name, cmdline.as_deref()))
    }

    fn uid_real(&self, pid: i64) -> Result<u32, PyErr> {
        let raw = self.read(pid, "status")?;
        String::from_utf8_lossy(&raw)
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|uid| uid.parse().ok())
            .ok_or_else(|| PyErr::value(format!("no Uid line in the status of pid {pid}")))
    }

    fn create_time(&self, pid: i64) -> Result<f64, PyErr> {
        let (_, fields) = self.stat(pid)?;
        let ticks: f64 = fields
            .get(19)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| PyErr::value(format!("no start time in the stat of pid {pid}")))?;
        Ok(ticks / self.clock_ticks + self.boot_time()?)
    }

    fn exe(&self, pid: i64) -> Result<String, PyErr> {
        match fs::read_link(self.dir(pid).join("exe")) {
            Ok(link) => {
                let link = link.to_string_lossy();
                let link = link.split('\0').next().unwrap_or_default();
                match link.strip_suffix(" (deleted)") {
                    Some(stripped) if !Path::new(link).exists() => Ok(stripped.to_owned()),
                    _ => Ok(link.to_owned()),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && self.dir(pid).exists() => Ok(String::new()),
            Err(e) => Err(self.failure(pid, &e)),
        }
    }

    fn cmdline(&self, pid: i64) -> Result<Vec<String>, PyErr> {
        Ok(split_cmdline(&String::from_utf8_lossy(&self.read(pid, "cmdline")?)))
    }

    fn environ(&self, pid: i64) -> Result<Vec<(String, String)>, PyErr> {
        Ok(super::parse_environ_block(&String::from_utf8_lossy(&self.read(pid, "environ")?)))
    }

    fn tcp_listeners(&self, pid: i64) -> Result<Vec<(String, u16)>, PyErr> {
        let inodes = self.socket_inodes(pid)?;
        let mut out = Vec::new();
        for (file, v6) in [("net/tcp", false), ("net/tcp6", true)] {
            let Ok(text) = fs::read_to_string(self.root.join(file)) else { continue };
            for line in text.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let (Some(local), Some(state), Some(inode)) = (fields.get(1), fields.get(3), fields.get(9)) else {
                    continue;
                };
                if *state == "0A"
                    && inodes.contains(*inode)
                    && let Some(address) = decode_address(local, v6)
                {
                    out.push(address);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(root: &Path, pid: i64, stat_name: &str) -> PathBuf {
        let dir = root.join(pid.to_string());
        fs::create_dir_all(dir.join("fd")).unwrap();
        let mut fields = vec!["S".to_owned(); 19];
        fields.push("250".to_owned());
        fs::write(dir.join("stat"), format!("{pid} ({stat_name}) {} 0 0", fields.join(" "))).unwrap();
        fs::write(dir.join("status"), "Name:\tktalk\nUid:\t1000\t1001\t1000\t1000\n").unwrap();
        dir
    }

    #[test]
    fn scripted_proc_tree_when_read_then_psutil_answers() {
        // given
        let tree = tempfile::tempdir().unwrap();
        let root = tree.path();
        fs::write(root.join("stat"), "cpu 1 2 3\nbtime 1700000000\n").unwrap();
        let dir = process(root, 42, "ktalk (main) x");
        fs::write(dir.join("cmdline"), "/opt/ktalk/ktalk\0--no-sandbox\0").unwrap();
        fs::write(dir.join("environ"), "APPIMAGE=/home/u/KTalk.AppImage\0NODE_OPTIONS=--x\0").unwrap();
        std::os::unix::fs::symlink("/opt/ktalk/ktalk", dir.join("exe")).unwrap();
        std::os::unix::fs::symlink("socket:[777]", dir.join("fd/3")).unwrap();
        std::os::unix::fs::symlink("socket:[778]", dir.join("fd/4")).unwrap();
        std::os::unix::fs::symlink("/dev/null", dir.join("fd/5")).unwrap();
        fs::create_dir_all(root.join("net")).unwrap();
        fs::write(
            root.join("net/tcp"),
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
             0: 0100007F:ACD2 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 777 1\n\
             1: 0100007F:1F90 0100007F:9000 01 00000000:00000000 00:00000000 00000000  1000        0 778 1\n",
        )
        .unwrap();
        fs::write(
            root.join("net/tcp6"),
            "  sl  local_address                         remote_address st\n\
             0: 00000000000000000000000001000000:2405 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 1000 0 778 1\n",
        )
        .unwrap();
        let table = LinuxTable::with_ticks(root, 100.0);
        // when / then
        assert_eq!(table.pids(), vec![42]);
        assert_eq!(table.name(42).unwrap(), "ktalk (main) x");
        assert_eq!(table.uid_real(42).unwrap(), 1000);
        assert_eq!(table.create_time(42).unwrap(), 2.5 + 1_700_000_000.0);
        assert_eq!(table.exe(42).unwrap(), "/opt/ktalk/ktalk");
        assert_eq!(table.cmdline(42).unwrap(), ["/opt/ktalk/ktalk", "--no-sandbox"]);
        assert_eq!(table.environ(42).unwrap()[0], ("APPIMAGE".to_owned(), "/home/u/KTalk.AppImage".to_owned()));
        assert_eq!(table.tcp_listeners(42).unwrap(), [("127.0.0.1".to_owned(), 44242), ("::1".to_owned(), 9221)]);
        assert!(table.open(43).unwrap_err().is("NoSuchProcess"));
    }

    #[test]
    fn retitled_process_when_cmdline_read_then_split_on_spaces_like_psutil() {
        // given / when / then
        assert_eq!(split_cmdline("ktalk --a --b"), ["ktalk", "--a", "--b"]);
        assert_eq!(split_cmdline("ktalk --a\0"), ["ktalk", "--a"]);
        assert_eq!(split_cmdline("a\0b\0"), ["a", "b"]);
        assert!(split_cmdline("").is_empty());
    }
}
