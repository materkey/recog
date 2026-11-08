//! The Copilot session in a detached tmux on its own socket (`TmuxRunner`).
//!
//! `tmux -L recog-copilot` keeps it apart from the user's tmux. The session's name is
//! its kind: `copilot-manual` for a webapp spawn, `copilot` for every meeting-driven
//! one, so a fresh supervisor reads the kind back instead of guessing. On Linux the
//! `tmux new-session` runs in a transient `systemd --user` unit, outside the
//! supervisor's cgroup, so `systemctl --user restart` of the supervisor does not take
//! the live Copilot down; the restarted supervisor re-adopts it through `alive()`.

use std::cell::RefCell;
use std::fmt;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::rc::Rc;
use std::time::Duration;

use serde_json::json;

use super::lifecycle::MANUAL_MEETING_ID;
use crate::ai_settings::CopilotSettings;
use crate::{log, py};

pub const TMUX_SOCKET: &str = "recog-copilot";
pub const TMUX_SESSION: &str = "copilot";
/// The manual (webapp) session's name. A session left by a supervisor that predates
/// the naming is `copilot` and reads as a conference session, the safe default.
pub const TMUX_SESSION_MANUAL: &str = "copilot-manual";
/// One transient unit per machine, like the tmux socket.
pub const SYSTEMD_SESSION_UNIT: &str = "recog-copilot-session";
pub const SYSTEMD_UNIT_STOP_TIMEOUT: Duration = Duration::from_secs(5);
pub const SYSTEMD_UNIT_POLL: Duration = Duration::from_millis(100);
pub const KILL_RETRIES: u32 = 3;
pub const KILL_RETRY_DELAY: Duration = Duration::from_millis(500);

/// A failed tmux, systemd or settings step, worded like the Python RuntimeError.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Runs one program: `(returncode, stdout and stderr combined)`. Injectable for tests,
/// like the Python `run` callable.
#[allow(async_fn_in_trait)]
pub trait CommandRunner {
    async fn run(&self, args: &[String]) -> Result<(i32, String), Error>;
}

/// Real processes, with the variables `.env` added to the environment (Python put them
/// into `os.environ`, which every child inherited).
#[derive(Clone, Debug, Default)]
pub struct ProcessRunner {
    pub extra_env: Vec<(String, String)>,
}

impl CommandRunner for ProcessRunner {
    async fn run(&self, args: &[String]) -> Result<(i32, String), Error> {
        let args = args.to_vec();
        let extra_env = self.extra_env.clone();
        // A blocking wait on the pool keeps the merged pipe simple; tmux calls are short.
        tokio::task::spawn_blocking(move || run_blocking(&args, &extra_env))
            .await
            .map_err(|e| Error(format!("subprocess task failed: {e}")))?
    }
}

fn run_blocking(args: &[String], extra_env: &[(String, String)]) -> Result<(i32, String), Error> {
    let (program, rest) = args.split_first().ok_or_else(|| Error("empty command".to_owned()))?;
    let (mut reader, writer) = std::io::pipe().map_err(|e| Error(e.to_string()))?;
    let writer_err = writer.try_clone().map_err(|e| Error(e.to_string()))?;
    let mut child = {
        // The command holds the pipe's write ends until it is dropped, and the read
        // below ends only when every write end is closed.
        let mut command = std::process::Command::new(program);
        command.args(rest).envs(extra_env.iter().cloned()).stdin(Stdio::null()).stdout(writer).stderr(writer_err);
        command.spawn().map_err(|e| Error(format!("[Errno {}] {e}: '{program}'", e.raw_os_error().unwrap_or(0))))?
    };
    let mut output = Vec::new();
    let read = reader.read_to_end(&mut output);
    let status = child.wait().map_err(|e| Error(e.to_string()))?;
    read.map_err(|e| Error(e.to_string()))?;
    let code = status.code().or_else(|| status.signal().map(|s| -s)).unwrap_or(-1);
    Ok((code, String::from_utf8_lossy(&output).into_owned()))
}

/// What the supervisor glue and the watchdog need from the session runner.
#[allow(async_fn_in_trait)]
pub trait Runner {
    /// Start the session for `meeting_id`; `Ok(false)` when a live one was adopted.
    async fn spawn(&self, meeting_id: Option<&str>) -> Result<bool, Error>;
    /// Kill the current session, or `session_name`.
    async fn kill(&self, session_name: Option<&str>) -> Result<(), Error>;
    /// Whether the current session, or `session_name`, exists.
    async fn alive(&self, session_name: Option<&str>) -> Result<bool, Error>;
    async fn capture(&self) -> Result<String, Error>;
    async fn send_keys(&self, keys: &[String]) -> Result<(), Error>;
    fn session_name(&self) -> String;
    fn set_session_name(&self, name: &str);
    /// Whether the watchdog can observe this runner (the Python supervisor creates no
    /// watchdog for a runner without `alive`/`capture`/`send_keys`).
    fn observable(&self) -> bool {
        true
    }
}

/// Whether the session must be wrapped in a transient systemd unit: Linux with
/// `systemd-run` on PATH, so a systemd-less Linux falls back to plain tmux.
pub fn linux_session_manager() -> bool {
    session_manager_for(std::env::consts::OS, |name| which(name).is_some())
}

pub fn session_manager_for(os: &str, found: impl Fn(&str) -> bool) -> bool {
    os == "linux" && found("systemd-run")
}

/// `shutil.which(name)`: an executable file on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The tmux driver; every verb targets the current session's name.
pub struct TmuxRunner<C: CommandRunner> {
    pub repo_root: PathBuf,
    pub mcp_url: String,
    run: C,
    pub socket_name: String,
    session_name: RefCell<String>,
    default_session_name: String,
    pub use_systemd_run: bool,
    pub systemd_unit: String,
    settings: Rc<dyn CopilotSettings>,
    /// `SESSION_ENV_KEYS` values, passed to the transient unit.
    pub session_env: Vec<(String, String)>,
    pub kill_retries: u32,
    pub kill_retry_delay: Duration,
    pub unit_stop_timeout: Duration,
    pub unit_poll: Duration,
}

impl<C: CommandRunner> TmuxRunner<C> {
    pub fn new(run: C, settings: Rc<dyn CopilotSettings>, repo_root: impl Into<PathBuf>, mcp_url: &str) -> Self {
        TmuxRunner {
            repo_root: repo_root.into(),
            mcp_url: mcp_url.to_owned(),
            run,
            socket_name: TMUX_SOCKET.to_owned(),
            session_name: RefCell::new(TMUX_SESSION.to_owned()),
            default_session_name: TMUX_SESSION.to_owned(),
            use_systemd_run: false,
            systemd_unit: SYSTEMD_SESSION_UNIT.to_owned(),
            settings,
            session_env: Vec::new(),
            kill_retries: KILL_RETRIES,
            kill_retry_delay: KILL_RETRY_DELAY,
            unit_stop_timeout: SYSTEMD_UNIT_STOP_TIMEOUT,
            unit_poll: SYSTEMD_UNIT_POLL,
        }
    }

    /// Binds the runner to another default session name (the constructor argument of
    /// the Python class).
    pub fn with_session_name(self, name: &str) -> Self {
        *self.session_name.borrow_mut() = name.to_owned();
        TmuxRunner { default_session_name: name.to_owned(), ..self }
    }

    pub fn command_runner(&self) -> &C {
        &self.run
    }

    fn tmux(&self, args: &[&str]) -> Vec<String> {
        let mut argv = vec!["tmux".to_owned(), "-L".to_owned(), self.socket_name.clone()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        argv
    }

    pub fn session_unit_name(&self) -> String {
        format!("{}.service", self.systemd_unit)
    }

    fn systemctl(args: &[&str]) -> Vec<String> {
        ["systemctl", "--user"].iter().chain(args).map(|a| (*a).to_owned()).collect()
    }

    /// Wraps `argv` in a transient `systemd --user` service. Type=oneshot +
    /// RemainAfterExit=yes match `tmux new-session -d`, which forks the server and
    /// returns at once: without RemainAfterExit the unit would count as finished (and,
    /// with --collect, be collected) the moment tmux detaches. KillMode=control-group
    /// makes an explicit stop reap the whole session; no Restart=, respawn is the
    /// supervisor's decision.
    fn systemd_run(&self, argv: Vec<String>) -> Vec<String> {
        let mut cmd: Vec<String> = vec![
            "systemd-run".into(),
            "--user".into(),
            "--collect".into(),
            format!("--unit={}", self.systemd_unit),
            "--property=Type=oneshot".into(),
            "--property=RemainAfterExit=yes".into(),
            "--property=KillMode=control-group".into(),
            format!("--working-directory={}", self.repo_root.display()),
        ];
        for (key, value) in &self.session_env {
            cmd.push(format!("--setenv={key}={value}"));
        }
        cmd.extend(argv);
        cmd
    }

    /// True while systemd still holds the transient unit under our name.
    async fn session_unit_loaded(&self) -> Result<bool, Error> {
        let unit = self.session_unit_name();
        let (rc, out) = self.run.run(&Self::systemctl(&["show", "-p", "LoadState", "--value", &unit])).await?;
        if rc != 0 {
            return Ok(false);
        }
        Ok(!matches!(py::strip(&out), "" | "not-found"))
    }

    /// Stops the transient unit and waits until systemd drops the name: a
    /// RemainAfterExit unit stays loaded after its session dies, and `systemd-run`
    /// refuses a loaded name, so a leftover would fail every later spawn.
    async fn reset_session_unit(&self) -> Result<(), Error> {
        if !self.use_systemd_run || !self.session_unit_loaded().await? {
            return Ok(());
        }
        let unit = self.session_unit_name();
        self.run.run(&Self::systemctl(&["stop", &unit])).await?;
        let deadline = tokio::time::Instant::now() + self.unit_stop_timeout;
        loop {
            if !self.session_unit_loaded().await? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error(format!("transient unit {unit} still loaded after stop")));
            }
            tokio::time::sleep(self.unit_poll).await;
        }
    }

    /// Exact tmux *session* target (`has-session`, `kill-session`): a bare name is
    /// prefix-matched, so `copilot` would resolve to `copilot-manual`.
    fn exact_target(session_name: &str) -> String {
        format!("={session_name}")
    }

    /// Exact *pane* target (`capture-pane`, `send-keys`): tmux rejects `=name` as a
    /// pane, the exact form needs the trailing colon (the session's current window and
    /// its active pane). Without it the capture came back empty and the dev-channels
    /// keystroke was dropped.
    fn exact_pane_target(session_name: &str) -> String {
        format!("={session_name}:")
    }

    /// The Claude invocation for `tmux new-session`, one shell string.
    /// `--strict-mcp-config --mcp-config` limits the session to the built-ins and the
    /// copilot daemon; the dev-channels flag lets the daemon's pushes reach the TUI.
    pub fn build_claude_command(&self) -> Result<String, Error> {
        let mcp_config = py::json_dumps(&json!({"mcpServers": {"copilot": {"type": "http", "url": self.mcp_url}}}));
        let task = self.settings.resolve().map_err(Error)?;
        if !task.enabled {
            return Err(Error("Copilot выключен в настройках.".to_owned()));
        }
        let mut args =
            vec!["claude".to_owned(), "--agent".into(), "copilot-session".into(), "--model".into(), task.model];
        if !task.claude_effort.is_empty() {
            args.extend(["--effort".to_owned(), task.claude_effort]);
        }
        args.extend([
            "--strict-mcp-config".to_owned(),
            "--mcp-config".into(),
            mcp_config,
            "--dangerously-load-development-channels".into(),
            "server:copilot".into(),
        ]);
        Ok(py::shlex_join(&args))
    }

    fn sibling_of(&self, name: &str) -> String {
        if name == TMUX_SESSION_MANUAL { self.default_session_name.clone() } else { TMUX_SESSION_MANUAL.to_owned() }
    }

    /// True while a session of the other kind still lives on the socket.
    async fn sibling_session_alive(&self, killed: &str) -> bool {
        if !self.use_systemd_run {
            return false;
        }
        let sibling = self.sibling_of(killed);
        self.alive(Some(&sibling)).await.unwrap_or(false)
    }

    async fn kill_tmux_session(&self, target: &str) -> Result<(), Error> {
        let (mut rc, mut out) = (0, String::new());
        for attempt in 0..self.kill_retries {
            if attempt > 0 {
                tokio::time::sleep(self.kill_retry_delay).await;
            }
            (rc, out) = self.run.run(&self.tmux(&["kill-session", "-t", &Self::exact_target(target)])).await?;
            if rc == 0 || !self.alive(Some(target)).await? {
                return Ok(());
            }
        }
        Err(Error(format!("tmux kill-session failed (rc={rc}): {}", py::strip(&out))))
    }
}

impl<C: CommandRunner> Runner for TmuxRunner<C> {
    /// Starts the session, adopting a live one instead of erroring: tmux refuses a
    /// duplicate name, which would burn a respawn attempt for nothing.
    async fn spawn(&self, meeting_id: Option<&str>) -> Result<bool, Error> {
        // Rebind to the name of the session's kind before the gate, so both the probe
        // and new-session target it.
        let name = if meeting_id == Some(MANUAL_MEETING_ID) {
            TMUX_SESSION_MANUAL.to_owned()
        } else {
            self.default_session_name.clone()
        };
        self.set_session_name(&name);
        // A live session of the other kind is stale by design (one copilot per
        // machine), e.g. a preemption whose kill ran out of retries: reap it first, and
        // give up the spawn when that fails, so two copilots cannot coexist.
        let sibling = self.sibling_of(&name);
        if self.alive(Some(&sibling)).await? {
            self.kill(Some(&sibling))
                .await
                .map_err(|_| Error(format!("stale sibling session {} could not be killed", py::str_repr(&sibling))))?;
        }
        if self.alive(None).await? {
            return Ok(false);
        }
        let root = self.repo_root.display().to_string();
        let command = self.build_claude_command()?;
        let mut argv = self.tmux(&["new-session", "-d", "-s", &name, "-c", &root, &command]);
        if self.use_systemd_run {
            // Only on the creating path: an adopted session's unit is never recreated.
            self.reset_session_unit().await?;
            argv = self.systemd_run(argv);
        }
        let (rc, out) = self.run.run(&argv).await?;
        if rc != 0 {
            return Err(Error(format!("tmux new-session failed (rc={rc}): {}", py::strip(&out))));
        }
        Ok(true)
    }

    /// Kills the session. A nonzero rc is checked against `has-session` before a retry
    /// or an error: gone is success (kill is idempotent), still alive after the retries
    /// is an error, so a failed kill is logged instead of orphaning a live Copilot.
    async fn kill(&self, session_name: Option<&str>) -> Result<(), Error> {
        let target = session_name.filter(|n| !n.is_empty()).map_or_else(|| self.session_name(), str::to_owned);
        self.kill_tmux_session(&target).await?;
        // The emptied unit stays loaded (RemainAfterExit): drop it so the next spawn
        // can claim the name. Not while the other kind lives: both share one tmux
        // server in that cgroup, and the stop would take the survivor down too.
        if self.sibling_session_alive(&target).await {
            return Ok(());
        }
        self.reset_session_unit().await
    }

    async fn alive(&self, session_name: Option<&str>) -> Result<bool, Error> {
        let target = session_name.filter(|n| !n.is_empty()).map_or_else(|| self.session_name(), str::to_owned);
        let (rc, _) = self.run.run(&self.tmux(&["has-session", "-t", &Self::exact_target(&target)])).await?;
        Ok(rc == 0)
    }

    async fn send_keys(&self, keys: &[String]) -> Result<(), Error> {
        let target = Self::exact_pane_target(&self.session_name());
        let mut args = vec!["send-keys", "-t", &target];
        args.extend(keys.iter().map(String::as_str));
        let (rc, out) = self.run.run(&self.tmux(&args)).await?;
        if rc != 0 {
            // A dropped keystroke is how a startup dialog stays up; the watchdog resends.
            log::rate_limited(
                "copilot-supervisor.send-keys",
                &format!("{} tmux send-keys failed (rc={rc}): {}", log::SUPERVISOR_PREFIX, py::strip(&out)),
            );
        }
        Ok(())
    }

    async fn capture(&self) -> Result<String, Error> {
        let target = Self::exact_pane_target(&self.session_name());
        let (rc, out) = self.run.run(&self.tmux(&["capture-pane", "-p", "-t", &target])).await?;
        if rc != 0 {
            // "" reads as an empty startup screen, so a silent failure would pin the
            // chip to `spawning`; logged, rate-limited (every watchdog tick).
            log::rate_limited(
                "copilot-supervisor.capture-pane",
                &format!("{} tmux capture-pane failed (rc={rc}): {}", log::SUPERVISOR_PREFIX, py::strip(&out)),
            );
            return Ok(String::new());
        }
        Ok(out)
    }

    fn session_name(&self) -> String {
        self.session_name.borrow().clone()
    }

    fn set_session_name(&self, name: &str) {
        *self.session_name.borrow_mut() = name.to_owned();
    }
}
