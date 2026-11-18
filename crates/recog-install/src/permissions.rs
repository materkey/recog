//! Inspect or request macOS capture permission using the same responsible application
//! as the live service. No microphone or screen stream is opened by the probe.
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct Grants {
    pub microphone: String,
    pub system_audio: String,
}

fn escaped(path: &Path) -> String {
    path.to_string_lossy().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn probe(root: &Path, request: Option<&str>) -> io::Result<Grants> {
    let launcher = root.join("Recog.app/Contents/MacOS/recog_launcher");
    let helper = root.join("bin/recog-permission-probe");
    if !launcher.is_file() || !helper.is_file() {
        return Err(io::Error::other("Run recog setup to install Recog.app and its permission probe"));
    }
    let flag = match request {
        None => "--permission-status",
        Some("microphone") => "--request-microphone",
        Some("system-audio") => "--request-system-audio",
        Some(_) => return Err(io::Error::other("Unknown capture permission")),
    };
    let temp = tempfile::Builder::new().prefix("recog-permissions-").tempdir()?;
    let label = temp.path().file_name().unwrap().to_string_lossy().to_string();
    let plist = temp.path().join("probe.plist");
    let stdout = temp.path().join("out");
    let stderr = temp.path().join("err");
    let user = Command::new("/usr/bin/id").arg("-u").output()?;
    if !user.status.success() {
        return Err(io::Error::other("Cannot determine the GUI login"));
    }
    let domain = format!("gui/{}", String::from_utf8_lossy(&user.stdout).trim());
    fs::write(
        &plist,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{}</string><string>{}</string><string>{flag}</string></array>
<key>RunAtLoad</key><true/><key>StandardOutPath</key><string>{}</string>
<key>StandardErrorPath</key><string>{}</string></dict></plist>"#,
            escaped(&launcher),
            escaped(&helper),
            escaped(&stdout),
            escaped(&stderr)
        ),
    )?;
    let out = Command::new("/bin/launchctl").args(["bootstrap", &domain]).arg(&plist).output()?;
    if !out.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let deadline = Instant::now() + Duration::from_secs(if request.is_some() { 120 } else { 8 });
    let result = loop {
        if let Ok(text) = fs::read_to_string(&stdout) {
            let mut lines = text.lines();
            if let (Some(mic), Some(system)) = (
                lines.next().and_then(|s| s.strip_prefix("microphone=")),
                lines.next().and_then(|s| s.strip_prefix("system-audio=")),
            ) {
                break Ok(Grants { microphone: mic.to_string(), system_audio: system.to_string() });
            }
        }
        if Instant::now() >= deadline {
            break Err(io::Error::other(
                "Recog permission probe timed out; finish the macOS dialog and run recog setup again",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = Command::new("/bin/launchctl").args(["bootout", &format!("{domain}/{label}")]).output();
    result
}
