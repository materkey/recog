//! `TmuxRunner`: the tmux and systemd commands it runs (tests/test_copilot_supervisor.py,
//! "TmuxRunner", the session-name and Linux sections, and the model/effort tests of
//! test_copilot_ai_settings.py and test_claude_effort.py).
#![forbid(unsafe_code)]

mod common;

use std::rc::Rc;
use std::time::Duration;

use common::*;
use recog_ai::defaults::DEFAULT_CLAUDE_MODEL;
use recog_ai::install::LegacyEnv;
use recog_copilot::ai_settings::AiSettingsFile;
use recog_copilot::log;
use recog_copilot::supervisor::tmux::{KILL_RETRIES, KILL_RETRY_DELAY, SYSTEMD_SESSION_UNIT, session_manager_for};
use recog_copilot::supervisor::{
    LAUNCH_OPTION, MANUAL_MEETING_ID, Runner, TMUX_SESSION, TMUX_SESSION_MANUAL, TmuxRunner,
};

fn split(command: &str) -> Vec<String> {
    // The command is shlex.join'ed; a tiny POSIX splitter for the quoting it uses.
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            '\'' => {
                in_token = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    current.push(q);
                }
            }
            '"' => {
                in_token = true;
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                    current.push(q);
                }
            }
            c => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    tokens
}

fn after<'a>(tokens: &'a [String], flag: &str) -> &'a str {
    &tokens[tokens.iter().position(|t| t == flag).expect(flag) + 1]
}

#[test]
fn claude_command_carries_every_flag_and_an_mcp_config_that_survives_quoting_as_json() {
    // given
    let runner = tmux_runner(FakeTmux::default());
    // when
    let tokens = split(&runner.build_claude_command().unwrap());
    // then
    assert_eq!(tokens[0], "claude");
    assert_eq!(tokens[1..3], ["--agent", "copilot-session"]);
    assert!(tokens.iter().any(|t| t == "--strict-mcp-config"));
    let config: serde_json::Value = serde_json::from_str(after(&tokens, "--mcp-config")).unwrap();
    assert_eq!(config, frame(r#"{"mcpServers": {"copilot": {"type": "http", "url": "http://127.0.0.1:8767/mcp"}}}"#));
    assert_eq!(after(&tokens, "--dangerously-load-development-channels"), "server:copilot");
}

fn settings_file(dir: &std::path::Path, body: &str) -> Rc<AiSettingsFile> {
    std::fs::write(dir.join("ai-settings.json"), body).unwrap();
    Rc::new(AiSettingsFile::new(dir, LegacyEnv::default()))
}

const REVISION: &str = "0b1e4c52-4f0a-4b7e-9d5e-3c2f6a7b8c9d";

#[test]
fn model_change_in_the_settings_is_picked_up_by_the_next_command() {
    // given: a global model, then a Copilot override
    let dir = tempfile::tempdir().unwrap();
    let global =
        format!(r#"{{"version": 1, "revision": "{REVISION}", "models": {{"claude": "global-model"}}, "tasks": {{}}}}"#);
    let settings = settings_file(dir.path(), &global);
    let runner = TmuxRunner::new(FakeTmux::default(), settings, "/repo", "http://x/mcp");
    // when
    let first = split(&runner.build_claude_command().unwrap());
    let own = format!(
        r#"{{"version": 1, "revision": "{REVISION}", "models": {{"claude": "global-model"}},
        "tasks": {{"copilot": {{"models": {{"claude": "copilot-model"}}}}}}}}"#
    );
    std::fs::write(dir.path().join("ai-settings.json"), own).unwrap();
    let second = split(&runner.build_claude_command().unwrap());
    // then
    assert_eq!(after(&first, "--model"), "global-model");
    assert_eq!(after(&second, "--model"), "copilot-model");
    assert_eq!(after(&second, "--agent"), "copilot-session");
}

#[test]
fn effort_change_in_the_settings_applies_to_the_next_command_and_disappears_when_reset() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let base = format!(r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{}}}}"#);
    let settings = settings_file(dir.path(), &base);
    let runner = TmuxRunner::new(FakeTmux::default(), settings, "/repo", "http://x/mcp");
    // when / then
    assert!(!split(&runner.build_claude_command().unwrap()).iter().any(|t| t == "--effort"));
    let max = format!(
        r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"copilot": {{"claude_effort": "max"}}}}}}"#
    );
    std::fs::write(dir.path().join("ai-settings.json"), max).unwrap();
    assert_eq!(after(&split(&runner.build_claude_command().unwrap()), "--effort"), "max");
    std::fs::write(dir.path().join("ai-settings.json"), base).unwrap();
    assert!(!split(&runner.build_claude_command().unwrap()).iter().any(|t| t == "--effort"));
}

#[test]
fn model_with_shell_characters_reaches_claude_as_one_literal_argument() {
    // given
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"copilot": {{"models": {{"claude": "custom$[literal]{{model}}"}}}}}}}}"#
    );
    let runner = TmuxRunner::new(FakeTmux::default(), settings_file(dir.path(), &body), "/repo", "http://x/mcp");
    // when
    let command = runner.build_claude_command().unwrap();
    // then
    assert!(command.contains("--model 'custom$[literal]{model}'"), "{command}");
}

#[test]
fn disabled_copilot_refuses_to_build_the_command() {
    // given
    let settings = TestSettings::enabled();
    settings.set_enabled(false);
    let runner = TmuxRunner::new(FakeTmux::default(), settings, "/repo", "http://x/mcp");
    // when
    let error = runner.build_claude_command().unwrap_err();
    // then
    assert!(error.0.contains("выключен"), "{error}");
}

#[test]
fn missing_tool_names_tmux_first_then_the_cli_of_any_copilot_provider() {
    // given: tmux and Claude Code installed
    let mut runner = tmux_runner(FakeTmux::default());
    // when / then
    assert_eq!(runner.missing_tool(), None);
    runner.cli_installed = Box::new(|name| name == "codex");
    assert_eq!(runner.missing_tool(), None);
    runner.cli_installed = Box::new(|_| false);
    assert_eq!(runner.missing_tool(), Some("Claude Code or Codex"));
    runner.tmux_installed = Box::new(|| false);
    assert_eq!(runner.missing_tool(), Some("tmux"));
}

#[tokio::test]
async fn spawn_probes_the_sibling_and_the_target_then_creates_the_session_on_the_socket_in_the_checkout() {
    // given
    let runner =
        TmuxRunner::new(FakeTmux::with_session(false), TestSettings::enabled(), "/tmp/checkout", "http://x/mcp");
    // when
    let created = runner.spawn(Some("A")).await.unwrap();
    // then
    let fake = runner.command_runner();
    assert!(created);
    assert_eq!(fake.subcommands(), ["has-session", "has-session", "new-session", "set-option"]);
    let new_session = fake.calls.borrow()[2].clone();
    assert_eq!(new_session[..3], ["tmux", "-L", "recog-copilot"]);
    assert_eq!(new_session[3..9], ["new-session", "-d", "-s", "copilot", "-c", "/tmp/checkout"]);
    assert_eq!(new_session[9], runner.build_command().unwrap());
}

#[tokio::test]
async fn spawn_records_the_launch_on_the_session_it_created() {
    // given
    let runner = tmux_runner(FakeTmux::with_session(false));
    // when
    runner.spawn(Some(MANUAL_MEETING_ID)).await.unwrap();
    // then
    let record = runner.command_runner().calls.borrow().last().cloned().unwrap();
    let launch = format!("claude/{DEFAULT_CLAUDE_MODEL}/");
    assert_eq!(record, ["tmux", "-L", "recog-copilot", "set-option", "-t", "=copilot-manual:", LAUNCH_OPTION, &launch]);
}

#[tokio::test]
async fn spawn_whose_launch_cannot_be_recorded_still_reports_the_session_created() {
    // given
    let fake = FakeTmux::with_session(false);
    fake.respond("set-option", 1, "no server running");
    let runner = tmux_runner(fake);
    let logs = log::Capture::start();
    // when
    let created = runner.spawn(Some("A")).await.unwrap();
    // then
    assert!(created);
    assert!(logs.text().contains("tmux set-option failed (rc=1): no server running"), "{}", logs.text());
}

#[test]
fn launch_names_the_provider_that_runs_with_its_model_and_effort() {
    // given
    let mut runner = tmux_runner(FakeTmux::default());
    // when / then
    assert_eq!(runner.launch().unwrap(), format!("claude/{DEFAULT_CLAUDE_MODEL}/"));
    runner.cli_installed = Box::new(|name| name == "codex");
    assert_eq!(runner.launch().unwrap(), "codex/gpt-6-astra/xhigh");
}

#[tokio::test]
async fn recorded_launch_when_the_settings_change_the_model_then_outdated() {
    // given
    let settings = TestSettings::enabled();
    let fake = FakeTmux::default();
    fake.respond("show-options", 0, &format!("claude/{DEFAULT_CLAUDE_MODEL}/\n"));
    let mut runner = TmuxRunner::new(fake, settings.clone(), "/repo", "http://x/mcp");
    runner.cli_installed = Box::new(|name| name == "claude");
    // when / then
    assert!(!runner.launch_outdated().await.unwrap());
    settings.set_model("claude-sonnet-5-5");
    assert!(runner.launch_outdated().await.unwrap());
    let show = runner.command_runner().calls.borrow().last().cloned().unwrap();
    assert_eq!(show, ["tmux", "-L", "recog-copilot", "show-options", "-t", "=copilot:", "-v", "-q", LAUNCH_OPTION]);
}

#[tokio::test]
async fn session_without_a_recorded_launch_is_never_outdated() {
    // given: a session started before the record existed, and one tmux cannot read
    for (rc, out) in [(0, ""), (1, "no server running")] {
        let fake = FakeTmux::default();
        fake.respond("show-options", rc, out);
        let settings = TestSettings::enabled();
        settings.set_model("claude-sonnet-5-5");
        let runner = TmuxRunner::new(fake, settings, "/repo", "http://x/mcp");
        // when / then
        assert!(!runner.launch_outdated().await.unwrap(), "{out:?}");
    }
}

#[tokio::test]
async fn spawn_with_a_live_session_adopts_it_without_new_session_or_kill() {
    // given
    let runner = tmux_runner(FakeTmux::named(&[TMUX_SESSION]));
    // when
    let created = runner.spawn(Some("A")).await.unwrap();
    // then
    assert!(!created);
    assert_eq!(runner.command_runner().subcommands(), ["has-session", "has-session"]);
    assert!(runner.command_runner().kill_targets().is_empty());
}

#[tokio::test]
async fn spawn_failing_in_tmux_raises_new_session_failed() {
    // given
    let fake = FakeTmux::default();
    fake.respond("new-session", 1, "error connecting to socket");
    let runner = tmux_runner(fake);
    // when
    let error = runner.spawn(Some("A")).await.unwrap_err();
    // then
    assert_eq!(error.0, "tmux new-session failed (rc=1): error connecting to socket");
}

#[tokio::test]
async fn spawn_derives_the_session_name_from_the_meeting_id() {
    // given
    let runner = tmux_runner(FakeTmux::named(&[]));
    // when / then
    runner.spawn(Some(MANUAL_MEETING_ID)).await.unwrap();
    assert_eq!(runner.session_name(), TMUX_SESSION_MANUAL);
    runner.spawn(Some("A")).await.unwrap();
    assert_eq!(runner.session_name(), TMUX_SESSION);
    assert_eq!(runner.command_runner().new_session_names(), [TMUX_SESSION_MANUAL, TMUX_SESSION]);
}

#[tokio::test]
async fn conference_spawn_with_a_stale_manual_session_reaps_it_and_creates_the_default_one() {
    // given
    let runner = tmux_runner(FakeTmux::named(&[TMUX_SESSION_MANUAL]));
    // when
    let created = runner.spawn(Some("B")).await.unwrap();
    // then
    let fake = runner.command_runner();
    assert!(created);
    assert_eq!(fake.kill_targets(), [TMUX_SESSION_MANUAL]);
    assert_eq!(fake.new_session_names(), [TMUX_SESSION]);
    assert_eq!(fake.live(), [TMUX_SESSION.to_owned()].into());
    assert_eq!(runner.session_name(), TMUX_SESSION);
}

#[tokio::test]
async fn manual_spawn_with_a_stale_default_session_reaps_it_and_creates_the_manual_one() {
    // given
    let runner = tmux_runner(FakeTmux::named(&[TMUX_SESSION]));
    // when
    let created = runner.spawn(Some(MANUAL_MEETING_ID)).await.unwrap();
    // then
    let fake = runner.command_runner();
    assert!(created);
    assert_eq!(fake.kill_targets(), [TMUX_SESSION]);
    assert_eq!(fake.new_session_names(), [TMUX_SESSION_MANUAL]);
    assert_eq!(fake.live(), [TMUX_SESSION_MANUAL.to_owned()].into());
}

#[tokio::test]
async fn spawn_whose_stale_sibling_cannot_be_killed_fails_before_creating_its_session() {
    // given
    let fake = FakeTmux::named(&[TMUX_SESSION_MANUAL]);
    fake.respond("kill-session", 1, "boom");
    let runner = tmux_runner(fake);
    // when
    let error = runner.spawn(Some("B")).await.unwrap_err();
    // then
    assert!(error.0.contains("stale sibling session 'copilot-manual'"), "{error}");
    assert!(runner.command_runner().new_session_names().is_empty());
    assert_eq!(runner.command_runner().live(), [TMUX_SESSION_MANUAL.to_owned()].into());
}

#[tokio::test]
async fn alive_answers_for_each_named_session_and_defaults_to_the_current_name() {
    // given
    let runner = tmux_runner(FakeTmux::named(&[TMUX_SESSION_MANUAL]));
    // when / then
    assert!(runner.alive(Some(TMUX_SESSION_MANUAL)).await.unwrap());
    assert!(!runner.alive(Some(TMUX_SESSION)).await.unwrap());
    assert!(!runner.alive(None).await.unwrap());
}

#[tokio::test]
async fn alive_follows_has_session() {
    // given / when / then
    assert!(tmux_runner(FakeTmux::with_session(true)).alive(None).await.unwrap());
    assert!(!tmux_runner(FakeTmux::with_session(false)).alive(None).await.unwrap());
}

#[tokio::test]
async fn kill_failing_with_the_session_already_gone_returns_without_retrying() {
    // given
    let fake = FakeTmux::default();
    fake.respond("kill-session", 1, "no server running");
    let runner = tmux_runner(fake);
    // when
    runner.kill(None).await.unwrap();
    // then
    let fake = runner.command_runner();
    assert_eq!(fake.calls.borrow()[0], ["tmux", "-L", "recog-copilot", "kill-session", "-t", "=copilot"]);
    assert_eq!(fake.subcommands(), ["kill-session", "has-session"]);
}

#[test]
fn kill_retry_policy_stays_three_attempts_half_a_second_apart() {
    // given / when / then
    assert_eq!(KILL_RETRIES, 3);
    assert_eq!(KILL_RETRY_DELAY, Duration::from_millis(500));
}

fn sequenced(kills: &[(i32, &str)], has_session: bool, has_session_results: Option<Vec<bool>>) -> FakeTmux {
    let fake = FakeTmux::with_session(has_session);
    *fake.kill_results.borrow_mut() = kills.iter().map(|(rc, out)| (*rc, (*out).to_owned())).collect();
    *fake.has_session_results.borrow_mut() = has_session_results;
    fake
}

#[tokio::test]
async fn kill_failing_once_while_alive_retries_and_succeeds() {
    // given
    let runner = tmux_runner(sequenced(&[(1, "server busy"), (0, "")], true, None));
    // when
    runner.kill(None).await.unwrap();
    // then
    assert_eq!(runner.command_runner().subcommands(), ["kill-session", "has-session", "kill-session"]);
}

#[tokio::test]
async fn kill_stops_retrying_once_the_session_disappears_after_a_later_attempt() {
    // given
    let runner =
        tmux_runner(sequenced(&[(1, "busy"), (1, "already gone"), (1, "must not run")], true, Some(vec![true, false])));
    // when
    runner.kill(None).await.unwrap();
    // then
    assert_eq!(runner.command_runner().subcommands(), ["kill-session", "has-session", "kill-session", "has-session"]);
}

#[tokio::test]
async fn kill_failing_while_the_session_stays_alive_raises_after_the_bounded_retries() {
    // given
    let runner = tmux_runner(sequenced(&[(1, "cannot kill\n"); 5], true, None));
    // when
    let error = runner.kill(None).await.unwrap_err();
    // then
    assert_eq!(error.0, "tmux kill-session failed (rc=1): cannot kill");
    let kills = runner.command_runner().subcommands().iter().filter(|s| *s == "kill-session").count();
    assert_eq!(kills, KILL_RETRIES as usize);
}

#[tokio::test(start_paused = true)]
async fn kill_pacing_sleeps_only_between_attempts() {
    // given: the real retry delay, on the paused clock
    let paced = |fake| {
        let mut runner = tmux_runner(fake);
        runner.kill_retry_delay = KILL_RETRY_DELAY;
        runner
    };
    let elapsed = |runner: TmuxRunner<FakeTmux>| async move {
        let start = tokio::time::Instant::now();
        let result = runner.kill(None).await;
        (start.elapsed(), result.is_ok())
    };
    // when / then: success at once and the session-gone path do not sleep
    assert_eq!(elapsed(paced(sequenced(&[(0, "")], true, None))).await, (Duration::ZERO, true));
    assert_eq!(elapsed(paced(sequenced(&[(1, "gone")], false, None))).await, (Duration::ZERO, true));
    // two retries sleep twice; exhaustion sleeps KILL_RETRIES - 1 times
    assert_eq!(
        elapsed(paced(sequenced(&[(1, "busy"), (1, "busy"), (0, "")], true, None))).await,
        (KILL_RETRY_DELAY * 2, true)
    );
    assert_eq!(
        elapsed(paced(sequenced(&[(1, "stuck"); 5], true, None))).await,
        (KILL_RETRY_DELAY * (KILL_RETRIES - 1), false)
    );
}

#[tokio::test]
async fn send_keys_and_capture_target_the_pane_with_the_trailing_colon() {
    // given
    let fake = FakeTmux::default();
    fake.respond("capture-pane", 0, "❯ prompt line");
    let runner = tmux_runner(fake);
    // when
    runner.send_keys(&["1".to_owned()]).await.unwrap();
    let text = runner.capture().await.unwrap();
    // then
    let calls = runner.command_runner().calls.borrow().clone();
    assert_eq!(calls[0], ["tmux", "-L", "recog-copilot", "send-keys", "-t", "=copilot:", "1"]);
    assert_eq!(calls[1], ["tmux", "-L", "recog-copilot", "capture-pane", "-p", "-t", "=copilot:"]);
    assert_eq!(text, "❯ prompt line");
}

#[tokio::test]
async fn runner_bound_to_the_manual_name_targets_exactly_that_pane_and_session() {
    // given
    let fake = FakeTmux::default();
    fake.respond("capture-pane", 0, "❯ prompt line");
    let runner = tmux_runner(fake).with_session_name(TMUX_SESSION_MANUAL);
    // when
    runner.send_keys(&["1".to_owned()]).await.unwrap();
    let text = runner.capture().await.unwrap();
    runner.alive(None).await.unwrap();
    // then
    let calls = runner.command_runner().calls.borrow().clone();
    assert_eq!(calls[0], ["tmux", "-L", "recog-copilot", "send-keys", "-t", "=copilot-manual:", "1"]);
    assert_eq!(calls[1], ["tmux", "-L", "recog-copilot", "capture-pane", "-p", "-t", "=copilot-manual:"]);
    assert_eq!(calls[2], ["tmux", "-L", "recog-copilot", "has-session", "-t", "=copilot-manual"]);
    assert_eq!(text, "❯ prompt line");
}

#[tokio::test]
async fn capture_of_a_gone_session_returns_empty_and_logs_the_refusal() {
    // given
    log::reset_rate_limits();
    let fake = FakeTmux::default();
    fake.respond("capture-pane", 1, "can't find pane: =copilot-manual");
    let runner = tmux_runner(fake);
    let capture = log::Capture::start();
    // when
    let text = runner.capture().await.unwrap();
    // then
    assert_eq!(text, "");
    assert!(capture.text().to_lowercase().contains("capture"), "{}", capture.text());
}

#[tokio::test]
async fn send_keys_refused_by_tmux_is_logged() {
    // given
    log::reset_rate_limits();
    let fake = FakeTmux::default();
    fake.respond("send-keys", 1, "can't find pane: =copilot-manual");
    let runner = tmux_runner(fake);
    let capture = log::Capture::start();
    // when
    runner.send_keys(&["1".to_owned()]).await.unwrap();
    // then
    assert!(capture.text().contains("send-keys"), "{}", capture.text());
}

/// A throwaway tmux server, never the service's `recog-copilot` socket.
struct TestServer(String);

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux").args(["-L", &self.0, "kill-server"]).output();
    }
}

#[tokio::test]
async fn capture_and_send_keys_drive_a_real_tmux_session_named_with_the_default_as_prefix() {
    // given
    if recog_copilot::supervisor::tmux::which("tmux").is_none() {
        eprintln!("tmux not installed: skipped");
        return;
    }
    let server = TestServer(format!("recog-test-{}", std::process::id()));
    // Started through the production runner: the tmux server it forks must not keep
    // the output pipe open, or every spawn would hang on reading it.
    let argv: Vec<String> =
        ["tmux", "-L", &server.0, "new-session", "-d", "-s", TMUX_SESSION_MANUAL, "cat"].map(str::to_owned).into();
    let created = tokio::time::timeout(
        Duration::from_secs(10),
        recog_copilot::supervisor::CommandRunner::run(&recog_copilot::supervisor::ProcessRunner::default(), &argv),
    )
    .await
    .expect("new-session returned")
    .unwrap();
    assert_eq!(created.0, 0, "{created:?}");
    let real = |name: &str| {
        let mut runner = TmuxRunner::new(
            recog_copilot::supervisor::ProcessRunner::default(),
            TestSettings::enabled(),
            "/tmp",
            "http://x/mcp",
        )
        .with_session_name(name);
        runner.socket_name = server.0.clone();
        runner
    };
    let runner = real(TMUX_SESSION_MANUAL);
    // when
    assert!(runner.alive(None).await.unwrap());
    runner.send_keys(&["RECOG_MARKER".to_owned()]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let screen = runner.capture().await.unwrap();
    // then
    assert!(screen.contains("RECOG_MARKER"), "{screen:?}");
    // the default name must not resolve to this session: exactness matters
    assert!(!real(TMUX_SESSION).alive(None).await.unwrap());
}

#[tokio::test]
async fn launch_recorded_on_a_real_tmux_session_reads_back_for_that_session_only() {
    // given
    if recog_copilot::supervisor::tmux::which("tmux").is_none() {
        eprintln!("tmux not installed: skipped");
        return;
    }
    let server = TestServer(format!("recog-test-launch-{}", std::process::id()));
    let settings = TestSettings::enabled();
    let real = |name: &str| {
        let mut runner = TmuxRunner::new(
            recog_copilot::supervisor::ProcessRunner::default(),
            settings.clone(),
            "/tmp",
            "http://x/mcp",
        )
        .with_session_name(name);
        runner.socket_name = server.0.clone();
        runner.cli_installed = Box::new(|name| name == "claude");
        runner
    };
    for name in [TMUX_SESSION, TMUX_SESSION_MANUAL] {
        let argv: Vec<String> =
            ["tmux", "-L", &server.0, "new-session", "-d", "-s", name, "cat"].map(str::to_owned).into();
        let created = tokio::time::timeout(
            Duration::from_secs(10),
            recog_copilot::supervisor::CommandRunner::run(&recog_copilot::supervisor::ProcessRunner::default(), &argv),
        )
        .await
        .expect("new-session returned")
        .unwrap();
        assert_eq!(created.0, 0, "{created:?}");
    }
    let launch = real(TMUX_SESSION).launch().unwrap();
    let record: Vec<String> =
        ["tmux", "-L", &server.0, "set-option", "-t", "=copilot:", LAUNCH_OPTION, &launch].map(str::to_owned).into();
    let recorded =
        recog_copilot::supervisor::CommandRunner::run(&recog_copilot::supervisor::ProcessRunner::default(), &record)
            .await
            .unwrap();
    assert_eq!(recorded.0, 0, "{recorded:?}");
    // when
    settings.set_model("claude-sonnet-5-5");
    // then: the recorded session is outdated, the one without a record is not
    assert!(real(TMUX_SESSION).launch_outdated().await.unwrap());
    assert!(!real(TMUX_SESSION_MANUAL).launch_outdated().await.unwrap());
}

#[test]
fn session_manager_is_the_transient_unit_only_on_linux_with_systemd_run() {
    // given / when / then
    assert!(session_manager_for("linux", |_| true));
    assert!(!session_manager_for("linux", |_| false));
    assert!(!session_manager_for("macos", |_| true));
}

fn linux_runner(fake: LinuxFake) -> TmuxRunner<LinuxFake> {
    let mut runner = tmux_runner(fake);
    runner.use_systemd_run = true;
    runner
}

#[tokio::test]
async fn linux_spawn_wraps_tmux_in_a_transient_unit_with_its_properties_and_environment() {
    // given
    let mut runner =
        TmuxRunner::new(LinuxFake::new(&[], false), TestSettings::enabled(), "/tmp/checkout", "http://x/mcp");
    runner.use_systemd_run = true;
    runner.session_env = vec![("PATH".into(), "/custom/bin".into()), ("TERM".into(), "xterm-256color".into())];
    // when
    let created = runner.spawn(Some("A")).await.unwrap();
    // then
    assert!(created);
    let argv = runner.command_runner().calls.borrow().iter().find(|c| c[0] == "systemd-run").cloned().unwrap();
    let has = |a: &str| argv.iter().any(|x| x == a);
    assert_eq!(argv[0], "systemd-run");
    assert!(has("--user") && has("--collect"));
    assert!(has(&format!("--unit={SYSTEMD_SESSION_UNIT}")));
    assert!(has("--property=Type=oneshot"));
    assert!(has("--property=RemainAfterExit=yes"));
    assert!(has("--property=KillMode=control-group"));
    assert!(has("--working-directory=/tmp/checkout"));
    assert!(has("--setenv=PATH=/custom/bin"));
    assert!(has("--setenv=TERM=xterm-256color"));
    assert!(!argv.iter().any(|a| a.starts_with("--property=Restart=")));
    let tmux = &argv[argv.iter().position(|a| a == "tmux").unwrap()..];
    assert_eq!(tmux[..4], ["tmux", "-L", "recog-copilot", "new-session"]);
    assert_eq!(tmux[4..9], ["-d", "-s", TMUX_SESSION, "-c", "/tmp/checkout"]);
    assert_eq!(tmux[9], runner.build_command().unwrap());
}

#[tokio::test]
async fn linux_spawn_adopting_a_live_session_leaves_the_unit_alone() {
    // given
    let runner = linux_runner(LinuxFake::new(&[TMUX_SESSION], true));
    // when
    let created = runner.spawn(Some("A")).await.unwrap();
    // then
    assert!(!created);
    assert!(!runner.command_runner().programs().iter().any(|p| p == "systemd-run"));
    assert!(runner.command_runner().systemctl_verbs().is_empty());
}

#[tokio::test]
async fn linux_spawn_stops_a_leftover_unit_and_confirms_it_gone_before_systemd_run() {
    // given
    let runner = linux_runner(LinuxFake::new(&[], true));
    // when
    runner.spawn(Some("A")).await.unwrap();
    // then
    let fake = runner.command_runner();
    let stop = fake.index_of(|c| c[..3] == ["systemctl", "--user", "stop"]);
    let run = fake.index_of(|c| c[0] == "systemd-run");
    assert!(stop < run);
    assert!(fake.calls.borrow()[stop..run].iter().any(|c| c[0] == "systemctl" && c.iter().any(|a| a == "show")));
}

#[tokio::test]
async fn linux_spawn_whose_unit_never_goes_away_raises_instead_of_running_systemd_run() {
    // given
    let fake = LinuxFake::new(&[], false);
    *fake.load_states.borrow_mut() = Some(vec!["loaded".into(), "loaded".into()]);
    let mut runner = linux_runner(fake);
    runner.unit_stop_timeout = Duration::ZERO;
    // when
    let error = runner.spawn(Some("A")).await.unwrap_err();
    // then
    assert!(error.0.contains("still loaded"), "{error}");
    assert!(!runner.command_runner().programs().iter().any(|p| p == "systemd-run"));
}

#[tokio::test]
async fn linux_kill_stops_the_transient_unit_after_the_session() {
    // given
    let runner = linux_runner(LinuxFake::new(&[TMUX_SESSION], true));
    // when
    runner.kill(None).await.unwrap();
    // then
    let fake = runner.command_runner();
    assert_eq!(fake.calls.borrow()[0][..4], ["tmux", "-L", "recog-copilot", "kill-session"]);
    assert!(fake.systemctl_verbs().iter().any(|v| v == "stop"));
}

#[tokio::test]
async fn linux_kill_of_one_kind_leaves_the_unit_alone_while_the_other_lives() {
    // given
    let runner = linux_runner(LinuxFake::new(&[TMUX_SESSION, TMUX_SESSION_MANUAL], true));
    // when
    runner.kill(Some(TMUX_SESSION_MANUAL)).await.unwrap();
    // then
    assert_eq!(runner.command_runner().calls.borrow()[0][..4], ["tmux", "-L", "recog-copilot", "kill-session"]);
    assert!(runner.command_runner().systemctl_verbs().is_empty());
}

#[tokio::test]
async fn linux_kill_without_a_loaded_unit_only_asks_systemd() {
    // given
    let runner = linux_runner(LinuxFake::new(&[TMUX_SESSION], false));
    // when
    runner.kill(None).await.unwrap();
    // then
    assert_eq!(runner.command_runner().systemctl_verbs(), ["show"]);
}

#[tokio::test]
async fn linux_spawn_whose_systemd_run_fails_is_not_reported_as_created() {
    // given
    let fake = LinuxFake::new(&[], false);
    fake.responses.borrow_mut().insert("systemd-run".into(), (1, "Failed to start transient service unit".into()));
    let runner = linux_runner(fake);
    // when
    let error = runner.spawn(Some("A")).await.unwrap_err();
    // then
    assert!(error.0.contains("new-session failed"), "{error}");
}

#[tokio::test]
async fn macos_path_runs_only_tmux() {
    // given
    let runner = tmux_runner(LinuxFake::new(&[TMUX_SESSION], false));
    // when
    runner.kill(None).await.unwrap();
    runner.command_runner().sessions.borrow_mut().clear();
    runner.spawn(Some("A")).await.unwrap();
    // then
    assert!(runner.command_runner().programs().iter().all(|p| p == "tmux"));
}

#[test]
fn only_codex_installed_when_the_session_command_is_built_then_the_codex_harness_in_the_checkout() {
    // given
    let mut runner = tmux_runner(FakeTmux::default());
    runner.cli_installed = Box::new(|name| name == "codex");
    // when
    let tokens = split(&runner.build_command().unwrap());
    // then
    assert!(tokens[0].ends_with("/recog-copilot-codex"), "{tokens:?}");
    assert_eq!(tokens[1..], ["--repo-root", "/repo"]);
}

#[test]
fn both_installed_when_built_then_claude_code_unless_codex_is_saved_alone() {
    // given
    let settings = TestSettings::enabled();
    let mut runner = TmuxRunner::new(FakeTmux::default(), settings.clone(), "/repo", "http://x/mcp");
    runner.cli_installed = Box::new(|_| true);
    // when / then
    assert_eq!(split(&runner.build_command().unwrap())[0], "claude");
    settings.set_providers(&["codex"]);
    assert!(split(&runner.build_command().unwrap())[0].ends_with("/recog-copilot-codex"));
}

#[test]
fn neither_installed_when_built_then_the_first_provider_as_before_codex() {
    // given
    let mut runner = tmux_runner(FakeTmux::default());
    runner.cli_installed = Box::new(|_| false);
    // when / then
    assert_eq!(split(&runner.build_command().unwrap())[0], "claude");
}

#[test]
fn disabled_codex_copilot_refuses_to_build_the_command() {
    // given
    let settings = TestSettings::enabled();
    settings.set_enabled(false);
    let mut runner = TmuxRunner::new(FakeTmux::default(), settings, "/repo", "http://x/mcp");
    runner.cli_installed = Box::new(|name| name == "codex");
    // when
    let error = runner.build_command().unwrap_err();
    // then
    assert!(error.0.contains("выключен"), "{error}");
}
