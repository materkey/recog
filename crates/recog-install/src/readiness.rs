//! Installed-program and command checks shared by service activation and doctor.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::units::Backend;

pub const PROGRAMS: [&str; 5] =
    ["recog", "recog-web", "recog-copilot-channel", "recog-copilot-supervisor", "recog-desktop"];

pub fn executable(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
pub fn available(name: &str, path: &OsStr) -> bool {
    std::env::split_paths(path).any(|dir| executable(&dir.join(name)))
}

pub struct Check {
    pub name: String,
    pub ready: bool,
    pub required: bool,
    pub hint: String,
}

pub fn checks(root: &Path, path: &OsStr, backend: Backend) -> Vec<Check> {
    let mut rows = Vec::new();
    for (name, required, hint) in [
        ("ffmpeg", true, "Install ffmpeg for audio playback"),
        ("tmux", false, "Install tmux to start Copilot sessions"),
        ("claude", false, "Install Claude Code and sign in to use Copilot"),
    ] {
        rows.push(Check { name: name.into(), ready: available(name, path), required, hint: hint.into() });
    }
    if backend == Backend::Systemd {
        for name in ["pactl", "parec"] {
            rows.push(Check {
                name: name.into(),
                ready: available(name, path),
                required: true,
                hint: "Install pulseaudio-utils (Debian/Ubuntu) or libpulse (Arch)".into(),
            });
        }
    }
    let mut programs: Vec<_> = PROGRAMS.iter().map(|p| root.join("bin").join(p)).collect();
    if backend == Backend::Launchd {
        programs.extend(
            ["bin/recog-mic", "sck_audio_capture", "Recog.app/Contents/MacOS/recog_launcher"].map(|p| root.join(p)),
        );
    }
    rows.extend(programs.into_iter().map(|p| Check {
        name: p.display().to_string(),
        ready: executable(&p),
        required: true,
        hint: "Run setup to install this program".into(),
    }));
    rows
}
