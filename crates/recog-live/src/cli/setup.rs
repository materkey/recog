//! Build a checkout, then let its freshly built binary install it.
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use recog_core::install_config::{Environ, InstallConfig};
use recog_core::meeting_storage::MeetingStorage;
use recog_install::readiness::{PROGRAMS, checks, executable};
use recog_install::services::{Manager, NAMES, State};

use super::{install, storage_options};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const HELP: &str = "usage: recog setup [--dry-run] [--no-services] [--no-extension] [--no-prefetch] [--no-env] [--force] | --permissions";

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("{} exited with {status}", cmd.get_program().to_string_lossy()).into());
    }
    Ok(())
}

fn manager() -> Result<Manager> {
    let backend = install::service_backend()?;
    let home = std::env::var_os("HOME").ok_or("HOME is missing")?;
    Ok(Manager { backend, dir: backend.unit_dir(home.as_ref()) })
}

fn guard(root: &Path, config: &InstallConfig, env: &Environ, force: bool) -> Result<()> {
    if force || !matches!(manager()?.state("live")?, State::Running(_)) {
        return Ok(());
    }
    let data = super::data_dir(root, env)?;
    let store = MeetingStorage::new(&data.join("meetings"), storage_options(config, env))?;
    for meeting in store.list_active_meetings()? {
        let source = meeting.as_object().and_then(|m| m.get("source")).and_then(|s| s.as_str());
        if store.is_conference_source(source) {
            return Err("A conference is active. Run this command after the call or pass --force.".into());
        }
    }
    Ok(())
}

fn programs() -> Vec<&'static str> {
    let mut names = PROGRAMS.to_vec();
    if cfg!(target_os = "macos") {
        names.push("recog-mic");
    }
    names
}
fn release_dir(root: &Path) -> Result<PathBuf> {
    let out =
        Command::new("cargo").args(["metadata", "--no-deps", "--format-version", "1"]).current_dir(root).output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    let value = recog_core::pyjson::loads(std::str::from_utf8(&out.stdout)?)?;
    let dir = value
        .as_object()
        .and_then(|o| o.get("target_directory"))
        .and_then(|v| v.as_str())
        .ok_or("cargo metadata did not report a target directory")?;
    Ok(Path::new(dir).join("release"))
}

fn build(root: &Path, args: &[String]) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root).args(["build", "--release", "--locked"]);
    for name in programs() {
        cmd.args(["--bin", name]);
    }
    run(&mut cmd)?;
    let dir = release_dir(root)?;
    Err(Command::new(dir.join("recog"))
        .current_dir(root)
        .env("RECOG_REPO_DIR", root)
        .arg("__install-built")
        .args(args)
        .exec()
        .into())
}

fn link_command(root: &Path, home: &Path) -> Result<()> {
    let dir = home.join(".local/bin");
    if !std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|p| p == dir) {
        println!("Add {} to PATH to run recog from any directory", dir.display());
        return Ok(());
    }
    fs::create_dir_all(&dir)?;
    let link = dir.join("recog");
    match symlink(root.join("bin/recog"), &link) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if fs::canonicalize(&link).ok() != fs::canonicalize(root.join("bin/recog")).ok() {
                eprintln!("Keeping existing {}", link.display());
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
fn install_programs(root: &Path) -> Result<()> {
    let this = fs::canonicalize(std::env::current_exe()?)?;
    let release = this.parent().ok_or("no build directory")?;
    let bin = root.join("bin");
    fs::create_dir_all(&bin)?;
    let stage = tempfile::tempdir_in(&bin)?;
    for name in programs() {
        let src = release.join(name);
        if !executable(&src) {
            return Err(format!("Missing built executable: {}", src.display()).into());
        }
        fs::copy(src, stage.path().join(name))?;
    }
    for name in programs() {
        fs::rename(stage.path().join(name), bin.join(name))?;
    }
    Ok(())
}
fn copy_initial_env(root: &Path) -> Result<()> {
    if root.join(".env").symlink_metadata().is_ok() || !root.join(".env.example").is_file() {
        return Ok(());
    }
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    io::copy(&mut fs::File::open(root.join(".env.example"))?, &mut temp)?;
    match temp.persist_noclobber(root.join(".env")) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn guide_permissions(root: &Path, config: &InstallConfig) -> Result<()> {
    use recog_install::permissions::probe;
    let terminal = io::stdin().is_terminal() && io::stdout().is_terminal();
    let mut grants = probe(root, None)?;
    for (key, title, reason, pane) in [
        ("microphone", "Микрофон", "для вашей речи", "Microphone"),
        ("system-audio", "Запись экрана и системного звука", "для звука других участников", "ScreenCapture"),
    ] {
        let granted = if key == "microphone" { &grants.microphone } else { &grants.system_audio };
        println!("Recog — {title} ({reason}): {granted}");
        if granted == "granted" {
            continue;
        }
        if terminal {
            print!("Нажмите Enter, чтобы запросить разрешение, или введите s, чтобы пропустить: ");
            io::stdout().flush()?;
            let mut answer = String::new();
            if io::stdin().read_line(&mut answer)? > 0 && answer.trim().is_empty() {
                grants = probe(root, Some(key))?;
            }
        }
        let state = if key == "microphone" { &grants.microphone } else { &grants.system_audio };
        if state != "granted" {
            println!(
                "Откройте Настройки системы → Конфиденциальность и безопасность → {title}, включите Recog и повторите recog setup --permissions. Ссылка: x-apple.systempreferences:com.apple.preference.security?Privacy_{pane}"
            );
        }
    }
    if config.live.zoom_app {
        let identity = &config.macos.codesign_identity;
        let app = recog_zoom::helper_app::find_or_build_helper(
            root,
            if identity.is_empty() { "-" } else { identity },
            &recog_zoom::helper_app::SystemToolchain,
            &recog_zoom::log::Logger::stderr(),
        )?;
        if !recog_zoom::helper_link::accessibility(&app, false)? {
            println!(
                "Для настольного Zoom включите Recog Zoom Helper в Настройки системы → Конфиденциальность и безопасность → Универсальный доступ."
            );
            if terminal {
                print!("Нажмите Enter для запроса или введите s, чтобы пропустить: ");
                io::stdout().flush()?;
                let mut answer = String::new();
                if io::stdin().read_line(&mut answer)? > 0 && answer.trim().is_empty() {
                    let granted = recog_zoom::helper_link::accessibility(&app, true)?;
                    if !granted {
                        println!("После выдачи доступа проверьте состояние через recog doctor.");
                    }
                }
            }
        } else {
            println!("Recog Zoom Helper: универсальный доступ выдан.");
        }
    }
    Ok(())
}

pub(super) fn setup(root: &Path, config: &InstallConfig, env: &Environ, args: &[String], built: bool) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return Ok(());
    }
    if args == ["--permissions"] {
        if cfg!(target_os = "macos") {
            return guide_permissions(root, config);
        }
        return Err("macOS permission setup is available on a Mac".into());
    }
    if !args.iter().all(|a| {
        matches!(
            a.as_str(),
            "--force" | "--no-services" | "--no-env" | "--no-extension" | "--no-prefetch" | "--dry-run"
        )
    }) {
        return Err(HELP.into());
    }
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if !recog_core::install::is_checkout(root) {
        return Err("Source setup needs a recog checkout".into());
    }
    if has("--dry-run") {
        println!(
            "Would build and check the programs, install them into {}, prepare permissions and the data directory.",
            root.join("bin").display()
        );
        println!(
            "Extension: {}; prefetch: {}; service activation: {}",
            !has("--no-extension"),
            !has("--no-prefetch"),
            !has("--no-services")
        );
        return Ok(());
    }
    if !built {
        return build(root, args);
    }
    let checked = install::check(root, config, env);
    if checked.code != 0 {
        return Err(format!("{}{}", checked.stdout, checked.stderr).into());
    }
    guard(root, config, env, has("--force"))?;
    install_programs(root)?;
    finish(root, config, env, args, false)
}

fn finish(root: &Path, config: &InstallConfig, env: &Environ, args: &[String], packaged: bool) -> Result<()> {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let manager = manager()?;
    let installed = NAMES.iter().any(|n| manager.unit(n).is_file());
    if !installed && !has("--no-env") {
        copy_initial_env(root)?;
    }
    let data = recog_core::data_dir::get_data_dir_process(root)?;
    fs::create_dir_all(&data)?;
    let bin = root.join("bin/recog");
    if !packaged || !root.join("recog-homebrew").is_file() {
        link_command(root, &recog_core::paths::expanduser("~", env))?;
    }
    if !has("--no-extension") {
        let ext = root.join("browser-extension");
        if packaged {
            recog_install::distribution::configure_extension(root, &config.jitsi, &config.matrix)?;
            println!(
                "Chrome: chrome://extensions → Developer mode → Load unpacked → {} (Reload after changes)",
                ext.join("dist").display()
            );
        } else if ext.is_dir() {
            run(Command::new("npm")
                .arg(if ext.join("package-lock.json").is_file() { "ci" } else { "install" })
                .current_dir(&ext))?;
            run(Command::new("npm").args(["run", "build"]).current_dir(&ext))?;
            println!(
                "Chrome: chrome://extensions → Developer mode → Load unpacked → {} (Reload after changes)",
                ext.join("dist").display()
            );
        }
    }
    if cfg!(target_os = "macos") {
        use crate::capture::sck::{self, OsToolchain};
        if !packaged {
            sck::find_or_build_sck_utility(root, &OsToolchain, &crate::log::Logger::stderr())
                .map_err(|e| e.to_string())?;
        }
        run(Command::new(&bin).arg("launcher"))?;
        if !packaged {
            let probe = tempfile::tempdir_in(root.join("bin"))?;
            let target = probe.path().join("recog-permission-probe");
            run(Command::new("swiftc")
                .arg(root.join("crates/recog-live/macos/recog_permission_probe.swift"))
                .arg("-o")
                .arg(&target))?;
            fs::rename(target, root.join("bin/recog-permission-probe"))?;
        }
        if !has("--no-services") && manager.backend == recog_install::units::Backend::Launchd {
            guide_permissions(root, config)?;
        }
    }
    if !has("--no-prefetch")
        && let Err(e) = run(Command::new(&bin).arg("prefetch"))
    {
        eprintln!("Model prefetch: {e}; transcription will retry");
    }
    if !has("--no-services") {
        let running = NAMES
            .iter()
            .map(|n| manager.state(n).map(|s| matches!(s, State::Running(_))))
            .collect::<io::Result<Vec<_>>>()?;
        run(Command::new(&bin).args(["service", "install"]))?;
        for row in checks(root, &std::env::var_os("PATH").unwrap_or_default(), manager.backend) {
            if !row.ready {
                eprintln!("{}: {}", row.name, row.hint);
                if row.required {
                    return Err("Install incomplete; services left untouched".into());
                }
            }
        }
        run(Command::new(&bin).arg("check"))?;
        for (name, restart) in NAMES.iter().zip(running) {
            manager.activate(name, restart)?;
        }
        println!("Live transcript: http://127.0.0.1:9080");
    }
    Ok(())
}

pub(super) fn archive_setup(
    package: &Path,
    root: &Path,
    config: &InstallConfig,
    env: &Environ,
    args: &[String],
) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if args == ["--permissions"] {
        return guide_permissions(root, config);
    }
    if !args.iter().all(|a| {
        matches!(
            a.as_str(),
            "--force" | "--no-services" | "--no-env" | "--no-extension" | "--no-prefetch" | "--dry-run"
        )
    }) {
        return Err(HELP.into());
    }
    if args.iter().any(|a| a == "--dry-run") {
        println!(
            "Would install release {} in {}",
            fs::read_to_string(package.join("recog-release"))?.trim(),
            root.display()
        );
        return Ok(());
    }
    let existing = manager()?.unit("live");
    if existing.is_file() {
        let unit = fs::read_to_string(existing)?;
        let path = root.display().to_string();
        let xml = path.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let owns = unit
            .lines()
            .any(|line| line == format!("WorkingDirectory={path}") || line.trim() == format!("<string>{xml}</string>"));
        if !owns {
            return Err("The installed live service belongs to another checkout. Update it there with recog update. Preserve its settings and data before switching installations.".into());
        }
    }
    let checked = install::check(root, config, env);
    if checked.code != 0 {
        return Err(format!("{}{}", checked.stdout, checked.stderr).into());
    }
    guard(root, config, env, args.iter().any(|a| a == "--force"))?;
    recog_install::distribution::prepare(package, root, !args.iter().any(|a| a == "--no-extension"))?;
    finish(root, config, env, args, true)
}

pub(super) fn update(root: &Path, config: &InstallConfig, env: &Environ, args: &[String]) -> Result<()> {
    if args == ["--help"] || args == ["-h"] {
        println!("usage: recog update [--force]");
        return Ok(());
    }
    if !args.is_empty() && args != ["--force"] {
        return Err("usage: recog update [--force]".into());
    }
    guard(root, config, env, args == ["--force"])?;
    if recog_install::distribution::is_release(root) {
        if root.join("recog-homebrew").is_file() {
            run(Command::new("brew").arg("update"))?;
            run(Command::new("brew").args(["upgrade", "materkey/apps/recog"]))?;
            let prefix = Command::new("brew").args(["--prefix", "recog"]).output()?;
            if !prefix.status.success() {
                return Err("Cannot locate the Homebrew installation".into());
            }
            let program = Path::new(std::str::from_utf8(&prefix.stdout)?.trim()).join("libexec/bin/recog");
            return run(Command::new(program).arg("setup").args(args));
        }
        return super::release_update::update(root, args);
    }
    let old = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(root).output()?;
    if !old.status.success() {
        return Err("Cannot resolve checkout revision".into());
    }
    run(Command::new("git").args(["pull", "--ff-only"]).current_dir(root))?;
    run(Command::new("git")
        .args([
            "diff",
            "--no-color",
            "--unified=0",
            std::str::from_utf8(&old.stdout)?.trim(),
            "HEAD",
            "--",
            "CHANGELOG.md",
        ])
        .current_dir(root))?;
    run(Command::new("cargo").arg("setup").args(args).current_dir(root))
}
