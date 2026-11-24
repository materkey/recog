//! Fakes shared by the supervisor tests, the counterparts of the Python test doubles
//! (`FakeRunner`, `FakeTmux`, `NameAwareTmux`, `SequencedKillTmux`, `LinuxFake`).
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::rc::Rc;

use recog_ai::defaults::DEFAULT_CLAUDE_MODEL;
use recog_copilot::ai_settings::{CopilotSettings, CopilotTask};
use recog_copilot::supervisor::{CommandRunner, Error, Runner, Supervisor, TmuxRunner, Tunables};

/// The settings file of an installation from before the AI functions level: no
/// `access`, so the Copilot runs at the automatic level.
pub fn pre_change_settings(data_dir: &std::path::Path) {
    std::fs::create_dir_all(data_dir).unwrap();
    let revision = uuid::Uuid::new_v4();
    let text = format!(r#"{{"version": 1, "revision": "{revision}", "models": {{}}, "tasks": {{}}}}"#);
    std::fs::write(data_dir.join("ai-settings.json"), text).unwrap();
}

/// Runs `f` on a `LocalSet`, where the supervisor spawns its tasks.
pub async fn local<F: Future>(f: F) -> F::Output {
    tokio::task::LocalSet::new().run_until(f).await
}

/// Lets every ready task run (`await asyncio.sleep(0)` a few times).
pub async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

/// Polls `condition` every 10 ms, up to a second, as the Python tests' loops do.
pub async fn wait_for(condition: impl Fn() -> bool) -> bool {
    for _ in 0..100 {
        if condition() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    condition()
}

/// The Copilot switch, set by the test.
pub struct TestSettings(pub RefCell<Result<CopilotTask, String>>);

impl TestSettings {
    pub fn enabled() -> Rc<Self> {
        Rc::new(TestSettings(RefCell::new(Ok(CopilotTask {
            enabled: true,
            automatic: true,
            providers: vec!["claude".into(), "codex".into()],
            model: DEFAULT_CLAUDE_MODEL.to_owned(),
            claude_effort: String::new(),
            codex_model: "gpt-6-astra".into(),
            reasoning_effort: "xhigh".into(),
        }))))
    }

    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(task) = self.0.borrow_mut().as_mut() {
            task.enabled = enabled;
        }
    }

    /// The `automatic` level (true) or `manual` (false).
    pub fn set_automatic(&self, automatic: bool) {
        if let Ok(task) = self.0.borrow_mut().as_mut() {
            task.automatic = automatic;
        }
    }

    pub fn set_providers(&self, providers: &[&str]) {
        if let Ok(task) = self.0.borrow_mut().as_mut() {
            task.providers = providers.iter().map(|provider| (*provider).to_owned()).collect();
        }
    }
}

impl CopilotSettings for TestSettings {
    fn resolve(&self) -> Result<CopilotTask, String> {
        self.0.borrow().clone()
    }
}

/// Records spawn/kill calls (`FakeRunner`); no session is ever alive and the watchdog
/// does not observe it, like the Python fake without `alive`/`capture`/`send_keys`.
#[derive(Default)]
pub struct FakeRunner {
    pub spawned: RefCell<Vec<Option<String>>>,
    pub kills: Cell<usize>,
    pub session: RefCell<String>,
    pub fail_kill: Cell<bool>,
    pub fail_alive: Cell<bool>,
    /// What `missing_tool` reports.
    pub missing: Cell<Option<&'static str>>,
    /// While set, `spawn` waits for this notify (blocking-runner tests).
    pub gates: RefCell<HashMap<String, Rc<Gate>>>,
}

/// One blocked spawn: `started` once it runs, released by `release`.
#[derive(Default)]
pub struct Gate {
    pub started: tokio::sync::Notify,
    pub started_flag: Cell<bool>,
    pub release: tokio::sync::Notify,
    pub released: Cell<bool>,
}

impl Gate {
    pub async fn wait_started(&self) {
        while !self.started_flag.get() {
            self.started.notified().await;
        }
    }

    pub fn open(&self) {
        self.released.set(true);
        self.release.notify_waiters();
    }
}

impl FakeRunner {
    pub fn new() -> Rc<Self> {
        Rc::new(FakeRunner { session: RefCell::new("copilot".into()), ..Default::default() })
    }

    pub fn spawned(&self) -> Vec<Option<String>> {
        self.spawned.borrow().clone()
    }

    pub fn gate(&self, meeting_id: &str) -> Rc<Gate> {
        let gate = Rc::new(Gate::default());
        self.gates.borrow_mut().insert(meeting_id.to_owned(), gate.clone());
        gate
    }
}

/// `["A", "A"]` as the spawn list the Python tests compare.
pub fn ids(names: &[&str]) -> Vec<Option<String>> {
    names.iter().map(|n| Some((*n).to_owned())).collect()
}

impl Runner for FakeRunner {
    async fn spawn(&self, meeting_id: Option<&str>) -> Result<bool, Error> {
        self.spawned.borrow_mut().push(meeting_id.map(str::to_owned));
        let gate = meeting_id.and_then(|id| self.gates.borrow().get(id).cloned());
        if let Some(gate) = gate {
            gate.started_flag.set(true);
            gate.started.notify_waiters();
            while !gate.released.get() {
                gate.release.notified().await;
            }
        }
        Ok(true)
    }

    async fn kill(&self, _session_name: Option<&str>) -> Result<(), Error> {
        if self.fail_kill.get() {
            return Err(Error("tmux kill-session failed (rc=1): boom".into()));
        }
        self.kills.set(self.kills.get() + 1);
        Ok(())
    }

    async fn alive(&self, _session_name: Option<&str>) -> Result<bool, Error> {
        if self.fail_alive.get() {
            return Err(Error("tmux unavailable".into()));
        }
        Ok(false)
    }

    async fn capture(&self) -> Result<String, Error> {
        Ok(String::new())
    }

    async fn send_keys(&self, _keys: &[String]) -> Result<(), Error> {
        Ok(())
    }

    fn session_name(&self) -> String {
        self.session.borrow().clone()
    }

    fn set_session_name(&self, name: &str) {
        *self.session.borrow_mut() = name.to_owned();
    }

    fn observable(&self) -> bool {
        false
    }

    fn missing_tool(&self) -> Option<&'static str> {
        self.missing.get()
    }
}

/// A runner whose session is alive while spawned, as `Runner` in test_copilot_toggle.py.
#[derive(Default)]
pub struct LiveRunner {
    pub running: Cell<bool>,
    pub spawns: RefCell<Vec<Option<String>>>,
    pub kills: Cell<usize>,
}

impl Runner for LiveRunner {
    async fn spawn(&self, meeting_id: Option<&str>) -> Result<bool, Error> {
        self.running.set(true);
        self.spawns.borrow_mut().push(meeting_id.map(str::to_owned));
        Ok(true)
    }

    async fn kill(&self, _session_name: Option<&str>) -> Result<(), Error> {
        self.running.set(false);
        self.kills.set(self.kills.get() + 1);
        Ok(())
    }

    async fn alive(&self, _session_name: Option<&str>) -> Result<bool, Error> {
        Ok(self.running.get())
    }

    async fn capture(&self) -> Result<String, Error> {
        Ok(String::new())
    }

    async fn send_keys(&self, _keys: &[String]) -> Result<(), Error> {
        Ok(())
    }

    fn session_name(&self) -> String {
        "copilot".into()
    }

    fn set_session_name(&self, _name: &str) {}

    fn observable(&self) -> bool {
        false
    }
}

/// Recorded tmux invocations and the per-name sessions they act on: `FakeTmux`,
/// `NameAwareTmux` and `SequencedKillTmux` in one programmable fake.
#[derive(Default)]
pub struct FakeTmux {
    pub calls: RefCell<Vec<Vec<String>>>,
    /// `None`: every name is alive or dead together (`has_session`), as `FakeTmux`.
    pub sessions: RefCell<Option<BTreeSet<String>>>,
    pub has_session: Cell<bool>,
    /// tmux subcommand -> (rc, output), as `responses`.
    pub responses: RefCell<HashMap<String, (i32, String)>>,
    pub kill_results: RefCell<Vec<(i32, String)>>,
    pub has_session_results: RefCell<Option<Vec<bool>>>,
    /// While set, kill-session blocks here (the in-flight grace kill test).
    pub kill_gate: RefCell<Option<Rc<Gate>>>,
}

impl FakeTmux {
    pub fn with_session(has_session: bool) -> Self {
        let fake = FakeTmux::default();
        fake.has_session.set(has_session);
        fake
    }

    pub fn named(sessions: &[&str]) -> Self {
        let fake = FakeTmux::default();
        *fake.sessions.borrow_mut() = Some(sessions.iter().map(|s| (*s).to_owned()).collect());
        fake
    }

    pub fn respond(&self, sub: &str, rc: i32, out: &str) {
        self.responses.borrow_mut().insert(sub.to_owned(), (rc, out.to_owned()));
    }

    pub fn subcommands(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c[3].clone()).collect()
    }

    fn target(args: &[String], flag: &str) -> String {
        let i = args.iter().position(|a| a == flag).expect("flag present");
        args[i + 1].trim_start_matches('=').to_owned()
    }

    pub fn kill_targets(&self) -> Vec<String> {
        self.calls.borrow().iter().filter(|c| c[3] == "kill-session").map(|c| Self::target(c, "-t")).collect()
    }

    pub fn new_session_names(&self) -> Vec<String> {
        self.calls.borrow().iter().filter(|c| c[3] == "new-session").map(|c| Self::target(c, "-s")).collect()
    }

    pub fn live(&self) -> BTreeSet<String> {
        self.sessions.borrow().clone().unwrap_or_default()
    }

    fn is_alive(&self, name: &str) -> bool {
        match &*self.sessions.borrow() {
            Some(sessions) => sessions.contains(name),
            None => self.has_session.get(),
        }
    }

    fn set_alive(&self, name: &str, alive: bool) {
        match &mut *self.sessions.borrow_mut() {
            Some(sessions) => {
                if alive {
                    sessions.insert(name.to_owned());
                } else {
                    sessions.remove(name);
                }
            }
            None => self.has_session.set(alive),
        }
    }
}

impl CommandRunner for FakeTmux {
    async fn run(&self, args: &[String]) -> Result<(i32, String), Error> {
        self.calls.borrow_mut().push(args.to_vec());
        let sub = args[3].as_str();
        if sub == "kill-session" {
            let next = {
                let mut results = self.kill_results.borrow_mut();
                (!results.is_empty()).then(|| results.remove(0))
            };
            if let Some(result) = next {
                return Ok(result);
            }
        }
        if sub == "has-session"
            && let Some(results) = self.has_session_results.borrow_mut().as_mut()
            && !results.is_empty()
        {
            let alive = results.remove(0);
            return Ok(if alive { (0, String::new()) } else { (1, "can't find session: copilot".into()) });
        }
        if let Some(response) = self.responses.borrow().get(sub) {
            return Ok(response.clone());
        }
        match sub {
            "has-session" => {
                let name = Self::target(args, "-t");
                Ok(if self.is_alive(&name) { (0, String::new()) } else { (1, format!("can't find session: {name}")) })
            }
            "new-session" => {
                self.set_alive(&Self::target(args, "-s"), true);
                Ok((0, String::new()))
            }
            "kill-session" => {
                self.set_alive(&Self::target(args, "-t"), false);
                let gate = self.kill_gate.borrow().clone();
                if let Some(gate) = gate {
                    gate.started_flag.set(true);
                    gate.started.notify_waiters();
                    while !gate.released.get() {
                        gate.release.notified().await;
                    }
                }
                Ok((0, String::new()))
            }
            _ => Ok((0, String::new())),
        }
    }
}

/// Records systemd-run / systemctl / tmux invocations (`LinuxFake`): `loaded` is
/// systemd's view of the transient unit, which a stop clears.
#[derive(Default)]
pub struct LinuxFake {
    pub calls: RefCell<Vec<Vec<String>>>,
    pub sessions: RefCell<BTreeSet<String>>,
    pub loaded: Cell<bool>,
    pub load_states: RefCell<Option<Vec<String>>>,
    /// program -> (rc, output).
    pub responses: RefCell<HashMap<String, (i32, String)>>,
}

impl LinuxFake {
    pub fn new(sessions: &[&str], loaded: bool) -> Self {
        let fake = LinuxFake::default();
        *fake.sessions.borrow_mut() = sessions.iter().map(|s| (*s).to_owned()).collect();
        fake.loaded.set(loaded);
        fake
    }

    pub fn programs(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|c| c[0].clone()).collect()
    }

    pub fn systemctl_verbs(&self) -> Vec<String> {
        self.calls.borrow().iter().filter(|c| c[0] == "systemctl").map(|c| c[2].clone()).collect()
    }

    pub fn index_of(&self, predicate: impl Fn(&[String]) -> bool) -> usize {
        self.calls.borrow().iter().position(|c| predicate(c)).expect("a matching call")
    }
}

impl CommandRunner for LinuxFake {
    async fn run(&self, args: &[String]) -> Result<(i32, String), Error> {
        self.calls.borrow_mut().push(args.to_vec());
        let head = args[0].as_str();
        if head == "systemctl" {
            if args.iter().any(|a| a == "show") {
                let state = match self.load_states.borrow_mut().as_mut().filter(|s| !s.is_empty()) {
                    Some(states) => states.remove(0),
                    None => if self.loaded.get() { "loaded" } else { "not-found" }.to_owned(),
                };
                return Ok((0, format!("{state}\n")));
            }
            if args.iter().any(|a| a == "stop") {
                self.loaded.set(false);
            }
            return Ok((0, String::new()));
        }
        if let Some(response) = self.responses.borrow().get(head) {
            return Ok(response.clone());
        }
        if head == "tmux" && args[3] == "has-session" {
            let name = args[5].trim_start_matches('=');
            return Ok(if self.sessions.borrow().contains(name) {
                (0, String::new())
            } else {
                (1, "can't find session".into())
            });
        }
        Ok((0, String::new()))
    }
}

pub type FakeSupervisor = Rc<Supervisor<FakeRunner>>;

/// `Supervisor(runner=FakeRunner(), grace_s=..., autospawn=...)`.
pub fn fake_supervisor(grace_s: f64, autospawn: bool) -> (FakeSupervisor, Rc<FakeRunner>) {
    let runner = FakeRunner::new();
    let sup =
        Supervisor::new("ws://127.0.0.1:1", runner.clone(), TestSettings::enabled(), autospawn, Tunables::new(grace_s));
    (sup, runner)
}

/// `Supervisor(runner=TmuxRunner(run=fake), ...)`.
pub fn tmux_supervisor(fake: FakeTmux, grace_s: f64, autospawn: bool) -> Rc<Supervisor<TmuxRunner<FakeTmux>>> {
    Supervisor::new(
        "ws://127.0.0.1:1",
        Rc::new(tmux_runner(fake)),
        TestSettings::enabled(),
        autospawn,
        Tunables::new(grace_s),
    )
}

/// `TmuxRunner(run=fake)` with the default MCP URL and the current directory.
pub fn tmux_runner<C: CommandRunner>(fake: C) -> TmuxRunner<C> {
    let mut runner = TmuxRunner::new(fake, TestSettings::enabled(), "/repo", "http://127.0.0.1:8767/mcp");
    runner.kill_retry_delay = std::time::Duration::ZERO;
    // Whatever this machine has installed, the tests' session runs Claude Code in tmux.
    runner.cli_installed = Box::new(|name| name == "claude");
    runner.tmux_installed = Box::new(|| true);
    runner
}

pub fn frame(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("a JSON test frame")
}

// Fixtures approximate real claude 2.1.197 rendered panes (spike captures), as in
// tests/test_copilot_supervisor.py.
pub const DEV_CHANNELS_SCREEN: &str = "╭──────────────────────────────────────────────╮\n\
│ Load local development channels?             │\n\
│                                              │\n\
│ ❯ 1. Yes, load them                          │\n\
│   2. No                                      │\n\
│                                              │\n\
│ Enter to confirm · Esc to cancel             │\n\
╰──────────────────────────────────────────────╯\n";
pub const MODEL_DIALOG_SCREEN: &str = "╭──────────────────────────────────────────────╮\n\
│ Select a model                               │\n\
│                                              │\n\
│ ❯ 1. Default (recommended)                   │\n\
│   2. Opus                                    │\n\
│   3. Sonnet                                  │\n\
│                                              │\n\
│ Enter to confirm · Esc to cancel             │\n\
╰──────────────────────────────────────────────╯\n";
pub const REPL_PROMPT_SCREEN: &str = " ▐▛███▜▌   Claude Code v2.1.197\n ▝▜█████▛▘\n\n\
╭──────────────────────────────────────────────╮\n\
│ ❯                                            │\n\
╰──────────────────────────────────────────────╯\n  ? for shortcuts\n";
pub const SPINNER_SCREEN: &str = "✻ Baking… (12s · esc to interrupt)\n\n\
╭──────────────────────────────────────────────╮\n\
│ ❯                                            │\n\
╰──────────────────────────────────────────────╯\n";
pub const STARTUP_SCREEN: &str = "\n\n  Starting up…\n";
/// Captured live on claude 2.1.270 (2026-10-01): no option numbers, the cursor on
/// "No, exit".
pub const TRUST_DIALOG_SCREEN: &str = " Accessing workspace:\n\n /Users/user/projects/recog\n\n \
Quick safety check: Is this a project you created or one you trust? (Like your\n \
own code, a well-known open source project, or work from your team). If not,\n \
take a moment to review what's in this folder first.\n\n \
Claude Code'll be able to read, edit, and execute files here.\n\n Security guide\n\n \
❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n";
/// Captured live on claude 2.1.197+ (2026-08-14).
pub const TRUST_DIALOG_NUMBERED_SCREEN: &str = " Accessing workspace:\n\n /Users/user/projects/recog\n\n \
Quick safety check: Is this a project you created or one you trust?\n\n \
Claude Code'll be able to read, edit, and execute files here.\n\n \
⚠ This folder pre-approves 1 tool permission in .claude/settings.local.json:\n   \
mcp__plugin_mem0_mem0__search_memories\n\n ❯ 1. Yes, I trust this folder\n   2. No, exit\n\n \
Enter to confirm · Esc to cancel\n";
/// Captured live on claude 2.1.322 (2026-11-12): the session's model out of usage,
/// every channel event refused, the prompt idle below.
pub const USAGE_LIMIT_SCREEN: &str = "← copilot: Privacy mode: use speaker_N labels instead of real names. T…\n  \
⎿  You've reached your Fable limit. Run /usage-credits to continue or switch\n     \
models with /model.\n\n✻ Brewed for 0s · done 12:26 PM\n\n\
← copilot: Privacy mode: use speaker_N labels instead of real names. T…\n  \
⎿  You've reached your Fable limit. Run /usage-credits to continue or switch\n     \
models with /model.\n\n✻ Churned for 0s · done 12:26 PM\n\n\
──────────────────────────────────────────────────── copilot-session ─\n\
❯ \n\
──────────────────────────────────────────────────────────────────────\n  \
⎇ main (1569e07) · ctx: 0% · Fable 5.2 [high] · /Users/user/projects/r…\n  \
⏵⏵ bypass permissions on (shift+tab to cycle) · ← 1 agent\n";
/// The same session after `/model`: the refusals still on screen, a reply below them.
pub const REPLY_AFTER_USAGE_LIMIT_SCREEN: &str = "← copilot: Privacy mode: use speaker_N labels instead of real names. T…\n  \
⎿  You've reached your Fable limit. Run /usage-credits to continue or switch\n     \
models with /model.\n\n✻ Churned for 0s · done 12:26 PM\n\n\
❯ /model claude-opus-5-5\n  ⎿  Set model to Opus 5.5\n\n\
← copilot: Privacy mode: use speaker_N labels instead of real names. T…\n\n\
⏺ Called copilot\n\n⏺ Ответил в чат Copilot: обсуждают PR с Gradle-флагом;\n  \
you've hit your target only if CI runs coverage.\n\n✻ Cogitated for 23s · done 12:27 PM\n\n\
──────────────────────────────────────────────────── copilot-session ─\n\
❯ \n\
──────────────────────────────────────────────────────────────────────\n  \
⎇ main (1569e07) · ctx: 23% · Opus 5.5 [high] · /Users/user/projects/r…\n";
