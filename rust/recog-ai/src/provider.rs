//! Claude and Codex through their CLIs (`recog/ai_provider.py`, `recog/node_env.py`).
//!
//! Both keep the user's subscription login (no API key, the CLI's own auth) and isolate
//! an automation call from the user's personal instructions, skills and plugins:
//! Claude runs with `--safe-mode --disable-slash-commands`, Codex with
//! `--ignore-user-config` and the customization switched off by `-c` overrides. The
//! prompt goes to stdin (Linux limits one argv string to 128 KiB), marked so that
//! session search can tell automation from the user's own sessions. The child inherits
//! the working directory and gets the environment of the process, with the newest nvm
//! node prepended to `PATH` when `node` is not on it (a service manager's `PATH` is
//! minimal).

use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::defaults::{DEFAULT_CLAUDE_MODEL, DEFAULT_CODEX_MODEL};
use crate::error::{AiError, Result};
use crate::json::Json;
use crate::pytext;
use crate::settings::ResolvedTask;

pub const CCS_AUTOMATION_MARKER_PREFIX: &str = "<!-- ccs-automation:recog";

/// A boxed future, so that providers can be trait objects.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Receives `(text, is_done)` chunks of a streamed answer.
pub type ChunkSink<'a> = dyn FnMut(String, bool) + Send + 'a;

/// The environment variables a CLI call sees, in the order of the process environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliEnv {
    vars: Vec<(OsString, OsString)>,
}

impl CliEnv {
    /// The environment of this process now.
    pub fn from_process() -> CliEnv {
        CliEnv { vars: std::env::vars_os().collect() }
    }

    pub fn from_pairs<K: Into<OsString>, V: Into<OsString>>(pairs: impl IntoIterator<Item = (K, V)>) -> CliEnv {
        let mut env = CliEnv::default();
        for (key, value) in pairs {
            env.set(key, value);
        }
        env
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_os_str())
    }

    fn get_str(&self, key: &str) -> Option<String> {
        self.get(key).map(|value| value.to_string_lossy().into_owned())
    }

    /// Replaces a variable in place, or appends it (a dict assignment).
    pub fn set(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        let (key, value) = (key.into(), value.into());
        match self.vars.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => self.vars.push((key, value)),
        }
    }

    pub fn vars(&self) -> &[(OsString, OsString)] {
        &self.vars
    }

    /// `os.path.expanduser("~" + rest)`: `$HOME` without its trailing slashes, the
    /// password database when `HOME` is unset.
    fn expand_home(&self, rest: &str) -> PathBuf {
        let home: OsString = match self.get("HOME") {
            Some(home) => home.to_owned(),
            None => std::env::home_dir().map(PathBuf::into_os_string).unwrap_or_default(),
        };
        let mut bytes = home.into_vec();
        while bytes.last() == Some(&b'/') {
            bytes.pop();
        }
        bytes.extend_from_slice(rest.as_bytes());
        if bytes.is_empty() {
            bytes.push(b'/');
        }
        PathBuf::from(OsString::from_vec(bytes))
    }
}

fn is_executable(path: &Path) -> bool {
    rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

/// `shutil.which(name)`: the first `PATH` entry (an empty one is the working directory)
/// holding an executable that is not a directory.
pub fn which(name: &str, env: &CliEnv) -> Option<PathBuf> {
    // `os.confstr("CS_PATH")` when PATH is unset.
    let default_path: &[u8] =
        if cfg!(target_os = "macos") { b"/usr/bin:/bin:/usr/sbin:/sbin" } else { b"/bin:/usr/bin" };
    let path = env.get("PATH").map_or(default_path, OsStr::as_bytes);
    if path.is_empty() {
        return None;
    }
    let mut seen: Vec<&[u8]> = Vec::new();
    for dir in path.split(|byte| *byte == b':') {
        if seen.contains(&dir) {
            continue;
        }
        seen.push(dir);
        let candidate = Path::new(OsStr::from_bytes(dir)).join(name);
        if candidate.exists() && is_executable(&candidate) && !candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// `version_key`: `(major, minor, patch)` of an nvm directory name, `(0, 0, 0)` for
/// another name.
pub fn version_key(name: &str) -> (u128, u128, u128) {
    let rest = name.strip_prefix('v').unwrap_or(name);
    let mut numbers = [0u128; 3];
    let mut rest = rest;
    for (index, slot) in numbers.iter_mut().enumerate() {
        let digits = rest.len() - rest.trim_start_matches(|ch: char| ch.is_ascii_digit()).len();
        if digits == 0 {
            return (0, 0, 0);
        }
        *slot = rest[..digits].parse().unwrap_or(u128::MAX);
        rest = &rest[digits..];
        if index < 2 {
            match rest.strip_prefix('.') {
                Some(after) => rest = after,
                None => return (0, 0, 0),
            }
        }
    }
    (numbers[0], numbers[1], numbers[2])
}

fn name_key(path: &Path) -> (u128, u128, u128) {
    version_key(&path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default())
}

/// `glob.glob(f"{base}/*/bin/{name}")`, newest version first: non-hidden directories.
fn nvm_candidates(base: &Path, name: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| !entry.file_name().as_bytes().starts_with(b"."))
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| path.join("bin").join(name))
        .filter(|candidate| candidate.symlink_metadata().is_ok())
        .collect();
    // Stable and descending, like `sorted(..., reverse=True)`.
    found.sort_by(|a, b| {
        let key = |path: &PathBuf| name_key(path.parent().and_then(Path::parent).unwrap_or(Path::new("")));
        key(b).cmp(&key(a))
    });
    found
}

/// `resolve_cli`: the full path of a CLI, also under a service manager's short `PATH`.
pub fn resolve_cli(name: &str, env: &CliEnv) -> Result<PathBuf> {
    if let Some(path) = which(name, env) {
        return Ok(path);
    }
    let mut candidates = vec![
        env.expand_home(&format!("/.local/bin/{name}")),
        PathBuf::from(format!("/opt/homebrew/bin/{name}")),
        PathBuf::from(format!("/usr/local/bin/{name}")),
    ];
    for base in ["/.local/share/nvm", "/.nvm/versions/node"] {
        candidates.extend(nvm_candidates(&env.expand_home(base), name));
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file() && is_executable(candidate))
        .ok_or_else(|| AiError::NotFound(format!("{name} not found in PATH, ~/.local/bin, or nvm")))
}

/// `get_node_env`: the environment, with the newest nvm node's `bin` first in `PATH`
/// when `node` is not on it.
pub fn node_env(env: &CliEnv) -> io::Result<CliEnv> {
    let mut out = env.clone();
    if which("node", env).is_some() {
        return Ok(out);
    }
    let home = env.expand_home("");
    for base in [home.join(".local/share/nvm"), home.join(".nvm/versions/node")] {
        if !base.exists() {
            continue;
        }
        let mut versions: Vec<PathBuf> =
            std::fs::read_dir(&base)?.map(|entry| entry.map(|entry| entry.path())).collect::<io::Result<_>>()?;
        versions.sort_by_key(|path| std::cmp::Reverse(name_key(path)));
        for version in versions {
            let node_bin = version.join("bin");
            if node_bin.join("node").exists() {
                let mut path = node_bin.into_os_string();
                path.push(":");
                path.push(env.get("PATH").unwrap_or_default());
                out.set("PATH", path);
                return Ok(out);
            }
        }
    }
    Ok(out)
}

fn sanitize_ccs_task(task: &str) -> String {
    let normalized = pytext::strip(if task.is_empty() { "analysis" } else { task }).to_lowercase();
    let safe: String =
        normalized.chars().map(|ch| if pytext::is_alnum(ch) || "._:-".contains(ch) { ch } else { '-' }).collect();
    let safe = safe.trim_matches('-');
    if safe.is_empty() { "analysis".to_owned() } else { safe.to_owned() }
}

/// Marks a recog-generated prompt so that session search classifies it as automation.
pub fn mark_ccs_automation(prompt: &str, task: &str) -> String {
    if pytext::lstrip(prompt).starts_with(CCS_AUTOMATION_MARKER_PREFIX) {
        return prompt.to_owned();
    }
    format!("{CCS_AUTOMATION_MARKER_PREFIX} task={} -->\n{prompt}", sanitize_ccs_task(task))
}

/// `build_codex_exec_command`.
pub fn build_codex_exec_command(
    cli: &OsStr,
    model: &str,
    reasoning_effort: Option<&str>,
    ephemeral: bool,
    isolated: bool,
) -> Vec<OsString> {
    let mut cmd: Vec<OsString> = vec![cli.to_owned(), "exec".into(), "-m".into(), model.into()];
    if isolated {
        // Keep CODEX_HOME and its login while excluding customization from analysis.
        cmd.push("--ignore-user-config".into());
        for setting in [
            "skills.include_instructions=false",
            "skills.bundled.enabled=false",
            "features.plugins=false",
            "features.hooks=false",
            "features.memories=false",
            "features.skill_search=false",
            "project_doc_max_bytes=0",
        ] {
            cmd.push("-c".into());
            cmd.push(setting.into());
        }
    }
    if let Some(effort) = reasoning_effort.filter(|effort| !effort.is_empty() && !model.contains("spark")) {
        cmd.push("-c".into());
        cmd.push(format!("model_reasoning_effort=\"{effort}\"").into());
    }
    if ephemeral {
        cmd.push("--ephemeral".into());
    }
    cmd.push("--json".into());
    cmd.push("-".into());
    cmd
}

/// The Claude CLI call: `--output-format text`, or `stream-json --verbose` to stream.
pub fn build_claude_command(cli: &OsStr, model: &str, effort: Option<&str>, stream: bool) -> Vec<OsString> {
    let mut cmd: Vec<OsString> = vec![
        cli.to_owned(),
        "-p".into(),
        "--safe-mode".into(),
        "--disable-slash-commands".into(),
        "--model".into(),
        model.into(),
    ];
    if let Some(effort) = effort.filter(|effort| !effort.is_empty()) {
        cmd.push("--effort".into());
        cmd.push(effort.into());
    }
    cmd.push("--output-format".into());
    if stream {
        cmd.push("stream-json".into());
        cmd.push("--verbose".into());
    } else {
        cmd.push("text".into());
    }
    cmd
}

/// An AI backend (`AIProvider`).
pub trait AiProvider: Send + Sync {
    /// `claude` or `codex`.
    fn key(&self) -> &str;
    /// For display, e.g. `Claude (claude-opus-5-5)`.
    fn name(&self) -> String;
    fn model(&self) -> &str;
    fn reasoning_effort(&self) -> Option<&str>;
    fn complete<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, Result<String>>;

    /// Streams `(text, is_done)`; by default the whole answer as one final chunk.
    fn stream<'a>(&'a self, prompt: &'a str, sink: &'a mut ChunkSink<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let answer = self.complete(prompt).await?;
            sink(answer, true);
            Ok(())
        })
    }
}

fn decode(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|error| AiError::Type(format!("'utf-8' codec can't decode: {error}")))
}

struct Finished {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn spawn(argv: &[OsString], env: &CliEnv) -> Result<tokio::process::Child> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(env.vars().iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command.spawn()?)
}

/// `proc.communicate(input)`: stdin written while stdout and stderr are read; a child
/// that closes stdin early is not an error.
async fn communicate(argv: &[OsString], env: &CliEnv, input: &[u8]) -> Result<Finished> {
    let mut child = spawn(argv, env)?;
    let stdin = child.stdin.take();
    let feed = async move {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(input).await;
            let _ = stdin.shutdown().await;
        }
    };
    let ((), output) = tokio::join!(feed, child.wait_with_output());
    let output = output?;
    Ok(Finished { status: output.status, stdout: output.stdout, stderr: output.stderr })
}

/// `stderr.decode().strip() if stderr else "Unknown error"`.
fn failure_text(stderr: &[u8]) -> Result<String> {
    if stderr.is_empty() { Ok("Unknown error".to_owned()) } else { Ok(pytext::strip(&decode(stderr)?).to_owned()) }
}

/// A streamed call: stdin fed and stderr collected in the background, stdout by line.
struct Streaming {
    child: tokio::process::Child,
    stdout: BufReader<tokio::process::ChildStdout>,
    stderr: tokio::task::JoinHandle<Vec<u8>>,
}

impl Streaming {
    fn start(argv: &[OsString], env: &CliEnv, input: Vec<u8>) -> Result<Streaming> {
        let mut child = spawn(argv, env)?;
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = stdin.write_all(&input).await;
                let _ = stdin.shutdown().await;
            });
        }
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");
        let stderr = tokio::spawn(async move {
            let mut buffer = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buffer).await;
            buffer
        });
        Ok(Streaming { child, stdout, stderr })
    }

    /// The next line with its newline, `None` at the end of stdout.
    async fn next_line(&mut self) -> Result<Option<String>> {
        let mut buffer = Vec::new();
        if self.stdout.read_until(b'\n', &mut buffer).await? == 0 {
            return Ok(None);
        }
        decode(&buffer).map(Some)
    }

    /// Waits for the exit; a failure carries `stderr.decode().strip()`.
    async fn finish(self, label: &str) -> Result<()> {
        let Streaming { mut child, stderr, .. } = self;
        let status = child.wait().await?;
        if status.success() {
            return Ok(());
        }
        let stderr = stderr.await.unwrap_or_default();
        Err(AiError::Provider(format!("{label} CLI error: {}", pytext::strip(&decode(&stderr)?))))
    }
}

fn call_env(env: &Option<CliEnv>) -> Result<CliEnv> {
    let base = env.clone().unwrap_or_else(CliEnv::from_process);
    Ok(node_env(&base)?)
}

/// Claude through `claude -p`.
#[derive(Clone, Debug)]
pub struct ClaudeCliProvider {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub task: String,
    pub cli: PathBuf,
    env: Option<CliEnv>,
}

impl ClaudeCliProvider {
    /// `env`: `None` is the process environment at each call.
    pub fn new(task: &str, model: Option<&str>, reasoning_effort: Option<&str>, env: Option<CliEnv>) -> Result<Self> {
        let base = env.clone().unwrap_or_else(CliEnv::from_process);
        let model = match model {
            Some(model) => model.to_owned(),
            None => base.get_str("CLAUDE_MODEL").unwrap_or_else(|| DEFAULT_CLAUDE_MODEL.to_owned()),
        };
        let cli = resolve_cli("claude", &base)?;
        Ok(ClaudeCliProvider {
            model,
            reasoning_effort: reasoning_effort.map(str::to_owned),
            task: task.to_owned(),
            cli,
            env,
        })
    }

    pub fn command(&self, stream: bool) -> Vec<OsString> {
        build_claude_command(self.cli.as_os_str(), &self.model, self.reasoning_effort.as_deref(), stream)
    }

    async fn run_stream(&self, prompt: &str, sink: &mut ChunkSink<'_>) -> Result<()> {
        let marked = mark_ccs_automation(prompt, &self.task);
        let mut call = Streaming::start(&self.command(true), &call_env(&self.env)?, marked.into_bytes())?;
        let mut full_text = String::new();
        while let Some(line) = call.next_line().await? {
            let line = pytext::strip(&line);
            if line.is_empty() {
                continue;
            }
            let Ok(event) = Json::parse(line) else {
                continue;
            };
            let Json::Object(event) = &event else {
                return Err(AiError::no_get(event.type_name()));
            };
            match event.get("type").and_then(Json::as_str) {
                Some("assistant") => {
                    let empty = Json::Object(Default::default());
                    let message = event.get("message").unwrap_or(&empty);
                    let Json::Object(message) = message else {
                        return Err(AiError::no_get(message.type_name()));
                    };
                    let no_content = Json::Array(Vec::new());
                    for block in iterate(message.get("content").unwrap_or(&no_content))? {
                        let Json::Object(block) = &block else {
                            return Err(AiError::no_get(block.type_name()));
                        };
                        if block.get("type").and_then(Json::as_str) != Some("text") {
                            continue;
                        }
                        let text = block.get("text").cloned().unwrap_or(Json::from(""));
                        if text.truthy() {
                            // Claude sends the full text each time.
                            full_text = chunk_text(&text);
                            sink(full_text.clone(), false);
                        }
                    }
                }
                Some("result") => {
                    let result = event.get("result").map_or_else(|| full_text.clone(), chunk_text);
                    sink(result, true);
                    return Ok(());
                }
                _ => {}
            }
        }
        call.finish("Claude").await?;
        if !full_text.is_empty() {
            sink(full_text, true);
        }
        Ok(())
    }
}

/// `for block in value`: a list's items, a string's characters, a dict's keys.
fn iterate(value: &Json) -> Result<Vec<Json>> {
    match value {
        Json::Array(items) => Ok(items.clone()),
        Json::Str(text) => Ok(text.chars().map(|ch| Json::Str(ch.to_string())).collect()),
        Json::Object(map) => Ok(map.keys().map(|key| Json::Str(key.clone())).collect()),
        other => Err(AiError::Type(format!("'{}' object is not iterable", other.type_name()))),
    }
}

/// A streamed chunk as text: Python passes a non-string value through as it is.
fn chunk_text(value: &Json) -> String {
    match value {
        Json::Str(text) => text.clone(),
        Json::Null => String::new(),
        other => other.py_str(),
    }
}

impl AiProvider for ClaudeCliProvider {
    fn key(&self) -> &str {
        "claude"
    }

    fn name(&self) -> String {
        match self.reasoning_effort.as_deref().filter(|effort| !effort.is_empty()) {
            Some(effort) => format!("Claude ({} {effort})", self.model),
            None => format!("Claude ({})", self.model),
        }
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    fn complete<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let marked = mark_ccs_automation(prompt, &self.task);
            let finished = communicate(&self.command(false), &call_env(&self.env)?, marked.as_bytes()).await?;
            if !finished.status.success() {
                return Err(AiError::Provider(format!("Claude CLI error: {}", failure_text(&finished.stderr)?)));
            }
            Ok(pytext::strip(&decode(&finished.stdout)?).to_owned())
        })
    }

    fn stream<'a>(&'a self, prompt: &'a str, sink: &'a mut ChunkSink<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.run_stream(prompt, sink)
                .await
                .map_err(|error| AiError::Provider(format!("Claude streaming error: {error}")))
        })
    }
}

/// One line of `codex exec --json`.
#[derive(Debug, PartialEq)]
enum CodexEvent {
    Message(Json),
    Done,
}

/// `_codex_event`: an agent message, `turn.completed`, or nothing; `turn.failed` is an
/// error. `error` events are not fatal: codex also emits its retry notices
/// ("Reconnecting... 2/5") that way, and a failure that ends the turn is `turn.failed`.
fn codex_event(line: &str) -> Result<Option<CodexEvent>> {
    let Ok(event) = Json::parse(line) else {
        return Ok(None);
    };
    let Json::Object(event) = &event else {
        return Err(AiError::no_get(event.type_name()));
    };
    let kind = event.get("type").and_then(Json::as_str);
    if kind == Some("turn.failed") {
        let empty = Json::Object(Default::default());
        let error = event.get("error").filter(|error| error.truthy()).unwrap_or(&empty);
        let Json::Object(error) = error else {
            return Err(AiError::no_get(error.type_name()));
        };
        let message = error.get("message").filter(|message| message.truthy());
        return Err(AiError::Provider(message.map_or_else(|| "turn failed".to_owned(), Json::py_str)));
    }
    if kind == Some("turn.completed") {
        return Ok(Some(CodexEvent::Done));
    }
    let empty = Json::Object(Default::default());
    let item = if kind == Some("item.completed") { event.get("item").unwrap_or(&empty) } else { &empty };
    let Json::Object(item) = item else {
        return Err(AiError::no_get(item.type_name()));
    };
    if item.get("type").and_then(Json::as_str) == Some("agent_message") {
        return Ok(Some(CodexEvent::Message(item.get("text").cloned().unwrap_or(Json::from("")))));
    }
    Ok(None)
}

/// Codex (ChatGPT subscription) through `codex exec`.
#[derive(Clone, Debug)]
pub struct CodexCliProvider {
    pub model: String,
    pub reasoning_effort: String,
    pub task: String,
    pub cli: PathBuf,
    env: Option<CliEnv>,
}

impl CodexCliProvider {
    /// `env`: `None` is the process environment at each call.
    pub fn new(task: &str, model: Option<&str>, reasoning_effort: Option<&str>, env: Option<CliEnv>) -> Result<Self> {
        let base = env.clone().unwrap_or_else(CliEnv::from_process);
        let model = match model {
            Some(model) => model.to_owned(),
            None => base.get_str("CODEX_MODEL").unwrap_or_else(|| DEFAULT_CODEX_MODEL.to_owned()),
        };
        let reasoning_effort = match reasoning_effort {
            Some(effort) => effort.to_owned(),
            None => base.get_str("CODEX_REASONING_EFFORT").unwrap_or_else(|| "xhigh".to_owned()),
        };
        let cli = resolve_cli("codex", &base)?;
        Ok(CodexCliProvider { model, reasoning_effort, task: task.to_owned(), cli, env })
    }

    pub fn command(&self) -> Vec<OsString> {
        build_codex_exec_command(self.cli.as_os_str(), &self.model, Some(&self.reasoning_effort), true, true)
    }

    async fn run_stream(&self, prompt: &str, sink: &mut ChunkSink<'_>) -> Result<()> {
        let marked = mark_ccs_automation(prompt, &self.task);
        let mut call = Streaming::start(&self.command(), &call_env(&self.env)?, marked.into_bytes())?;
        let mut full_text = String::new();
        while let Some(line) = call.next_line().await? {
            match codex_event(&line)? {
                None => {}
                Some(CodexEvent::Done) => {
                    sink(full_text, true);
                    return Ok(());
                }
                Some(CodexEvent::Message(text)) if text.truthy() => {
                    let Json::Str(text) = text else {
                        return Err(AiError::Type(format!("agent message text is a {}", text.type_name())));
                    };
                    full_text = text.clone();
                    sink(text, false);
                }
                Some(CodexEvent::Message(_)) => {}
            }
        }
        call.finish("Codex").await?;
        if !full_text.is_empty() {
            sink(full_text, true);
        }
        Ok(())
    }
}

impl AiProvider for CodexCliProvider {
    fn key(&self) -> &str {
        "codex"
    }

    fn name(&self) -> String {
        if self.model.contains("spark") {
            format!("Codex ({})", self.model)
        } else {
            format!("Codex ({} {})", self.model, self.reasoning_effort)
        }
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn reasoning_effort(&self) -> Option<&str> {
        Some(&self.reasoning_effort)
    }

    fn complete<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let marked = mark_ccs_automation(prompt, &self.task);
            let finished = communicate(&self.command(), &call_env(&self.env)?, marked.as_bytes()).await?;
            if !finished.status.success() {
                return Err(AiError::Provider(format!("Codex CLI error: {}", failure_text(&finished.stderr)?)));
            }
            let stdout = decode(&finished.stdout)?;
            let mut parts = Vec::new();
            for line in pytext::splitlines(&stdout) {
                if let Some(CodexEvent::Message(text)) = codex_event(line)? {
                    parts.push(text);
                }
            }
            if parts.is_empty() {
                return Err(AiError::Provider("No response from Codex".to_owned()));
            }
            let mut texts = Vec::with_capacity(parts.len());
            for (index, part) in parts.into_iter().enumerate() {
                match part {
                    Json::Str(text) => texts.push(text),
                    other => {
                        return Err(AiError::Type(format!(
                            "sequence item {index}: expected str instance, {} found",
                            other.type_name()
                        )));
                    }
                }
            }
            Ok(texts.join("\n"))
        })
    }

    fn stream<'a>(&'a self, prompt: &'a str, sink: &'a mut ChunkSink<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.run_stream(prompt, sink)
                .await
                .map_err(|error| AiError::Provider(format!("Codex streaming error: {error}")))
        })
    }
}

/// `create_provider`: the explicitly selected backend, its login unchanged.
pub fn create_provider(
    provider: &str,
    task: &str,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    env: Option<CliEnv>,
) -> Result<Box<dyn AiProvider>> {
    match provider {
        "claude" => Ok(Box::new(ClaudeCliProvider::new(task, model, reasoning_effort, env)?)),
        "codex" => Ok(Box::new(CodexCliProvider::new(task, model, reasoning_effort, env)?)),
        other => Err(AiError::Value(format!("Unknown AI provider: {other}"))),
    }
}

/// `provider_for_settings`: the scenario's selected provider (or `provider`) with the
/// scenario's model and that provider's own effort.
pub fn provider_for_settings(
    settings: &ResolvedTask,
    task: &str,
    provider: Option<&str>,
    env: Option<CliEnv>,
) -> Result<Box<dyn AiProvider>> {
    let key = provider.unwrap_or(settings.provider());
    let model = settings.models.get(key).ok_or_else(|| AiError::Key(key.to_owned()))?;
    let effort = if key == "claude" { &settings.claude_effort } else { &settings.reasoning_effort };
    create_provider(key, task, Some(model), Some(effort), env)
}

/// Creates the provider of a scenario; the CLI factory in production, a fake in tests.
pub trait ProviderFactory: Send + Sync {
    fn create(&self, settings: &ResolvedTask, task: &str, provider: Option<&str>) -> Result<Box<dyn AiProvider>>;
}

/// [`provider_for_settings`] with the given environment (`None`: the process's).
#[derive(Clone, Debug, Default)]
pub struct CliProviderFactory {
    pub env: Option<CliEnv>,
}

impl ProviderFactory for CliProviderFactory {
    fn create(&self, settings: &ResolvedTask, task: &str, provider: Option<&str>) -> Result<Box<dyn AiProvider>> {
        provider_for_settings(settings, task, provider, self.env.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvm_names_given_version_key_when_parsed_then_semver_or_zero() {
        assert_eq!(version_key("v20.10.3"), (20, 10, 3));
        assert_eq!(version_key("18.0.0-extra"), (18, 0, 0));
        assert_eq!(version_key("system"), (0, 0, 0));
        assert_eq!(version_key("v20.1"), (0, 0, 0));
    }

    #[test]
    fn codex_lines_given_event_parser_when_read_then_python_outcomes() {
        assert_eq!(codex_event("not json").unwrap(), None);
        assert_eq!(codex_event(r#"{"type":"turn.completed"}"#).unwrap(), Some(CodexEvent::Done));
        assert_eq!(
            codex_event(r#"{"type":"item.completed","item":{"type":"agent_message","text":"hi"}}"#).unwrap(),
            Some(CodexEvent::Message(Json::from("hi")))
        );
        assert_eq!(codex_event(r#"{"type":"error","message":"Reconnecting... 2/5"}"#).unwrap(), None);
        assert_eq!(codex_event(r#"{"type":"turn.failed","error":{"message":"x"}}"#).unwrap_err().to_string(), "x");
        assert_eq!(codex_event(r#"{"type":"turn.failed"}"#).unwrap_err().to_string(), "turn failed");
        assert_eq!(codex_event("null").unwrap_err().to_string(), "'NoneType' object has no attribute 'get'");
    }

    #[test]
    fn task_names_given_sanitizer_when_marked_then_safe_lowercase_slug() {
        assert_eq!(sanitize_ccs_task("Increment Prompt"), "increment-prompt");
        assert_eq!(sanitize_ccs_task(""), "analysis");
        assert_eq!(sanitize_ccs_task(" -/- "), "analysis");
        assert_eq!(sanitize_ccs_task("Saved:Мой.v2"), "saved:мой.v2");
    }
}
