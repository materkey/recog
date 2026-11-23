//! Installed-program and command checks shared by service activation and doctor.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::units::Backend;

pub const PROGRAMS: [&str; 6] =
    ["recog", "recog-web", "recog-copilot-channel", "recog-copilot-supervisor", "recog-copilot-codex", "recog-desktop"];

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
    ] {
        rows.push(Check { name: name.into(), ready: available(name, path), required, hint: hint.into() });
    }
    // Copilot, titles, summaries and meeting questions take whichever of the two is there.
    rows.push(Check {
        name: "claude or codex".into(),
        ready: available("claude", path) || available("codex", path),
        required: false,
        hint: "Install Claude Code or Codex and sign in for Copilot, meeting titles, summaries and questions".into(),
    });
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

pub fn permission_checks(root: &Path, backend: Backend, zoom: bool) -> Vec<Check> {
    if backend != Backend::Launchd {
        return Vec::new();
    }
    let mut rows = capture_grants(crate::permissions::probe(root, None));
    if zoom {
        let app = root.join(recog_zoom::helper_app::HELPER_APP);
        let result = recog_zoom::helper_link::accessibility(&app, false);
        rows.push(Check {
            name: "Recog Zoom Helper — Универсальный доступ".into(),
            ready: matches!(result, Ok(true)),
            required: false,
            hint: result.err().map_or_else(|| "recog setup --permissions".into(), |e| e.to_string()),
        });
    }
    rows
}

fn capture_grants(probe: std::io::Result<crate::permissions::Grants>) -> Vec<Check> {
    match probe {
        Ok(grants) => [("Микрофон", grants.microphone), ("Системный звук", grants.system_audio)]
            .into_iter()
            .map(|(name, state)| Check {
                name: name.into(),
                ready: state == "granted",
                required: true,
                hint: format!("{state}; recog setup --permissions"),
            })
            .collect(),
        Err(e) => {
            vec![Check { name: "Разрешения macOS".into(), ready: false, required: true, hint: e.to_string() }]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn grants_when_reporting_readiness_then_each_capture_is_reported_separately() {
        for (mic, system) in [("granted", "not-granted"), ("denied", "granted")] {
            let checks =
                capture_grants(Ok(crate::permissions::Grants { microphone: mic.into(), system_audio: system.into() }));
            assert_eq!((checks[0].ready, checks[1].ready), (mic == "granted", system == "granted"));
            assert!(checks.iter().all(|c| c.required));
        }
        let failed = capture_grants(Err(std::io::Error::other("launcher unavailable")));
        assert!(!failed[0].ready && failed[0].hint.contains("unavailable"));
    }

    #[test]
    fn missing_programs_when_checking_each_platform_then_only_platform_dependencies_block_activation() {
        let dir = tempfile::tempdir().unwrap();
        for backend in [Backend::Systemd, Backend::Launchd] {
            let rows = checks(dir.path(), OsStr::new("/nonexistent"), backend);
            for name in PROGRAMS {
                assert!(rows.iter().any(|r| r.name.ends_with(&format!("/bin/{name}")) && !r.ready && r.required));
            }
            assert!(!rows.iter().any(|r| r.name == "claude"));
            for name in ["tmux", "claude or codex"] {
                assert!(rows.iter().any(|r| r.name == name && !r.required && !r.ready));
            }
            assert_eq!(rows.iter().any(|r| r.name == "parec"), backend == Backend::Systemd);
            assert_eq!(rows.iter().any(|r| r.name.ends_with("recog_launcher")), backend == Backend::Launchd);
        }
        let fake = dir.path().join("ffmpeg");
        fs::write(&fake, "fixture").unwrap();
        assert!(!available("ffmpeg", dir.path().as_os_str()));
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(available("ffmpeg", dir.path().as_os_str()));
    }

    #[test]
    fn only_codex_installed_when_checking_then_the_ai_row_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("codex");
        fs::write(&codex, "fixture").unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
        let rows = checks(dir.path(), dir.path().as_os_str(), Backend::Launchd);
        assert!(rows.iter().find(|r| r.name == "claude or codex").unwrap().ready);
    }
}
