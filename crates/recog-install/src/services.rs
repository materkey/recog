//! Operate on the installed user units. All service-manager calls use argv.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use crate::units::Backend;

pub const NAMES: [&str; 4] = ["live", "frontend", "copilot-channel", "copilot-supervisor"];
/// User units of the Linux packages (`deploy/linux/packages`), outside [`NAMES`].
pub const UPGRADE_WATCH: &str = "recog-upgrade.path";
pub const UPGRADE_SERVICE: &str = "recog-upgrade.service";

pub struct Manager {
    pub backend: Backend,
    pub dir: PathBuf,
}

fn invoke(program: &str, args: &[&OsStr]) -> io::Result<Output> {
    Command::new(program).args(args).output()
}

fn checked(output: Output) -> io::Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim())))
    }
}

impl Manager {
    fn label(&self, name: &str) -> String {
        match self.backend {
            Backend::Launchd => format!("com.recog.{name}"),
            Backend::Systemd => format!("recog-{name}.service"),
        }
    }
    pub fn unit(&self, name: &str) -> PathBuf {
        let label = self.label(name);
        self.dir.join(if self.backend == Backend::Launchd { format!("{label}.plist") } else { label })
    }
    fn systemctl(&self, args: &[&str]) -> io::Result<Output> {
        Command::new("systemctl").arg("--user").args(args).output()
    }
    fn launch(&self, verb: &str, name: &str) -> io::Result<()> {
        checked(invoke("launchctl", &[verb.as_ref(), self.unit(name).as_os_str()])?)
    }
    pub fn reload(&self) -> io::Result<()> {
        if self.backend == Backend::Systemd {
            checked(self.systemctl(&["daemon-reload"])?)?;
        }
        Ok(())
    }
    pub fn state(&self, name: &str) -> io::Result<State> {
        if !self.unit(name).is_file() {
            return Ok(State::Missing);
        }
        let label = self.label(name);
        match self.backend {
            Backend::Launchd => {
                let out = invoke("launchctl", &["list".as_ref()])?;
                if !out.status.success() {
                    checked(out)?;
                    unreachable!();
                }
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    let mut fields = line.split_whitespace();
                    let pid = fields.next();
                    if fields.nth(1) == Some(label.as_str()) {
                        return Ok(pid
                            .and_then(|p| p.parse::<u32>().ok())
                            .filter(|p| *p != 0)
                            .map_or(State::Loaded, State::Running));
                    }
                }
                Ok(State::Stopped)
            }
            Backend::Systemd => {
                let out = self.systemctl(&["show", "-p", "MainPID", "--value", &label])?;
                if out.status.success()
                    && let Some(pid) =
                        String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok().filter(|p| *p != 0)
                {
                    return Ok(State::Running(pid));
                }
                let loaded = self.systemctl(&["is-enabled", "--quiet", &label])?.status.success()
                    || self.systemctl(&["is-active", "--quiet", &label])?.status.success();
                Ok(if loaded { State::Loaded } else { State::Stopped })
            }
        }
    }
    pub fn activate(&self, name: &str, restart: bool) -> io::Result<()> {
        let unit = self.unit(name);
        if !unit.is_file() {
            return Err(io::Error::other(format!("{} missing; run recog service install", unit.display())));
        }
        if self.backend == Backend::Systemd {
            let label = self.label(name);
            return checked(if restart {
                self.systemctl(&["restart", &label])?
            } else {
                self.systemctl(&["enable", "--now", &label])?
            });
        }
        let reload = if restart {
            true
        } else {
            match self.state(name)? {
                State::Running(_) => return Ok(()),
                State::Loaded => true,
                _ => false,
            }
        };
        if reload {
            let _ = self.launch("unload", name);
            thread::sleep(Duration::from_secs(1));
        }
        self.launch("load", name)
    }
    pub fn stop(&self, name: &str, uninstall: bool) -> io::Result<()> {
        if self.unit(name).is_file() {
            match self.backend {
                Backend::Launchd => {
                    let _ = self.launch("unload", name);
                }
                Backend::Systemd => {
                    let label = self.label(name);
                    if uninstall {
                        self.systemctl(&["disable", "--now", &label])?;
                    } else {
                        self.systemctl(&["stop", &label])?;
                    }
                }
            }
            if uninstall {
                fs::remove_file(self.unit(name))?;
            }
        }
        Ok(())
    }
    /// The Linux packages ship `recog-upgrade.path`, which `recog setup`
    /// enables; it refreshes the services it would otherwise bring back.
    /// Best effort, as `launchctl unload` in `stop`: an archive has no such unit.
    pub fn forget_upgrades(&self) {
        if self.backend == Backend::Systemd {
            let _ = self.systemctl(&["disable", "--now", UPGRADE_WATCH, UPGRADE_SERVICE]);
        }
    }
    pub fn reap_copilot(&self) -> io::Result<()> {
        if self.backend == Backend::Systemd {
            self.systemctl(&["stop", "recog-copilot-session.service"])?;
        }
        for target in ["=copilot", "=copilot-manual"] {
            let tmux = |verb: &str| Command::new("tmux").args(["-L", "recog-copilot", verb, "-t", target]).output();
            for attempt in 1..=3 {
                match tmux("kill-session") {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                    other => {
                        other?;
                    }
                }
                if !tmux("has-session")?.status.success() {
                    break;
                }
                if attempt == 3 {
                    return Err(io::Error::other(format!("Could not stop tmux session {target}")));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub enum State {
    Missing,
    Stopped,
    Loaded,
    Running(u32),
}

pub fn log_path(logs: &Path, name: &str, stdout: bool) -> PathBuf {
    logs.join(format!("{name}.{}.log", if stdout { "stdout" } else { "stderr" }))
}
