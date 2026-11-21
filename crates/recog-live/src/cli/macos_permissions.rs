//! Local companion to the native permissions window. Read operations do not
//! prompt. The existing launcher and Zoom helper own every system request.
use std::io::{self, IsTerminal as _};
use std::path::Path;
use std::process::Command;

use recog_ai::json::{Json, object};
use recog_core::install_config::InstallConfig;
use recog_install::permissions::{Grants, probe};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) fn local_terminal() -> bool {
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && std::env::var_os("SSH_CONNECTION").is_none()
        && std::env::var_os("SSH_TTY").is_none()
}

fn zoom_state(root: &Path, config: &InstallConfig, prompt: bool) -> Result<&'static str> {
    if !config.live.zoom_app {
        return Ok("off");
    }
    let app = super::setup::zoom_helper(root, &config.macos.codesign_identity)?;
    Ok(if recog_zoom::helper_link::accessibility(&app, prompt)? { "granted" } else { "not-granted" })
}

fn request_for(action: &str) -> Result<Option<&str>> {
    match action {
        "microphone" | "system-audio" => Ok(Some(action)),
        "status" | "zoom" => Ok(None),
        _ => Err("Unknown macOS permission".into()),
    }
}

/// The window always invokes the executable inside the stable installed runtime.
pub fn command(action: &str) -> Result<()> {
    let root = super::repo_root();
    let mut env = std::env::vars().collect();
    let config = recog_core::install_config::load(&root, &mut env, &mut |notice| eprintln!("{notice}"))?;
    report(&root, &config, action)
}

fn report(root: &Path, config: &InstallConfig, action: &str) -> Result<()> {
    let request = request_for(action)?;
    let capture = probe(root, request).unwrap_or_else(|e| {
        eprintln!("{e}");
        Grants { microphone: "unknown".into(), system_audio: "unknown".into() }
    });
    let zoom = zoom_state(root, config, action == "zoom").unwrap_or_else(|e| {
        eprintln!("{e}");
        "unknown"
    });
    let states = object([
        ("microphone", Json::Str(capture.microphone)),
        ("system-audio", Json::Str(capture.system_audio)),
        ("zoom", Json::from(zoom)),
    ]);
    println!("{}", states.dumps(None));
    Ok(())
}

pub(super) fn window(root: &Path, config: &InstallConfig, explicit: bool) -> Result<()> {
    if !explicit && ready(root, config) {
        return Ok(());
    }
    let mut cmd = Command::new(root.join("bin/recog-permissions"));
    cmd.arg(root);
    if config.live.zoom_app {
        cmd.arg("--zoom");
    }
    println!("Разрешения Recog открыты в отдельном окне. Закройте окно, чтобы продолжить.");
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("Окно разрешений Recog завершилось с {status}").into());
    }
    Ok(())
}

fn ready(root: &Path, config: &InstallConfig) -> bool {
    probe(root, None).is_ok_and(|grants| grants.microphone == "granted" && grants.system_audio == "granted")
        && matches!(zoom_state(root, config, false), Ok("off" | "granted"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_a_window_action_when_routed_then_only_an_explicit_capture_request_prompts() {
        for action in ["status", "zoom"] {
            assert_eq!(request_for(action).unwrap(), None);
        }
        for action in ["microphone", "system-audio"] {
            assert_eq!(request_for(action).unwrap(), Some(action));
        }
        assert!(request_for("open").is_err());
    }
}
