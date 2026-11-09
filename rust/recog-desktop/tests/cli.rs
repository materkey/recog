//! cli.rs and the `recog-desktop` binary on the cases tests/differential.rs gave
//! both implementations, with what Python printed, exited with and wrote stated.
#![forbid(unsafe_code)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use recog_core::pyjson::Value;
use recog_desktop::cli::{self, Exit};
use recog_desktop::host::{Host, Platform, ScriptedClock};
use recog_desktop::storage;

fn argv(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

const DESKTOP_USAGE: &str = "usage: recog desktop [-h] {enable,disable,attach,status} ...\n";
const TOP_USAGE: &str = "usage: recog [-h] {live,meetings,desktop} ...\n";

fn failure(usage: &str, prog: &str, message: &str) -> Exit {
    Exit { code: 2, stdout: String::new(), stderr: format!("{usage}{prog}: error: {message}\n") }
}

fn help_of(name: &str) -> Exit {
    let text = format!("usage: recog desktop {name} [-h]\n\noptions:\n  -h, --help  show this help message and exit\n");
    Exit { code: 0, stdout: text, stderr: String::new() }
}

#[test]
fn malformed_desktop_arguments_when_parsed_then_argparse_answers() {
    // given
    let enable = "usage: recog desktop enable [-h] --app APP\n";
    let status = "usage: recog desktop status [-h] [--json]\n";
    let cases = [
        ("-h", None),
        ("--help", None),
        ("--x status", Some(failure(TOP_USAGE, "recog", "unrecognized arguments: --x"))),
        ("enable --app", Some(failure(enable, "recog desktop enable", "argument --app: expected one argument"))),
        ("enable --app --json", Some(failure(enable, "recog desktop enable", "argument --app: expected one argument"))),
        ("disable -h", Some(help_of("disable"))),
        ("attach --he", Some(help_of("attach"))),
        (
            "status --json=1",
            Some(failure(status, "recog desktop status", "argument --json: ignored explicit argument '1'")),
        ),
        ("disable extra", Some(failure(TOP_USAGE, "recog", "unrecognized arguments: extra"))),
        ("status -x", Some(failure(TOP_USAGE, "recog", "unrecognized arguments: -x"))),
        (
            "bogus",
            Some(failure(
                DESKTOP_USAGE,
                "recog desktop",
                "argument desktop_command: invalid choice: 'bogus' (choose from enable, disable, attach, status)",
            )),
        ),
    ];
    for (line, expected) in cases {
        // when
        let exit = cli::parse(&argv(line)).unwrap_err();
        // then
        match expected {
            Some(expected) => assert_eq!(exit, expected, "{line}"),
            None => {
                assert_eq!(exit.code, 0, "{line}");
                assert!(exit.stdout.starts_with(DESKTOP_USAGE) && exit.stdout.contains("Подключить работающий Толк"));
            }
        }
    }
    let help = cli::parse(&argv("enable -h")).unwrap_err();
    assert!(help.stdout.contains("--app APP   Путь к официальному AppImage или .app"), "{help:?}");
    let spaced = vec!["status".to_owned(), "-a b".to_owned()];
    assert_eq!(cli::parse(&spaced).unwrap_err(), failure(TOP_USAGE, "recog", "unrecognized arguments: -a b"));
}

#[test]
fn option_values_when_parsed_then_taken_as_argparse_takes_them() {
    // given / when / then
    assert_eq!(cli::parse(&argv("enable --app -5")).unwrap(), cli::Command::Enable { app: "-5".into() });
    assert_eq!(cli::parse(&argv("enable --app=")).unwrap(), cli::Command::Enable { app: String::new() });
    assert_eq!(cli::parse(&argv("status")).unwrap(), cli::Command::Status { json: false });
    assert_eq!(cli::parse(&argv("disable")).unwrap(), cli::Command::Disable);
}

struct Side {
    dir: tempfile::TempDir,
    world: World,
}

impl Side {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let world = World::new(&dir.path().join("world"));
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        std::fs::create_dir_all(dir.path().join("repo/browser-extension")).unwrap();
        Self { dir, world }
    }

    fn path(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    fn root(&self) -> PathBuf {
        self.path().join("data/desktop")
    }

    fn host(&self) -> Host {
        let mut host = host(&self.world, free_port(), free_port(), &self.path().join("home"));
        // The fake application is built for this platform (a bundle on macOS).
        host.platform = Platform::current();
        host.clock = ScriptedClock::new(4e9 + 1.0, &[0.0]);
        host
    }

    fn write(&self, name: &str, contents: &str) {
        std::fs::create_dir_all(self.root()).unwrap();
        std::fs::write(self.root().join(name), contents).unwrap();
    }

    /// The fake application, and the extension built in the fake checkout.
    fn built_app(&self) -> FakeApp {
        let app = fake_app(&self.path(), "FakeTalk");
        std::fs::rename(dist(&self.path()), self.path().join("repo/browser-extension/dist")).unwrap();
        app
    }

    async fn run(&self, line: &str) -> cli::Outcome {
        let command = cli::parse(&argv(line)).unwrap();
        cli::main(&self.host(), &command, &self.path().join("data"), &self.path().join("repo")).await
    }

    async fn run_with(&self, line: &str, last: &Path) -> cli::Outcome {
        let mut args = argv(line);
        args.push(last.to_string_lossy().into_owned());
        let command = cli::parse(&args).unwrap();
        cli::main(&self.host(), &command, &self.path().join("data"), &self.path().join("repo")).await
    }
}

fn printed(code: i32, stdout: &str) -> cli::Outcome {
    cli::Outcome { code, stdout: stdout.to_owned(), stderr: String::new() }
}

fn refused(message: &str) -> cli::Outcome {
    printed(1, &format!("Desktop integration: {message}\n"))
}

const FRESH_HEARTBEAT: &str = r#"{"running": true, "updated_at": 4000000000.0}"#;

#[tokio::test]
async fn configurations_when_commands_run_then_python_output_and_status() {
    // given
    let cases: Vec<(&str, Option<&str>, Option<&str>, cli::Outcome)> = vec![
        ("disable", Some("[1]"), None, refused("'list' object is not a mapping")),
        (
            "disable",
            Some("{broken"),
            None,
            refused("Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
        ),
        ("attach", None, None, refused("'app'")),
        (
            "attach",
            Some(r#"{"enabled": true, "app": "/nonexistent/KTalk.app"}"#),
            None,
            refused("[Errno 2] No such file or directory: '/nonexistent'"),
        ),
        (
            "attach",
            Some(r#"{"enabled": true, "app": 5}"#),
            None,
            refused("argument should be a str or an os.PathLike object where __fspath__ returns a str, not 'int'"),
        ),
        ("attach", Some("[]"), None, refused("list indices must be integers or slices, not str")),
        (
            "status",
            Some("{broken"),
            Some(r#"{"state": "inspector_unclosed", "reason": "cleanup_failed"}"#),
            printed(0, "inspector_unclosed: cleanup_failed\n"),
        ),
        (
            "status --json",
            Some(r#"{"enabled": true}"#),
            None,
            printed(
                0,
                "{\"enabled\": true, \"app_version\": null, \"reason\": \"web_server_not_running\", \"inspector_closed\": null, \"state\": \"server_offline\"}\n",
            ),
        ),
        ("status", Some("[]"), None, printed(0, "error: invalid_configuration\n")),
        ("status", Some(r#"{"enabled": true}"#), Some(r#"{"reason": "x"}"#), refused("'state'")),
        (
            "status",
            Some(r#"{"enabled": true}"#),
            Some(r#"{"state": 5, "reason": null}"#),
            refused("unsupported operand type(s) for +: 'int' and 'str'"),
        ),
        (
            "status",
            Some(r#"{"enabled": true}"#),
            Some(r#"{"state": [1], "reason": null}"#),
            refused("can only concatenate list (not \"str\") to list"),
        ),
        (
            "status",
            Some(r#"{"enabled": true}"#),
            Some(r#"{"state": "error", "reason": 7}"#),
            refused("can only concatenate str (not \"int\") to str"),
        ),
    ];
    for (line, config, status, expected) in cases {
        let side = Side::new();
        if let Some(config) = config {
            side.write("config.json", config);
        }
        if let Some(status) = status {
            side.write("status.json", status);
            side.write("heartbeat.json", FRESH_HEARTBEAT);
        }
        // when
        let outcome = side.run(line).await;
        // then
        assert_eq!(outcome, expected, "{line} {config:?} {status:?}");
    }
}

#[tokio::test]
async fn configuration_with_other_settings_when_disabled_then_the_rest_kept() {
    // given
    let side = Side::new();
    side.write("config.json", r#"{"enabled": true, "app": "/x", "bundle": "d", "extra": [1, "é"]}"#);
    // when
    let outcome = side.run("disable").await;
    // then
    assert_eq!(
        outcome,
        printed(0, "Автоподключение выключено. Уже загруженное расширение действует до выхода из Толка.\n")
    );
    assert_eq!(
        std::fs::read_to_string(side.root().join("config.json")).unwrap(),
        r#"{"enabled": false, "app": "/x", "bundle": "d", "extra": [1, "é"]}"#
    );
}

#[tokio::test]
async fn selected_applications_when_enabled_then_snapshot_configured_or_refused() {
    // given
    let side = Side::new();
    let app = side.built_app();
    // when
    let outcome = side.run_with("enable --app", &app.app).await;
    // then
    assert_eq!(outcome, printed(0, "Автоподключение включено. Оно работает, пока запущен web-сервер Recog.\n"));
    let config = storage::read(&side.root().join("config.json")).unwrap();
    let digest = text(&config, "bundle").unwrap();
    assert_eq!(field(&config, "enabled"), &Value::Bool(true));
    assert_eq!(text(&config, "app"), Some(app.app.to_string_lossy().into_owned()));
    assert!(side.root().join("extensions").join(&digest).is_dir());
    // a directory, an unbuilt checkout, a build with a link; on macOS a directory is
    // read as an application bundle
    let directory_refused = if cfg!(target_os = "macos") {
        let plist = std::fs::canonicalize(side.path()).unwrap().join("Contents/Info.plist");
        format!("[Errno 2] No such file or directory: '{}'", plist.display())
    } else {
        "Select an executable KTalk AppImage or binary".to_owned()
    };
    assert_eq!(side.run_with("enable --app", &side.path()).await, refused(&directory_refused));
    let unbuilt = Side::new();
    let unbuilt_app = fake_app(&unbuilt.path(), "FakeTalk");
    assert_eq!(
        unbuilt.run_with("enable --app", &unbuilt_app.app).await,
        refused("Build browser-extension before enabling desktop integration")
    );
    std::os::unix::fs::symlink("/etc/hosts", side.path().join("repo/browser-extension/dist/link")).unwrap();
    assert_eq!(
        side.run_with("enable --app", &app.app).await,
        refused("Extension snapshot cannot contain symbolic links")
    );
}

#[tokio::test]
async fn symlink_loop_selected_when_enabled_then_the_exception_line_on_stderr() {
    // given
    let side = Side::new();
    std::os::unix::fs::symlink(side.path().join("loop-b"), side.path().join("loop-a")).unwrap();
    std::os::unix::fs::symlink(side.path().join("loop-a"), side.path().join("loop-b")).unwrap();
    // when
    let outcome = side.run_with("enable --app", &side.path().join("loop-a")).await;
    // then
    assert_eq!(
        outcome,
        cli::Outcome {
            code: 1,
            stdout: String::new(),
            stderr: format!("RuntimeError: Symlink loop from '{}'\n", side.path().join("loop-a").display()),
        }
    );
}

// --- the binary --------------------------------------------------------------------------

fn binary(side: &Side, args: &[&str], data_dir: &str) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_recog-desktop"));
    command
        .args(args)
        .env("HOME", side.path().join("home"))
        .env("RECOG_CONFIG_FILE", side.path().join("config.toml"))
        .env("RECOG_DATA_DIR", data_dir)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("RECOG_CONFERENCE_HOSTS_FILE");
    command.output().unwrap()
}

fn status_of(output: &std::process::Output) -> (Option<i32>, String, String) {
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn binary_invocations_when_run_then_exit_status_and_output() {
    // given
    let side = Side::new();
    let repo = side.path().join("repo");
    let repo = repo.to_str().unwrap();
    let data = side.path().join("data");
    let data = data.to_str().unwrap();
    // when / then
    let (code, _, stderr) = status_of(&binary(&side, &["--repo-root"], data));
    assert_eq!((code, stderr.as_str()), (Some(2), "recog-desktop: --repo-root needs a directory\n"));
    let (code, _, stderr) = status_of(&binary(&side, &["--repo-root", repo, "worker"], data));
    assert_eq!(code, Some(2));
    assert!(stderr.starts_with("usage: worker.py"), "{stderr}");
    let (code, stdout, _) = status_of(&binary(&side, &["--repo-root", repo, "status", "--json"], data));
    assert_eq!(code, Some(0));
    assert_eq!(
        stdout,
        "{\"enabled\": false, \"state\": \"disabled\", \"reason\": null, \"app_version\": null, \"inspector_closed\": null}\n"
    );
    assert_eq!(status_of(&binary(&side, &["status"], data)), (Some(0), "disabled\n".to_owned(), String::new()));
    let (code, _, stderr) = status_of(&binary(&side, &["--repo-root", repo, "bogus"], data));
    assert_eq!(code, Some(2));
    assert!(stderr.contains("invalid choice: 'bogus'"), "{stderr}");
    let (code, _, stderr) = status_of(&binary(&side, &["--repo-root", repo, "status"], "relative"));
    assert_eq!((code, stderr.as_str()), (Some(1), "RECOG_DATA_DIR must be an absolute path (or start with ~/)\n"));
    std::fs::write(side.path().join("config.toml"), "[broken").unwrap();
    let (code, _, stderr) = status_of(&binary(&side, &["--repo-root", repo, "status"], data));
    assert_eq!(code, Some(1));
    assert!(stderr.contains("config.toml"), "{stderr}");
    assert!(!side.path().join("data/desktop").exists());
}
