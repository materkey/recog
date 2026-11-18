use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use super::{args, install};
use recog_core::install_config::{Environ, InstallConfig};
use recog_install::readiness::{Check, checks};
use recog_install::services::{Manager, NAMES, State, log_path};

pub(super) fn command(root: &Path, config: &InstallConfig, env: &Environ, args: &[String]) -> i32 {
    match run(root, config, env, args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn run(root: &Path, config: &InstallConfig, env: &Environ, args: &[String]) -> Result<(), String> {
    let fail = |e: io::Error| e.to_string();
    let backend = install::service_backend()?;
    let home = std::env::var_os("HOME").ok_or("HOME is missing")?;
    let manager = Manager { backend, dir: backend.unit_dir(home.as_ref()) };
    let action = args.first().map_or("", String::as_str);
    let mut names = NAMES.to_vec();
    let mut selected = false;
    let mut stdout = false;
    for arg in &args[1..] {
        if arg == "--stdout" && action == "logs" && !stdout {
            stdout = true;
        } else if !selected
            && matches!(action, "start" | "stop" | "restart" | "status" | "logs")
            && NAMES.contains(&arg.as_str())
        {
            names = vec![arg.as_str()];
            selected = true;
        } else {
            return Err(args::SERVICE_USAGE.into());
        }
    }
    if matches!(action, "stop" | "uninstall") {
        for name in &names {
            manager.stop(name, action == "uninstall").map_err(fail)?;
        }
        if action == "uninstall" {
            manager.reload().map_err(fail)?;
        }
        if names.contains(&"copilot-supervisor") {
            manager.reap_copilot().map_err(fail)?;
        }
        return Ok(());
    }
    let data = super::data_dir(root, env)?;
    let logs = data.join("logs");
    match action {
        "install" => {
            std::fs::create_dir_all(&logs).map_err(fail)?;
            for file in install::render_units(root, env, &manager.dir)? {
                println!("Installed {}", file.display());
            }
            manager.reload().map_err(fail)
        }
        "logs" => {
            let path = log_path(&logs, names[0], stdout);
            if !path.is_file() {
                return Err(format!("Log file not found: {}", path.display()));
            }
            Err(Command::new("tail").arg("-f").arg(path).exec().to_string())
        }
        "debug" => {
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(std::time::Duration::from_secs(3)))
                .build()
                .new_agent();
            let body = agent
                .get("http://127.0.0.1:9080/api/debug")
                .call()
                .and_then(|mut r| r.body_mut().read_to_string())
                .map_err(|e| e.to_string())?;
            let value = recog_core::pyjson::loads(&body).map_err(|e| e.to_string())?;
            println!("{}", recog_core::pyjson::dumps(&value, Some(2)));
            Ok(())
        }
        "status" => {
            show_status(&manager, &names)?;
            if manager.backend == recog_install::units::Backend::Systemd {
                println!(
                    "To keep user services running after logout: loginctl enable-linger {}",
                    std::env::var("USER").unwrap_or_else(|_| "$USER".into())
                );
            }
            Ok(())
        }
        "doctor" | "start" | "restart" => {
            let mut rows = checks(root, &std::env::var_os("PATH").unwrap_or_default(), backend);
            if action == "doctor" {
                rows.extend(recog_install::readiness::permission_checks(root, backend, config.live.zoom_app));
            }
            let checked = install::check(root, config, env);
            rows.push(Check {
                name: "configuration".into(),
                ready: checked.code == 0,
                required: true,
                hint: format!("{}{}", checked.stdout.trim(), checked.stderr.trim()),
            });
            let mut ready = true;
            for row in rows {
                if !row.ready && row.required {
                    ready = false;
                }
                if !row.ready || action == "doctor" {
                    println!(
                        "{} {} {}",
                        if row.ready {
                            "[ok]"
                        } else if row.required {
                            "[missing]"
                        } else {
                            "[optional]"
                        },
                        row.name,
                        if row.ready { "" } else { &row.hint }
                    );
                }
            }
            if action == "doctor" {
                let running = show_status(&manager, &names)?;
                return if ready && running { Ok(()) } else { Err("Installation is not ready".into()) };
            }
            if !ready && std::env::var("RECOG_SKIP_READINESS").as_deref() != Ok("1") {
                return Err("Install is not complete; services left untouched".into());
            }
            if !selected {
                names.retain(|n| manager.unit(n).is_file());
            }
            if names.is_empty() {
                return Err("No installed services; run recog service install".into());
            }
            std::fs::create_dir_all(&logs).map_err(fail)?;
            let mut failures = Vec::new();
            for name in names {
                if let Err(e) = manager.activate(name, action == "restart") {
                    failures.push(format!("{name}: {e}"));
                }
            }
            if failures.is_empty() { Ok(()) } else { Err(failures.join("\n")) }
        }
        _ => Err(args::SERVICE_USAGE.into()),
    }
}
fn show_status(manager: &Manager, names: &[&str]) -> Result<bool, String> {
    let mut serving = true;
    for name in names {
        let state = manager.state(name).map_err(|e| e.to_string())?;
        if matches!(*name, "live" | "frontend") && !matches!(state, State::Running(_)) {
            serving = false;
        }
        println!("{name}: {state:?}");
    }
    Ok(serving)
}
