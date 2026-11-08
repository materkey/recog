//! Differential tests: the same inputs through the Python supervisor
//! (`tests/oracle/supervisor_oracle.py`, run with the repository's interpreter) and
//! through the port, outputs compared exactly. Lifecycle event sequences, reconcile
//! inputs, watchdog tick scripts, frame sequences through the glue, AI settings files,
//! the Claude command line, the hello as the real bridge reads it, and the Python
//! builtins the port mirrors (truthiness, `str`, `repr`, `json.dumps`, `splitlines`,
//! `shlex.join`, the Unicode classes).
//!
//! The interpreter is `RECOG_ORACLE_PYTHON` (scripts/check.sh sets it), else
//! `venv-gigaam/bin/python` of the checkout; without either the tests say so and pass.
#![forbid(unsafe_code)]

mod common;

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::rc::Rc;
use std::time::Duration;

use common::*;
use futures_util::StreamExt;
use recog_copilot::ai_settings::{AiSettingsFile, CopilotSettings, LegacyEnv};
use recog_copilot::supervisor::watchdog::{self, Watchdog, WatchdogHost};
use recog_copilot::supervisor::{
    AgentState, Command, Error, Runner, SessionLifecycle, State, Supervisor, TmuxRunner, Tunables, classify_screen,
    reconcile, screen, tmux, trust_dialog_keys,
};
use recog_copilot::{config, py};
use serde_json::{Value, json};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The Python outputs for `cases`, or `None` without an interpreter.
fn oracle(cases: &[Value]) -> Option<Vec<Value>> {
    let Some(python) = oracle_python() else {
        eprintln!("no Python interpreter for the oracle (set RECOG_ORACLE_PYTHON): skipped");
        return None;
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oracle/supervisor_oracle.py");
    let mut child = Process::new(&python)
        .arg(&script)
        .arg(repo_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", python.display()));
    let input = serde_json::to_vec(cases).unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(output.status.success(), "the oracle failed:\n{}", String::from_utf8_lossy(&output.stderr));
    let answers: Vec<Value> = serde_json::from_slice(&output.stdout).expect("the oracle's JSON");
    assert_eq!(answers.len(), cases.len());
    Some(answers)
}

/// Compares case by case, reporting the first difference with its input.
fn assert_same(kind: &str, cases: &[Value], rust: &[Value], python: &[Value]) {
    for (i, (r, p)) in rust.iter().zip(python).enumerate() {
        assert_eq!(r, p, "{kind} case {i} differs\ninput: {}\nrust:   {r}\npython: {p}", cases[i]);
    }
    eprintln!("{kind}: {} cases identical", cases.len());
}

/// xorshift64*: reproducible inputs without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

fn state_of(name: &str) -> State {
    match name {
        "idle" => State::Idle,
        "spawning" => State::Spawning,
        "running" => State::Running,
        _ => State::Grace,
    }
}

fn snapshot(m: &SessionLifecycle) -> Value {
    json!({"state": m.state.as_str(), "meeting_id": m.meeting_id})
}

fn commands_json(commands: &[Command]) -> Value {
    Value::Array(
        commands
            .iter()
            .map(|c| match c {
                Command::Spawn(id) => json!(["spawn", id]),
                Command::Kill => json!(["kill"]),
            })
            .collect(),
    )
}

fn random_start(rng: &mut Rng) -> Value {
    let state = *rng.pick(&["idle", "spawning", "running", "grace"]);
    let id = if state == "idle" {
        Value::Null
    } else {
        rng.pick(&[json!("A"), json!("B"), json!("manual"), Value::Null]).clone()
    };
    json!({"state": state, "meeting_id": id})
}

fn lifecycle_of(start: &Value) -> SessionLifecycle {
    SessionLifecycle {
        state: state_of(start["state"].as_str().unwrap()),
        meeting_id: start["meeting_id"].as_str().map(str::to_owned),
    }
}

#[test]
fn lifecycle_event_sequences_give_the_same_commands_and_states_as_python() {
    // given: random walks over every input from every state
    let mut rng = Rng(0x5EED_0001);
    let ids = [json!("A"), json!("B"), json!("manual")];
    let maybe_ids = [json!("A"), json!("B"), json!("manual"), Value::Null];
    let cases: Vec<Value> = (0..400)
        .map(|_| {
            let events: Vec<Value> = (0..14)
                .map(|_| match rng.below(7) {
                    0 => json!(["meeting_started", rng.pick(&ids)]),
                    1 => json!(["meeting_ended", rng.pick(&maybe_ids)]),
                    2 => json!(["timer_expired", null]),
                    3 => json!(["session_died", null]),
                    4 => json!(["session_ready", null]),
                    5 => json!(["stop_now", rng.pick(&maybe_ids)]),
                    _ => json!(["adopt", rng.pick(&maybe_ids)]),
                })
                .collect();
            json!({"kind": "lifecycle", "start": random_start(&mut rng), "events": events})
        })
        .collect();
    // when
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| {
            let mut m = lifecycle_of(&case["start"]);
            let steps: Vec<Value> = case["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| {
                    let arg = event[1].as_str();
                    let commands = match event[0].as_str().unwrap() {
                        "meeting_started" => m.meeting_started(arg.unwrap()),
                        "meeting_ended" => m.meeting_ended(arg),
                        "timer_expired" => m.timer_expired(),
                        "session_died" => m.session_died(),
                        "session_ready" => m.session_ready(),
                        "stop_now" => m.stop_now(arg),
                        _ => m.adopt(arg),
                    };
                    let mut out = snapshot(&m);
                    out["commands"] = commands_json(&commands);
                    out
                })
                .collect();
            Value::Array(steps)
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("lifecycle", &cases, &rust, &python);
    }
}

/// Frame ids of every JSON type: Python's truthiness and `str()` decide what spawns.
fn odd_ids() -> Vec<Value> {
    vec![
        json!("A"),
        json!("B"),
        json!("manual"),
        json!(""),
        Value::Null,
        json!(0),
        json!(5),
        json!(1.5),
        json!(true),
        json!(false),
        json!([]),
        json!(["x", 1]),
        json!({}),
        json!({"k": "v"}),
    ]
}

#[test]
fn reconcile_inputs_of_every_json_type_give_the_same_commands_and_states_as_python() {
    // given
    let mut rng = Rng(0x5EED_0002);
    let actives = [json!(true), json!(false), json!("false"), json!(0), json!(1), Value::Null, json!([])];
    let ids = odd_ids();
    let cases: Vec<Value> = (0..400)
        .map(|_| {
            json!({
                "kind": "reconcile",
                "start": random_start(&mut rng),
                "active": rng.pick(&actives),
                "meeting_id": rng.pick(&ids),
                "session_alive": rng.chance(50),
            })
        })
        .collect();
    // when
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| {
            let mut m = lifecycle_of(&case["start"]);
            let meeting_id = Some(&case["meeting_id"]).filter(|v| py::truthy(v)).map(py::str_of);
            let commands =
                reconcile(&mut m, py::truthy(&case["active"]), meeting_id.as_deref(), case["session_alive"] == true);
            let mut out = snapshot(&m);
            out["commands"] = commands_json(&commands);
            out
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("reconcile", &cases, &rust, &python);
    }
}

struct ScriptedRunner {
    alive: std::cell::Cell<bool>,
    screen: RefCell<String>,
    keys: RefCell<Vec<Vec<String>>>,
}

impl Runner for ScriptedRunner {
    async fn spawn(&self, _: Option<&str>) -> Result<bool, Error> {
        Ok(true)
    }
    async fn kill(&self, _: Option<&str>) -> Result<(), Error> {
        Ok(())
    }
    async fn alive(&self, _: Option<&str>) -> Result<bool, Error> {
        Ok(self.alive.get())
    }
    async fn capture(&self) -> Result<String, Error> {
        Ok(self.screen.borrow().clone())
    }
    async fn send_keys(&self, keys: &[String]) -> Result<(), Error> {
        self.keys.borrow_mut().push(keys.to_vec());
        Ok(())
    }
    fn session_name(&self) -> String {
        "copilot".into()
    }
    fn set_session_name(&self, _: &str) {}
}

struct Host {
    lifecycle: RefCell<SessionLifecycle>,
    statuses: RefCell<Vec<AgentState>>,
    died: std::cell::Cell<usize>,
    in_flight: std::cell::Cell<bool>,
}

impl WatchdogHost for Host {
    fn lifecycle_state(&self) -> State {
        self.lifecycle.borrow().state
    }
    fn session_ready(&self) {
        self.lifecycle.borrow_mut().session_ready();
    }
    fn spawn_in_flight(&self) -> bool {
        self.in_flight.get()
    }
    async fn send_status(&self, status: AgentState) {
        self.statuses.borrow_mut().push(status);
    }
    async fn on_session_died(&self) {
        self.died.set(self.died.get() + 1);
    }
}

fn screens() -> Vec<String> {
    let mut all: Vec<String> = [
        DEV_CHANNELS_SCREEN,
        MODEL_DIALOG_SCREEN,
        REPL_PROMPT_SCREEN,
        SPINNER_SCREEN,
        STARTUP_SCREEN,
        TRUST_DIALOG_SCREEN,
        TRUST_DIALOG_NUMBERED_SCREEN,
        "",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    all.push(" Do you trust the files in this folder?\n ❯ 1. Proceed\n   2. Exit\n".into());
    all.push("   Yes, I trust this folder\n\n\n ❯ No, exit\n Enter to confirm\n".into());
    all.push(format!("{}\n{}", "x\n".repeat(20), "❯ prompt far above"));
    all.push(format!("❯ prompt\n{}", "line\n".repeat(16)));
    all.push("LOAD LOCAL DEVELOPMENT CHANNELS\r❯ 1. yes\rEsc to cancel".into());
    all
}

#[test]
fn watchdog_tick_scripts_give_the_same_statuses_keys_and_deaths_as_python() {
    // given: random panes, liveness, in-flight spawns and clock jumps
    let mut rng = Rng(0x5EED_0003);
    let screens = screens();
    let jumps = [0.0, 1.0, 2.0, 3.0, 5.0, 9.0, 10.0, 30.0, 60.0];
    let cases: Vec<Value> = (0..300)
        .map(|_| {
            let mut now = 0.0;
            let steps: Vec<Value> = (0..12)
                .map(|_| {
                    now += rng.pick(&jumps);
                    json!({
                        "alive": rng.chance(85),
                        "screen": rng.pick(&screens),
                        "in_flight": rng.chance(15),
                        "now": now,
                        "idle": rng.chance(4),
                    })
                })
                .collect();
            let mut start = random_start(&mut rng);
            if start["state"] == "idle" && rng.chance(70) {
                start = json!({"state": "spawning", "meeting_id": "A"});
            }
            json!({"kind": "watchdog", "start": start, "steps": steps})
        })
        .collect();
    // when
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| {
            let host = Host {
                lifecycle: RefCell::new(lifecycle_of(&case["start"])),
                statuses: RefCell::default(),
                died: Default::default(),
                in_flight: Default::default(),
            };
            let runner =
                ScriptedRunner { alive: Default::default(), screen: RefCell::default(), keys: RefCell::default() };
            let now = Rc::new(std::cell::Cell::new(0.0));
            let clock = now.clone();
            let mut watchdog = Watchdog::new(Box::new(move || clock.get()));
            let steps: Vec<Value> = case["steps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|step| {
                    runner.alive.set(step["alive"] == true);
                    *runner.screen.borrow_mut() = step["screen"].as_str().unwrap().to_owned();
                    host.in_flight.set(step["in_flight"] == true);
                    now.set(step["now"].as_f64().unwrap());
                    if step["idle"] == true {
                        *host.lifecycle.borrow_mut() = SessionLifecycle::new();
                    }
                    let (before_s, before_k) = (host.statuses.borrow().len(), runner.keys.borrow().len());
                    runtime.block_on(watchdog.tick(&host, &runner)).unwrap();
                    json!({
                        "statuses": host.statuses.borrow()[before_s..].iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                        "keys": runner.keys.borrow()[before_k..].to_vec(),
                        "died": host.died.get(),
                        "state": host.lifecycle.borrow().state.as_str(),
                    })
                })
                .collect();
            Value::Array(steps)
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("watchdog", &cases, &rust, &python);
    }
}

const REVISION: &str = "0b1e4c52-4f0a-4b7e-9d5e-3c2f6a7b8c9d";

/// Pieces of `ai-settings.json`, valid and not, combined at random.
fn settings_bodies(rng: &mut Rng, count: usize) -> Vec<Option<String>> {
    let revisions = [
        json!(REVISION),
        json!("defaults"),
        json!(REVISION.to_uppercase()),
        json!(1),
        json!(REVISION.replace('-', "")),
    ];
    let versions = [json!(1), json!(1.0), json!(2), json!(true), json!("1")];
    let models = [
        json!({}),
        json!({"claude": "global-model"}),
        json!({"codex": "gpt-x"}),
        json!({"claude": " "}),
        json!({"claude": "with space"}),
        json!({"claude": "-dash"}),
        json!({"claude": "zero\u{200b}width"}),
        json!({"claude": "x".repeat(161)}),
        json!({"claude": "модель"}),
        json!({"openrouter": "gone"}),
        json!({"other": "x"}),
        json!({"claude": 5}),
        json!([]),
    ];
    let copilot = [
        json!({}),
        json!({"enabled": false}),
        json!({"enabled": true}),
        json!({"enabled": "no"}),
        json!({"enabled": 0}),
        json!({"models": {"claude": "copilot-model"}}),
        json!({"models": {"claude": "a$b'c\"d{e}"}}),
        json!({"models": {"codex": "x"}}),
        json!({"models": {}}),
        json!({"models": {"claude": ""}}),
        json!({"models": {"openrouter": "y"}}),
        json!({"claude_effort": "max"}),
        json!({"claude_effort": "turbo"}),
        json!({"claude_effort": ""}),
        json!({"reasoning_effort": "low"}),
        json!({"prompt": "Ты помощник"}),
        json!({"prompt": "   "}),
        json!({"prompt": "with {unknown} placeholder"}),
        json!({"prompt": "nul\u{0}"}),
        json!({"providers": ["claude"]}),
        json!({"providers": ["codex"]}),
        json!({"providers": ["openrouter"]}),
        json!({"providers": ["claude", "openrouter"]}),
        json!({"unknown": 1}),
        json!("not an object"),
    ];
    let others = [
        ("title", json!({"prompt": "Title of {transcript}"})),
        ("title", json!({"prompt": "no placeholder"})),
        ("title", json!({"prompt": "{transcript} and {meeting_title}"})),
        ("title", json!({"prompt": "{transcript:>10}"})),
        ("title", json!({"prompt": "{{literal}} {transcript}"})),
        ("title", json!({"prompt": "{{transcript}}"})),
        ("title", json!({"prompt": "{transcript}}"})),
        ("title", json!({"prompt": "{ transcript} {transcript}"})),
        ("title", json!({"prompt": "{transcript.attr} x"})),
        ("title", json!({"prompt": "{user_name}{transcript}{participants}"})),
        ("title", json!({"prompt": "{transcript", })),
        ("title", json!({"enabled": true})),
        ("title", json!({"providers": ["claude", "codex"]})),
        ("analysis.increment", json!({"providers": ["claude", "codex"]})),
        ("analysis.increment", json!({"providers": ["claude", "claude"]})),
        ("analysis.increment", json!({"providers": []})),
        ("analysis.increment", json!({"reasoning_effort": "minimal"})),
        ("analysis.increment", json!({"reasoning_effort": "ultra"})),
        ("meeting_chat", json!({"models": {"codex": "m"}})),
        ("saved:my-prompt", json!({"prompt": "{transcript}"})),
        ("saved:тест.v1-2", json!({"claude_effort": "low"})),
        ("saved:", json!({})),
        ("saved:-bad", json!({})),
        ("saved:x²", json!({})),
        ("saved:é\u{301}", json!({})),
        ("saved:Ⅻ", json!({})),
        ("unknown-task", json!({})),
        ("anchor", json!({"whatever": 1})),
        ("analysis.summary", json!(5)),
    ];
    let mut bodies: Vec<Option<String>> = vec![None, Some(String::new()), Some("not json".into()), Some("[]".into())];
    bodies.push(Some(format!(
        "\u{feff}{{\"version\": 1, \"revision\": \"{REVISION}\", \"models\": {{}}, \"tasks\": {{}}}}"
    )));
    bodies.push(Some(format!(
        "{{\"version\": 1, \"revision\": \"{REVISION}\", \"models\": {{}}, \"tasks\": {{}}, \"x\": 1}}"
    )));
    bodies.push(Some(format!("{{\"version\": 1, \"revision\": \"{REVISION}\", \"models\": {{}}}}")));
    bodies.push(Some(format!(
        "{{\"version\": 1, \"revision\": \"{REVISION}\", \"models\": {{}}, \"tasks\": {{}}, {}}}",
        " ".repeat(262_144)
    )));
    // Indices of the pieces Python accepts (retired entries included: they are dropped).
    let valid_models = [0, 1, 2, 8, 9];
    let valid_copilot = [0, 1, 2, 5, 6, 10, 11, 13, 15, 19, 22];
    let valid_others = [0, 2, 4, 9, 13, 16, 19, 20, 27, 28];
    while bodies.len() < count {
        // Half the files only from accepted pieces, so the model and switch paths are
        // compared as often as the refusals.
        let valid = rng.chance(50);
        let mut tasks = serde_json::Map::new();
        if rng.chance(80) {
            let entry = if valid { &copilot[*rng.pick(&valid_copilot)] } else { rng.pick(&copilot) };
            tasks.insert("copilot".into(), entry.clone());
        }
        for _ in 0..rng.below(3) {
            let (id, values) = if valid { &others[*rng.pick(&valid_others)] } else { rng.pick(&others) };
            tasks.insert((*id).to_owned(), values.clone());
        }
        let valid_frame = valid || rng.chance(80);
        let models = if valid {
            models[*rng.pick(&valid_models)].clone()
        } else if rng.chance(70) {
            json!({})
        } else {
            rng.pick(&models).clone()
        };
        let body = json!({
            "version": if valid_frame { json!(1) } else { rng.pick(&versions).clone() },
            "revision": if valid_frame { json!(REVISION) } else { rng.pick(&revisions).clone() },
            "models": models,
            "tasks": Value::Object(tasks),
        });
        bodies.push(Some(body.to_string()));
    }
    bodies
}

fn legacy_values() -> Vec<Value> {
    vec![Value::Null, json!(""), json!("legacy-model"), json!(" padded "), json!("bad model"), json!("-dash")]
}

#[test]
fn ai_settings_files_resolve_to_the_same_copilot_task_or_refusal_as_python() {
    // given
    let mut rng = Rng(0x5EED_0004);
    let efforts = [Value::Null, json!(""), json!("low"), json!("xhigh"), json!("turbo"), json!(" low")];
    let legacy = legacy_values();
    let mut cases: Vec<Value> = settings_bodies(&mut rng, 600)
        .into_iter()
        .map(|body| {
            let plain = rng.chance(60);
            json!({
                "kind": "settings",
                "body": body,
                "claude_model": if plain { Value::Null } else { rng.pick(&legacy).clone() },
                "codex_model": if plain { Value::Null } else { rng.pick(&legacy).clone() },
                "codex_effort": if plain { Value::Null } else { rng.pick(&efforts).clone() },
            })
        })
        .collect();
    cases.push(json!({"kind": "settings", "body": null, "directory": true}));
    // when
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| {
            let dir = tempfile::tempdir().unwrap();
            if let Some(body) = case["body"].as_str() {
                std::fs::write(dir.path().join("ai-settings.json"), body).unwrap();
            }
            if case["directory"] == true {
                std::fs::create_dir(dir.path().join("ai-settings.json")).unwrap();
            }
            let text = |key: &str| case[key].as_str().unwrap_or("").to_owned();
            let legacy = LegacyEnv {
                claude_model: text("claude_model"),
                codex_model: text("codex_model"),
                codex_reasoning_effort: text("codex_effort"),
            };
            match AiSettingsFile::new(dir.path(), legacy).resolve() {
                Ok(task) => json!({"enabled": task.enabled, "model": task.model, "effort": task.claude_effort}),
                Err(_) => json!({"error": true}),
            }
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        let refused = rust.iter().filter(|r| r.get("error").is_some()).count();
        eprintln!("settings: {refused} of {} files refused", cases.len());
        assert!(refused * 3 < cases.len() * 2, "too few accepted files to compare the task paths");
        assert_same("settings", &cases, &rust, &python);
    }
}

#[test]
fn claude_command_lines_are_byte_identical_to_python() {
    // given: MCP URLs and models that need quoting and escaping
    let urls = ["http://127.0.0.1:8767/mcp", "http://x/mcp?a=1&b='2'", "http://хост/mcp", "http://a b/\"q\"\\", ""];
    let bodies = [
        None,
        Some(format!(r#"{{"version": 1, "revision": "{REVISION}", "models": {{"claude": "global"}}, "tasks": {{}}}}"#)),
        Some(format!(
            r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"copilot": {{"models": {{"claude": "custom$[literal]{{model}}"}}, "claude_effort": "max"}}}}}}"#
        )),
        Some(format!(
            r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"copilot": {{"models": {{"claude": "it's\"ok"}}}}}}}}"#
        )),
        Some(format!(
            r#"{{"version": 1, "revision": "{REVISION}", "models": {{}}, "tasks": {{"copilot": {{"enabled": false}}}}}}"#
        )),
        Some("broken".to_owned()),
    ];
    let mut cases = Vec::new();
    for url in urls {
        for body in &bodies {
            cases.push(json!({"kind": "claude_command", "mcp_url": url, "body": body}));
        }
    }
    // when
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| {
            let dir = tempfile::tempdir().unwrap();
            if let Some(body) = case["body"].as_str() {
                std::fs::write(dir.path().join("ai-settings.json"), body).unwrap();
            }
            let settings = Rc::new(AiSettingsFile::new(dir.path(), LegacyEnv::default()));
            let runner = TmuxRunner::new(FakeTmux::default(), settings, "/repo", case["mcp_url"].as_str().unwrap());
            match runner.build_claude_command() {
                Ok(command) => json!({"command": command}),
                Err(_) => json!({"error": true}),
            }
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("claude_command", &cases, &rust, &python);
    }
}

/// Records spawns and kills in one ordered list, as the oracle's `LogRunner`.
struct LogRunner {
    calls: RefCell<Vec<Value>>,
    alive: bool,
}

impl Runner for LogRunner {
    async fn spawn(&self, meeting_id: Option<&str>) -> Result<bool, Error> {
        self.calls.borrow_mut().push(json!(["spawn", meeting_id]));
        Ok(true)
    }
    async fn kill(&self, _: Option<&str>) -> Result<(), Error> {
        self.calls.borrow_mut().push(json!(["kill"]));
        Ok(())
    }
    async fn alive(&self, _: Option<&str>) -> Result<bool, Error> {
        Ok(self.alive)
    }
    async fn capture(&self) -> Result<String, Error> {
        Ok(String::new())
    }
    async fn send_keys(&self, _: &[String]) -> Result<(), Error> {
        Ok(())
    }
    fn session_name(&self) -> String {
        "copilot".into()
    }
    fn set_session_name(&self, _: &str) {}
    fn observable(&self) -> bool {
        false
    }
}

#[test]
fn frame_sequences_through_the_glue_give_the_same_runner_calls_and_state_as_python() {
    // given: lifecycle, manual and channel frames with odd ids, deaths and readiness
    let mut rng = Rng(0x5EED_0005);
    let ids = odd_ids();
    let actives = [json!(true), json!(false), json!("false"), json!(0), json!(1), Value::Null];
    let cases: Vec<Value> = (0..400)
        .map(|_| {
            let frames: Vec<Value> = (0..12)
                .map(|_| match rng.below(11) {
                    0 | 1 => json!({"type": "meeting_started", "meeting_id": rng.pick(&ids)}),
                    2 => json!({"type": "meeting_ended", "meeting_id": rng.pick(&ids)}),
                    3 => json!({"type": "meeting_state", "active": rng.pick(&actives), "meeting_id": rng.pick(&ids)}),
                    4 => json!({"type": "copilot_spawn_request", "request_id": "r"}),
                    5 => json!({"type": "copilot_stop_request", "request_id": rng.pick(&ids)}),
                    6 => json!({"type": "copilot_channel_unavailable", "meeting_id": rng.pick(&ids)}),
                    7 => json!("__died__"),
                    8 => json!("__ready__"),
                    9 => rng.pick(&[json!({"type": "unknown"}), json!({}), json!(["x"]), json!({"type": 5})]).clone(),
                    _ => json!({"type": "meeting_ended"}),
                })
                .collect();
            json!({"kind": "frames", "autospawn": rng.chance(75), "session_alive": rng.chance(30), "frames": frames})
        })
        .collect();
    // when
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
    let rust: Vec<Value> = cases
        .iter()
        .map(|case| {
            let local = tokio::task::LocalSet::new();
            local.block_on(&runtime, async {
                let runner = Rc::new(LogRunner { calls: RefCell::default(), alive: case["session_alive"] == true });
                let sup = Supervisor::new(
                    "ws://127.0.0.1:1",
                    runner.clone(),
                    TestSettings::enabled(),
                    case["autospawn"] == true,
                    Tunables::new(1000.0),
                );
                for item in case["frames"].as_array().unwrap() {
                    match item.as_str() {
                        Some("__died__") => sup.on_session_died(),
                        Some("__ready__") => {
                            sup.with_lifecycle(|l| l.session_ready());
                        }
                        _ => sup.handle_frame(item).await,
                    }
                }
                let out = json!({
                    "calls": runner.calls.borrow().clone(),
                    "state": sup.state().as_str(),
                    "meeting_id": sup.meeting_id(),
                    "grace_pending": sup.grace_pending(),
                    "respawn_attempts": sup.respawn_attempts(),
                    "respawn_pending": sup.respawn_pending(),
                    "hello": sup.live_meeting_id(),
                });
                sup.stop();
                out
            })
        })
        .collect();
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("frames", &cases, &rust, &python);
    }
}

#[tokio::test]
async fn hello_bytes_from_the_wire_rearm_the_real_python_bridge_and_match_json_dumps() {
    local(async {
        // given: a live manual session whose supervisor connects to a capturing bridge
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let captured = Rc::new(RefCell::new(None::<String>));
        let sink = captured.clone();
        tokio::task::spawn_local(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            if let Some(Ok(message)) = socket.next().await {
                *sink.borrow_mut() = Some(message.to_text().unwrap().to_owned());
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let runner = FakeRunner::new();
        let sup = Supervisor::new(&url, runner, TestSettings::enabled(), false, Tunables::new(90.0));
        sup.handle_frame(&frame(r#"{"type": "copilot_spawn_request", "request_id": "r1"}"#)).await;
        sup.with_lifecycle(|l| l.session_ready());
        // when
        sup.start();
        for _ in 0..100 {
            if captured.borrow().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        sup.stop();
        let raw = captured.borrow().clone().expect("a hello on the wire");
        // then: the real KTalkBridge re-arms its manual mirror and replies adoptably
        let cases = [json!({"kind": "bridge_hello", "raw": raw})];
        if let Some(python) = oracle(&cases) {
            assert_eq!(python[0]["manual_active"], true);
            let states = python[0]["meeting_states"].as_array().unwrap();
            assert_eq!(states.len(), 1);
            assert_eq!(states[0]["active"], true);
            assert_eq!(states[0]["meeting_id"], "manual");
            assert_eq!(python[0]["python_bytes"], json!(raw), "the hello bytes differ from json.dumps");
        }
    })
    .await;
}

#[test]
fn python_builtins_the_port_mirrors_agree_on_json_values() {
    // given
    let values = vec![
        Value::Null,
        json!(true),
        json!(false),
        json!(0),
        json!(-7),
        json!(9_007_199_254_740_993_i64),
        json!(0.0),
        json!(-0.0),
        json!(1.0),
        json!(1.5),
        json!(0.1),
        json!(1e16),
        json!(1.5e16),
        json!(1e15),
        json!(123456789.125),
        json!(1e-4),
        json!(1e-5),
        json!(2.5e-7),
        json!(1e300),
        json!(""),
        json!("A"),
        json!("it's"),
        json!("say \"hi\""),
        json!("both ' and \""),
        json!("tab\tnew\nline\rcr\\back"),
        json!("\u{0}\u{7}\u{1b}\u{7f}\u{85}\u{a0}\u{ad}"),
        json!("ünïcödé ❯ 😀 \u{2028} \u{e000} \u{fffe}"),
        json!([]),
        json!([1, "a", null, [true]]),
        json!({}),
        json!({"k": "v", "n": [1.5, {"x": null}]}),
    ];
    let cases = vec![json!({"kind": "values", "values": values})];
    // when
    let rust = vec![json!({
        "truthy": values.iter().map(py::truthy).collect::<Vec<_>>(),
        "str": values.iter().map(py::str_of).collect::<Vec<_>>(),
        "repr": values.iter().map(py::repr).collect::<Vec<_>>(),
        "json": values.iter().map(py::json_dumps).collect::<Vec<_>>(),
    })];
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("values", &cases, &rust, &python);
    }
}

#[test]
fn python_text_handling_the_port_mirrors_agrees_on_panes_and_arguments() {
    // given: line boundaries, trailing whitespace and case mappings Python treats specially
    let mut texts: Vec<String> = screens();
    let mut rng = Rng(0x5EED_0006);
    let pieces = [
        "❯",
        "❯ 1.",
        " Yes, I trust this folder",
        "No, exit",
        "esc to interrupt",
        "Enter to confirm",
        "local development",
        "trust this folder",
        "plain",
        "İSTANBUL",
        "ẞ",
        "ǅ",
        "\u{3000}",
        "\u{1f}",
        " ",
        "\t",
        "",
    ];
    let breaks = ["\n", "\r", "\r\n", "\u{b}", "\u{c}", "\u{1c}", "\u{1d}", "\u{1e}", "\u{85}", "\u{2028}", "\u{2029}"];
    for _ in 0..300 {
        let mut text = String::new();
        for _ in 0..rng.below(25) {
            text.push_str(rng.pick(&pieces));
            text.push_str(rng.pick(&breaks));
        }
        if rng.chance(30) {
            text.push_str(rng.pick(&["  ", "\u{3000}", "\u{1c}", "\u{a0}\n"]));
        }
        texts.push(text);
    }
    texts.extend(["", "a b", "it's", "x=1,2", "safe_@%+=:,./-", "$HOME", "ü"].map(str::to_owned));
    let cases = vec![json!({"kind": "text", "texts": texts})];
    // when
    let rust = vec![json!({
        "splitlines": texts.iter().map(|t| py::splitlines(t)).collect::<Vec<_>>(),
        "rstrip": texts.iter().map(|t| py::rstrip(t)).collect::<Vec<_>>(),
        "strip": texts.iter().map(|t| py::strip(t)).collect::<Vec<_>>(),
        "lower": texts.iter().map(|t| t.to_lowercase()).collect::<Vec<_>>(),
        "shlex": py::shlex_join(&texts),
        "classify": texts.iter().map(|t| classify_screen(Some(t)).as_str()).collect::<Vec<_>>(),
        "trust_keys": texts.iter().map(|t| trust_dialog_keys(t)).collect::<Vec<_>>(),
    })];
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("text", &cases, &rust, &python);
    }
}

fn ranges(predicate: impl Fn(char) -> bool) -> Value {
    let mut out = Vec::new();
    let mut start = None;
    for code in 0..0x11_0000u32 {
        let hit = char::from_u32(code).is_some_and(&predicate);
        match (hit, start) {
            (true, None) => start = Some(code),
            (false, Some(s)) => {
                out.push(json!([s, code - 1]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(json!([s, 0x10_FFFF]));
    }
    Value::Array(out)
}

#[test]
fn unicode_classes_match_python_on_every_code_point() {
    // given
    let cases = vec![json!({"kind": "unicode"})];
    // when
    let rust = vec![json!({
        "isspace": ranges(py::is_space),
        "other_category": ranges(py::is_other_category),
        "word": ranges(py::is_word),
        "printable": ranges(py::is_printable),
    })];
    // then
    if let Some(python) = oracle(&cases) {
        assert_same("unicode", &cases, &rust, &python);
    }
}

#[test]
fn constants_defaults_and_fixture_screens_match_the_python_module() {
    // given
    let cases = vec![json!({"kind": "constants"})];
    let tunables = Tunables::new(config::DEFAULT_GRACE_S);
    // when
    let rust = json!({
        "manual": recog_copilot::supervisor::MANUAL_MEETING_ID,
        "socket": tmux::TMUX_SOCKET,
        "session": tmux::TMUX_SESSION,
        "session_manual": tmux::TMUX_SESSION_MANUAL,
        "unit": tmux::SYSTEMD_SESSION_UNIT,
        "session_env_keys": config::SESSION_ENV_KEYS,
        "unit_stop_timeout_s": tmux::SYSTEMD_UNIT_STOP_TIMEOUT.as_secs_f64(),
        "unit_poll_s": tmux::SYSTEMD_UNIT_POLL.as_secs_f64(),
        "kill_retries": tmux::KILL_RETRIES,
        "kill_retry_delay_s": tmux::KILL_RETRY_DELAY.as_secs_f64(),
        "respawn_backoff_s": tunables.respawn_backoff_s,
        "initial_backoff_s": tunables.initial_backoff_s,
        "max_backoff_s": tunables.max_backoff_s,
        "log_throttle_s": tunables.log_throttle_s,
        "poll_s": watchdog::POLL_INTERVAL_S,
        "heartbeat_s": watchdog::HEARTBEAT_S,
        "spawn_deadline_s": watchdog::SPAWN_DEADLINE_S,
        "dismiss_retry_s": watchdog::DISMISS_RETRY_S,
        "blocked_markers": screen::BLOCKED_MARKERS,
        "auto_dismiss_markers": screen::AUTO_DISMISS_MARKERS,
        "bridge_url": config::DEFAULT_BRIDGE_URL,
        "log_prefix": recog_copilot::log::SUPERVISOR_PREFIX,
        "default_autospawn": true,
        "default_grace_s": config::DEFAULT_GRACE_S,
        "default_mcp_url": config::DEFAULT_MCP_URL,
        "default_claude_model": recog_copilot::ai_settings::DEFAULT_CLAUDE_MODEL,
        "default_codex_model": recog_copilot::ai_settings::DEFAULT_CODEX_MODEL,
        "screens": {
            "DEV_CHANNELS_SCREEN": DEV_CHANNELS_SCREEN,
            "MODEL_DIALOG_SCREEN": MODEL_DIALOG_SCREEN,
            "REPL_PROMPT_SCREEN": REPL_PROMPT_SCREEN,
            "SPINNER_SCREEN": SPINNER_SCREEN,
            "STARTUP_SCREEN": STARTUP_SCREEN,
            "TRUST_DIALOG_SCREEN": TRUST_DIALOG_SCREEN,
            "TRUST_DIALOG_NUMBERED_SCREEN": TRUST_DIALOG_NUMBERED_SCREEN,
        },
    });
    // then
    if let Some(python) = oracle(&cases) {
        let mut python = python[0].clone();
        assert_eq!(python["bridge_manual"], json!(recog_copilot::supervisor::MANUAL_MEETING_ID));
        python.as_object_mut().unwrap().remove("bridge_manual");
        assert_eq!(rust, python);
    }
}

#[test]
fn settings_on_the_disk_and_the_test_switch_agree_on_a_missing_file() {
    // given: the fake used by the glue tests behaves like the real default
    let dir = tempfile::tempdir().unwrap();
    // when / then
    assert_eq!(AiSettingsFile::new(dir.path(), LegacyEnv::default()).resolve(), TestSettings::enabled().resolve(),);
}
