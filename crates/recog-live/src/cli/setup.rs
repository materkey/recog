//! Build a checkout, then let its freshly built binary install it.
use std::fs;
use std::io;
#[cfg(target_os = "macos")]
use std::io::IsTerminal;
#[cfg(any(target_os = "macos", test))]
use std::io::Write;
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

#[cfg(target_os = "macos")]
fn confirm_permission() -> Result<bool> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    read_confirmation(stdin.is_terminal() && stdout.is_terminal(), stdin.lock(), stdout.lock())
}

#[cfg(any(target_os = "macos", test))]
fn read_confirmation(interactive: bool, mut input: impl io::BufRead, mut output: impl Write) -> Result<bool> {
    if !interactive {
        return Ok(false);
    }
    write!(output, "Нажмите Enter для запроса или введите s, чтобы пропустить: ")?;
    output.flush()?;
    let mut answer = String::new();
    Ok(input.read_line(&mut answer)? > 0 && answer.trim().is_empty())
}

#[cfg(any(target_os = "macos", test))]
fn guide_grant(
    label: &str,
    granted: bool,
    hint: &str,
    confirm: impl FnOnce() -> Result<bool>,
    request: impl FnOnce() -> Result<bool>,
) -> Result<()> {
    if granted {
        println!("{label}: доступ выдан.");
        return Ok(());
    }
    println!("{label}: требуется разрешение.");
    if confirm()? && request()? {
        println!("{label}: доступ выдан.");
    } else {
        println!("{hint}");
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn permissions(_: &Path, _: &InstallConfig, _: bool) -> Result<()> {
    Err("macOS permission setup is available on a Mac".into())
}

#[cfg(target_os = "macos")]
fn permissions(root: &Path, config: &InstallConfig, explicit: bool) -> Result<()> {
    if super::macos_permissions::local_terminal() {
        match super::macos_permissions::window(root, config, explicit) {
            Ok(()) => return Ok(()),
            Err(e) => eprintln!("Не удалось открыть окно разрешений: {e}. Продолжаем в терминале."),
        }
    }
    guide_permissions(root, config)
}

#[cfg(target_os = "macos")]
fn guide_permissions(root: &Path, config: &InstallConfig) -> Result<()> {
    use recog_install::permissions::probe;
    let grants = probe(root, None)?;
    guide_grant(
        "Recog — микрофон для вашей речи",
        grants.microphone == "granted",
        "Настройки системы → Конфиденциальность и безопасность → Микрофон → Recog. Затем повторите recog setup --permissions.",
        confirm_permission,
        || Ok(probe(root, Some("microphone"))?.microphone == "granted"),
    )?;
    guide_grant(
        "Recog — запись экрана и системного звука для речи других участников",
        grants.system_audio == "granted",
        "Настройки системы → Конфиденциальность и безопасность → Запись экрана и системного звука → Recog. Затем повторите recog setup --permissions.",
        confirm_permission,
        || Ok(probe(root, Some("system-audio"))?.system_audio == "granted"),
    )?;
    if config.live.zoom_app {
        guide_zoom_access(
            root,
            &config.macos.codesign_identity,
            confirm_permission,
            recog_zoom::helper_link::accessibility,
        )?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn zoom_helper(root: &Path, identity: &str) -> Result<PathBuf> {
    Ok(recog_zoom::helper_app::find_or_build_helper(
        root,
        if identity.is_empty() { "-" } else { identity },
        &recog_zoom::helper_app::SystemToolchain,
        &recog_zoom::log::Logger::stderr(),
    )?)
}

#[cfg(any(target_os = "macos", test))]
fn guide_zoom_access(
    root: &Path,
    identity: &str,
    confirm: impl FnOnce() -> Result<bool>,
    probe: impl Fn(&Path, bool) -> std::result::Result<bool, recog_zoom::helper_link::LaunchError>,
) -> Result<()> {
    let app = zoom_helper(root, identity)?;
    guide_grant(
        "Recog Zoom Helper — универсальный доступ для настольного Zoom",
        probe(&app, false)?,
        "Настройки системы → Конфиденциальность и безопасность → Универсальный доступ → Recog Zoom Helper. Затем запустите recog doctor.",
        confirm,
        || Ok(probe(&app, true)?),
    )
}

pub(super) fn setup(root: &Path, config: &InstallConfig, env: &Environ, args: &[String], built: bool) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return Ok(());
    }
    if args == ["--permissions"] {
        return permissions(root, config, true);
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
    #[cfg(target_os = "macos")]
    {
        use crate::capture::sck::{self, OsToolchain};
        if !packaged {
            sck::find_or_build_sck_utility(root, &OsToolchain, &crate::log::Logger::stderr())
                .map_err(|e| e.to_string())?;
        }
        run(Command::new(&bin).arg("launcher"))?;
        if !packaged {
            build_permission_tools(root)?;
        }
        if !has("--no-services") && manager.backend == recog_install::units::Backend::Launchd {
            permissions(root, config, false)?;
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

#[cfg(target_os = "macos")]
fn build_permission_tools(root: &Path) -> Result<()> {
    let stage = tempfile::tempdir_in(root.join("bin"))?;
    for (source, name) in [
        ("recog_permission_probe.swift", "recog-permission-probe"),
        ("recog_permissions_window.swift", "recog-permissions"),
    ] {
        let target = stage.path().join(name);
        run(Command::new("swiftc").arg(root.join("crates/recog-live/macos").join(source)).arg("-o").arg(&target))?;
        fs::rename(target, root.join("bin").join(name))?;
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
        return permissions(root, config, true);
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
    finish(root, config, env, args, true)?;
    if !args.iter().any(|a| a == "--no-services") {
        watch_package_upgrades(&manager()?)?;
        recog_install::distribution::record_services_release(root)?;
    }
    Ok(())
}

/// A Linux package installs `recog-upgrade.path` for every user; it reruns
/// this setup after an upgrade. The service is only enabled, for the check at
/// login: starting it here would wait on itself when it is what runs setup.
fn watch_package_upgrades(manager: &Manager) -> Result<()> {
    use recog_install::services::{UPGRADE_SERVICE, UPGRADE_WATCH};
    if manager.backend != recog_install::units::Backend::Systemd
        || !Path::new("/usr/lib/systemd/user").join(UPGRADE_WATCH).is_file()
    {
        return Ok(());
    }
    run(Command::new("systemctl").args(["--user", "enable", UPGRADE_SERVICE]))?;
    run(Command::new("systemctl").args(["--user", "enable", "--now", UPGRADE_WATCH]))
}

pub(super) fn update(root: &Path, config: &InstallConfig, env: &Environ, args: &[String]) -> Result<()> {
    if args == ["--help"] || args == ["-h"] {
        println!("usage: recog update [--force]");
        return Ok(());
    }
    if !args.is_empty() && args != ["--force"] {
        return Err("usage: recog update [--force]".into());
    }
    if let Some(manager) = recog_install::distribution::package_manager(root) {
        return Err(package_update(&manager).into());
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

/// How to update a release that a system package installed: the package
/// manager owns the files, and `recog-upgrade.path` moves the services over.
fn package_update(manager: &str) -> String {
    let command = match manager {
        "apt" => "sudo apt update && sudo apt upgrade",
        "pacman" => "sudo pacman -Syu",
        _ => "your package manager",
    };
    format!(
        "Recog was installed as a system package. Update it with {command}; the services switch to the new version by themselves."
    )
}

#[cfg(test)]
mod package_tests {
    use super::*;

    #[test]
    fn package_install_when_updated_then_the_package_manager_command_is_named() {
        assert!(package_update("apt").contains("sudo apt update && sudo apt upgrade"));
        assert!(package_update("pacman").contains("sudo pacman -Syu"));
        assert!(package_update("dnf").contains("your package manager"));
    }
}

#[cfg(test)]
mod grant_tests {
    use super::*;
    #[test]
    fn terminal_input_when_reading_confirmation_then_only_enter_requests_a_grant() {
        for (terminal, input, want) in
            [(false, "\n", false), (true, "", false), (true, "s\n", false), (true, "\n", true)]
        {
            let mut out = Vec::new();
            assert_eq!(read_confirmation(terminal, input.as_bytes(), &mut out).unwrap(), want);
            assert_eq!(!out.is_empty(), terminal);
        }
    }

    #[test]
    fn installed_zoom_helper_when_access_is_missing_then_only_confirmation_requests_access() {
        use recog_zoom::helper_app::{HELPER_APP, HELPER_SOURCE, HELPER_STAMP};
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join(HELPER_SOURCE);
        let app = dir.path().join(HELPER_APP);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(source, b"test").unwrap();
        let signature = app.join("Contents/_CodeSignature/CodeResources");
        fs::create_dir_all(signature.parent().unwrap()).unwrap();
        fs::write(signature, b"fixture").unwrap();
        fs::write(dir.path().join(HELPER_STAMP), format!("{:x}", Sha256::digest(b"test"))).unwrap();
        for confirm in [false, true] {
            let calls = std::cell::RefCell::new(Vec::new());
            guide_zoom_access(
                dir.path(),
                "",
                || Ok(confirm),
                |path, prompt| {
                    assert_eq!(path, app);
                    calls.borrow_mut().push(prompt);
                    Ok(prompt)
                },
            )
            .unwrap();
            assert_eq!(calls.into_inner(), if confirm { vec![false, true] } else { vec![false] });
        }
    }
    #[test]
    fn permission_step_when_granted_skipped_or_requested_then_only_a_confirmed_missing_grant_prompts() {
        for (granted, confirm, expect_request) in [(true, false, false), (false, false, false), (false, true, true)] {
            for answer in [false, true] {
                let called = std::cell::Cell::new(false);
                guide_grant(
                    "Recog",
                    granted,
                    "settings",
                    || {
                        assert!(!granted);
                        Ok(confirm)
                    },
                    || {
                        called.set(true);
                        Ok(answer)
                    },
                )
                .unwrap();
                assert_eq!(called.get(), expect_request);
            }
        }
        assert!(guide_grant("Recog", false, "settings", || Ok(true), || Err("probe failed".into())).is_err());
    }
}
