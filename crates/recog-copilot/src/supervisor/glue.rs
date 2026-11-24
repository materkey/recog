//! The glue around [`SessionLifecycle`]: bridge frames in, the grace timer, the respawn
//! backoff, the Copilot switch and the runner (`Supervisor` of the Python module).
//!
//! Single-threaded on purpose, like the asyncio original: every task runs on one
//! `LocalSet`, so state changes between two awaits are atomic and the races the Python
//! tests pin (a grace kill parked on the execute lock while a frame claims the
//! session) play out the same way. A `RefCell` borrow is never held across an await.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Duration;

use recog_core::protocol;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::{JoinHandle, spawn_local};
use tokio_tungstenite::tungstenite::Message;

use super::lifecycle::{Command, MANUAL_MEETING_ID, SessionLifecycle, State, reconcile, reconcile_without_autostart};
use super::screen::AgentState;
use super::tmux::{Runner, TMUX_SESSION, TMUX_SESSION_MANUAL};
use super::watchdog::{self, Watchdog, WatchdogHost};
use crate::ai_settings::CopilotSettings;
use crate::{log, py};

/// Timings the Python class keeps as attributes (tests shorten some of them).
#[derive(Clone, Debug)]
pub struct Tunables {
    pub grace_s: f64,
    /// At most this many respawn attempts per meeting, with these delays.
    pub respawn_backoff_s: Vec<f64>,
    pub initial_backoff_s: f64,
    pub max_backoff_s: f64,
    pub log_throttle_s: f64,
    pub settings_poll_s: f64,
    pub watchdog_poll_s: f64,
}

impl Tunables {
    pub fn new(grace_s: f64) -> Self {
        Tunables {
            grace_s,
            respawn_backoff_s: vec![5.0, 15.0, 30.0],
            initial_backoff_s: 1.0,
            max_backoff_s: 30.0,
            log_throttle_s: 60.0,
            settings_poll_s: 2.0,
            watchdog_poll_s: watchdog::POLL_INTERVAL_S,
        }
    }
}

/// `asyncio.sleep(seconds)`: a negative or invalid delay does not wait.
pub fn seconds(value: f64) -> Duration {
    Duration::try_from_secs_f64(value).unwrap_or(Duration::ZERO)
}

/// A frame's `meeting_id` as Python compares it: equal only to the same string, or
/// `None` to `None` (a missing key reads as `None`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawId {
    Null,
    Str(String),
    /// A number, list or object: never equal to a lifecycle id.
    Other,
}

impl RawId {
    pub fn of(value: Option<&Value>) -> Self {
        match value {
            None | Some(Value::Null) => RawId::Null,
            Some(Value::String(s)) => RawId::Str(s.clone()),
            Some(_) => RawId::Other,
        }
    }

    fn matches(&self, id: &Option<String>) -> bool {
        match (self, id) {
            (RawId::Null, None) => true,
            (RawId::Str(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn repr(&self, raw: Option<&Value>) -> String {
        match self {
            RawId::Null => "None".to_owned(),
            _ => raw.map(py::repr).unwrap_or_default(),
        }
    }
}

#[derive(Default)]
struct Inner {
    lifecycle: SessionLifecycle,
    enabled: bool,
    /// The level let the Copilot start by itself at the last check.
    automatic: bool,
    settings_error_logged: bool,
    latest_meeting_state: Option<Value>,
    respawn_attempts: usize,
    respawn_exhausted_logged: bool,
    respawn_task: Option<JoinHandle<()>>,
    grace_task: Option<JoinHandle<()>>,
    run_task: Option<JoinHandle<()>>,
    watchdog_task: Option<JoinHandle<()>>,
    settings_task: Option<JoinHandle<()>>,
    /// The connected bridge's outgoing queue; `None` while disconnected.
    ws: Option<UnboundedSender<Message>>,
    spawn_in_flight_count: usize,
    connected_once: bool,
    failure_streak: u32,
    last_failure_log: Option<tokio::time::Instant>,
}

fn is_pending(task: &Option<JoinHandle<()>>) -> bool {
    task.as_ref().is_some_and(|t| !t.is_finished())
}

pub struct Supervisor<R: Runner + 'static> {
    me: Weak<Self>,
    pub bridge_url: String,
    pub runner: Rc<R>,
    settings: Rc<dyn CopilotSettings>,
    /// `RECOG_COPILOT_AUTOSPAWN`: gates only the meeting-driven frames.
    pub autospawn: bool,
    /// Whether a copilot tmux session exists, for reconciliation; the runner's
    /// `alive()` unless a test replaces it.
    alive_probe: Option<Box<dyn Fn() -> bool>>,
    pub log_prefix: String,
    tunables: RefCell<Tunables>,
    st: RefCell<Inner>,
    /// Serialises command execution. The grace expiry holds it across its re-check
    /// and kill, so whichever of {grace kill, frame-driven spawn} gets it first settles
    /// before the other looks at the world (DW-10).
    execute_lock: tokio::sync::Mutex<()>,
}

/// Lowers the in-flight count when a spawn's `execute` ends, also when it is aborted.
struct InFlight<'a>(&'a RefCell<Inner>);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().spawn_in_flight_count -= 1;
    }
}

impl<R: Runner + 'static> Supervisor<R> {
    pub fn new(
        bridge_url: &str,
        runner: Rc<R>,
        settings: Rc<dyn CopilotSettings>,
        autospawn: bool,
        tunables: Tunables,
    ) -> Rc<Self> {
        Self::build(bridge_url, runner, settings, autospawn, tunables, None)
    }

    /// The same supervisor with a fixed answer to "does a session exist" (the
    /// `session_alive_probe` argument of the Python class).
    pub fn with_alive_probe(
        bridge_url: &str,
        runner: Rc<R>,
        settings: Rc<dyn CopilotSettings>,
        autospawn: bool,
        tunables: Tunables,
        probe: Box<dyn Fn() -> bool>,
    ) -> Rc<Self> {
        Self::build(bridge_url, runner, settings, autospawn, tunables, Some(probe))
    }

    fn build(
        bridge_url: &str,
        runner: Rc<R>,
        settings: Rc<dyn CopilotSettings>,
        autospawn: bool,
        tunables: Tunables,
        alive_probe: Option<Box<dyn Fn() -> bool>>,
    ) -> Rc<Self> {
        Rc::new_cyclic(|me| Supervisor {
            me: me.clone(),
            bridge_url: bridge_url.to_owned(),
            runner,
            settings,
            autospawn,
            alive_probe,
            log_prefix: log::SUPERVISOR_PREFIX.to_owned(),
            tunables: RefCell::new(tunables),
            st: RefCell::new(Inner { enabled: true, automatic: true, ..Inner::default() }),
            execute_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn me(&self) -> Rc<Self> {
        self.me.upgrade().expect("the supervisor is alive while its methods run")
    }

    fn log(&self, message: &str) {
        log::line(&format!("{} {message}", self.log_prefix));
    }

    // --- inspection, for tests and the bridge loop ---

    pub fn tunables(&self) -> std::cell::RefMut<'_, Tunables> {
        self.tunables.borrow_mut()
    }

    pub fn lifecycle(&self) -> SessionLifecycle {
        self.st.borrow().lifecycle.clone()
    }

    pub fn state(&self) -> State {
        self.st.borrow().lifecycle.state
    }

    pub fn meeting_id(&self) -> Option<String> {
        self.st.borrow().lifecycle.meeting_id.clone()
    }

    /// Runs `f` on the lifecycle, as the Python tests poke `sup.lifecycle`.
    pub fn with_lifecycle<T>(&self, f: impl FnOnce(&mut SessionLifecycle) -> T) -> T {
        f(&mut self.st.borrow_mut().lifecycle)
    }

    pub fn execute_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.execute_lock
    }

    pub fn is_enabled(&self) -> bool {
        self.st.borrow().enabled
    }

    pub fn grace_pending(&self) -> bool {
        self.st.borrow().grace_task.is_some()
    }

    pub fn grace_task_id(&self) -> Option<tokio::task::Id> {
        self.st.borrow().grace_task.as_ref().map(JoinHandle::id)
    }

    pub fn respawn_attempts(&self) -> usize {
        self.st.borrow().respawn_attempts
    }

    pub fn respawn_task_id(&self) -> Option<tokio::task::Id> {
        self.st.borrow().respawn_task.as_ref().map(JoinHandle::id)
    }

    pub fn respawn_pending(&self) -> bool {
        is_pending(&self.st.borrow().respawn_task)
    }

    pub fn spawn_in_flight(&self) -> bool {
        self.st.borrow().spawn_in_flight_count > 0
    }

    pub fn connected(&self) -> bool {
        self.st.borrow().ws.is_some()
    }

    pub fn watchdog_running(&self) -> bool {
        is_pending(&self.st.borrow().watchdog_task)
    }

    pub fn is_running(&self) -> bool {
        is_pending(&self.st.borrow().run_task)
    }

    /// Attaches (or detaches) the bridge's outgoing queue.
    pub fn set_ws(&self, ws: Option<UnboundedSender<Message>>) {
        self.st.borrow_mut().ws = ws;
    }

    // --- lifecycle event plumbing ---

    /// Drives the lifecycle from one bridge frame.
    ///
    /// `autospawn` gates only the three meeting-driven frames (D3); the manual frames
    /// are an explicit click relayed by the bridge and run regardless. The bridge
    /// already refused a spawn during a live conference, and `request_id` is its
    /// business: the supervisor answers only through `copilot_agent_status`.
    pub async fn handle_frame(&self, data: &Value) {
        let Some(frame) = data.as_object() else { return };
        let ftype = frame.get("type").and_then(Value::as_str);
        if let Some(kind @ (protocol::MEETING_STATE | protocol::MEETING_STARTED | protocol::MEETING_ENDED)) = ftype {
            let active = frame.get("active").cloned().unwrap_or(Value::Bool(kind == protocol::MEETING_STARTED));
            let meeting_id = frame.get("meeting_id").cloned().unwrap_or(Value::Null);
            self.st.borrow_mut().latest_meeting_state =
                Some(json!({"type": protocol::MEETING_STATE, "meeting_id": meeting_id, "active": active}));
        }
        if ftype == Some(protocol::COPILOT_SETTINGS_CHANGED) {
            self.refresh_enabled().await;
            return;
        }
        if !self.read_enabled() {
            self.refresh_enabled().await;
            return;
        }
        self.st.borrow_mut().enabled = true;
        match ftype {
            Some(kind @ (protocol::MEETING_STATE | protocol::MEETING_STARTED | protocol::MEETING_ENDED)) => {
                self.on_meeting_frame(kind, frame).await;
            }
            Some(protocol::COPILOT_CHANNEL_UNAVAILABLE) => {
                self.recover_channel_session(frame.get("meeting_id")).await;
            }
            Some(protocol::COPILOT_SPAWN_REQUEST) => self.on_spawn_request().await,
            Some(protocol::COPILOT_STOP_REQUEST) => self.on_stop_request(frame).await,
            _ => {}
        }
    }

    /// The meeting-driven frames, inert while `autospawn` is off (D3).
    async fn on_meeting_frame(&self, kind: &str, frame: &serde_json::Map<String, Value>) {
        if !self.autospawn {
            return;
        }
        let automatic = self.read_automatic();
        // Kept, so that the level reaching `automatic` later replays this meeting.
        self.st.borrow_mut().automatic = automatic;
        // Checked per meeting: a tool installed later serves the next one.
        if let Some(tool) = self.runner.missing_tool() {
            if automatic && kind == protocol::MEETING_STARTED {
                let meeting = frame.get("meeting_id").and_then(Value::as_str);
                self.log(&format!("{tool} is not installed: no Copilot for meeting {}", py::repr_opt(meeting)));
            }
            return;
        }
        match kind {
            protocol::MEETING_STATE => {
                let session_alive = self.probe_session_alive().await;
                let active = frame.get("active").is_some_and(py::truthy);
                let meeting_id = frame.get("meeting_id").filter(|v| py::truthy(v)).map(py::str_of);
                let commands = self.reconcile_at_level(automatic, active, meeting_id.as_deref(), session_alive);
                self.apply(commands).await;
            }
            protocol::MEETING_STARTED => {
                if let Some(meeting_id) = frame.get("meeting_id").filter(|v| py::truthy(v)).map(py::str_of) {
                    let commands = self.meeting_started_at_level(automatic, &meeting_id);
                    self.apply(commands).await;
                }
            }
            _ => {
                let meeting_id = frame.get("meeting_id").and_then(Value::as_str);
                let commands = self.st.borrow_mut().lifecycle.meeting_ended(meeting_id);
                self.apply(commands).await;
            }
        }
    }

    /// A conference started: below the `automatic` level it can only revive the
    /// session already running for it.
    fn meeting_started_at_level(&self, automatic: bool, meeting_id: &str) -> Vec<Command> {
        let lifecycle = &mut self.st.borrow_mut().lifecycle;
        if automatic { lifecycle.meeting_started(meeting_id) } else { lifecycle.meeting_resumed(meeting_id) }
    }

    /// The bridge's meeting state: below the `automatic` level no conference session is
    /// spawned, killed or replaced; the manual session's own state is reconciled as before.
    fn reconcile_at_level(
        &self,
        automatic: bool,
        active: bool,
        meeting_id: Option<&str>,
        session_alive: bool,
    ) -> Vec<Command> {
        let lifecycle = &mut self.st.borrow_mut().lifecycle;
        if automatic || meeting_id == Some(MANUAL_MEETING_ID) {
            reconcile(lifecycle, active, meeting_id, session_alive)
        } else {
            reconcile_without_autostart(lifecycle, active, meeting_id, session_alive)
        }
    }

    async fn on_spawn_request(&self) {
        // An explicit click is fresh intent, not a flap: reset the budget. A
        // session that spent every attempt parks in SPAWNING("manual"), where
        // the same-id flap emits no Spawn, so `apply` would never reset it and
        // Start would stay inert. A retry already pending is kept: cancelling
        // it would make a repeated click postpone recovery.
        if !self.respawn_pending() {
            self.reset_respawn_backoff();
        }
        let commands = self.st.borrow_mut().lifecycle.meeting_started(MANUAL_MEETING_ID);
        self.apply(commands).await;
    }

    async fn on_stop_request(&self, frame: &serde_json::Map<String, Value>) {
        // Immediate kill, no grace. Logged: the stuck "останавливается…"
        // incident of 2026-08-06 was undiagnosable because this path was silent.
        let commands = self.st.borrow_mut().lifecycle.stop_now(Some(MANUAL_MEETING_ID));
        if !commands.is_empty() {
            let request_id = frame.get("request_id").map_or_else(|| "None".to_owned(), py::repr);
            self.log(&format!("manual stop request {request_id} — killing session now"));
        }
        self.apply(commands).await;
    }

    /// Replaces a session whose MCP client disconnected, on the respawn budget.
    async fn recover_channel_session(&self, raw: Option<&Value>) {
        let meeting_id = RawId::of(raw);
        let _guard = self.execute_lock.lock().await;
        {
            let st = self.st.borrow();
            if st.lifecycle.state != State::Running
                || !meeting_id.matches(&st.lifecycle.meeting_id)
                || st.spawn_in_flight_count > 0
                || is_pending(&st.respawn_task)
                || st.respawn_attempts >= self.tunables.borrow().respawn_backoff_s.len()
            {
                return;
            }
        }
        self.log(&format!("MCP session lost for {} — restarting agent", meeting_id.repr(raw)));
        self.execute_locked(&[Command::Kill]).await;
        // A new meeting or a Stop may have arrived while the runner was stopping.
        let still = {
            let st = self.st.borrow();
            meeting_id.matches(&st.lifecycle.meeting_id) && st.lifecycle.state == State::Running
        };
        if still {
            self.on_session_died();
        }
    }

    async fn probe_session_alive(&self) -> bool {
        if let Some(probe) = &self.alive_probe {
            return probe();
        }
        match self.runner.alive(None).await {
            Ok(alive) => alive,
            Err(e) => {
                self.log(&format!("session probe failed: {e}"));
                false
            }
        }
    }

    /// The Copilot switch of the AI settings, as far as the AI functions level allows a
    /// Copilot at all; an unreadable file counts as off.
    pub fn read_enabled(&self) -> bool {
        match self.settings.resolve() {
            Ok(task) => {
                self.st.borrow_mut().settings_error_logged = false;
                task.enabled
            }
            Err(e) => {
                if !self.st.borrow().settings_error_logged {
                    self.log(&format!("cannot read Copilot switch: {e}"));
                    self.st.borrow_mut().settings_error_logged = true;
                }
                false
            }
        }
    }

    /// Follows the switch and the AI functions level: turning the Copilot off stops the
    /// session and its retries; turning it back on replays the latest conference state;
    /// the level reaching `automatic` is `replay_for_automatic`.
    pub async fn refresh_enabled(&self) -> bool {
        let automatic_turned_on = self.note_automatic();
        if self.st.borrow().enabled && self.read_enabled() {
            self.replay_for_automatic(automatic_turned_on).await;
            return true;
        }
        let was_enabled;
        {
            let _guard = self.execute_lock.lock().await;
            was_enabled = self.st.borrow().enabled;
            let enabled = self.read_enabled();
            self.st.borrow_mut().enabled = enabled;
            if !enabled {
                self.reset_respawn_backoff();
                let commands = {
                    let mut st = self.st.borrow_mut();
                    let current = st.lifecycle.meeting_id.clone();
                    st.lifecycle.stop_now(current.as_deref())
                };
                self.sync_grace_timer();
                if !commands.is_empty() || self.probe_session_alive().await {
                    self.execute_locked(&[Command::Kill]).await;
                }
                self.send_agent_status(AgentState::Offline);
            }
        }
        let enabled = self.st.borrow().enabled;
        if enabled && !was_enabled {
            self.replay_latest_meeting().await;
        }
        enabled
    }

    /// Whether the AI functions level just reached `automatic`; keeps the new value.
    fn note_automatic(&self) -> bool {
        let automatic = self.read_automatic();
        let mut st = self.st.borrow_mut();
        let turned_on = automatic && !st.automatic;
        st.automatic = automatic;
        turned_on
    }

    /// The level just reached `automatic`: the latest state is replayed only while it has
    /// a conference on, so that conference gets its Copilot and a session started by hand
    /// is not ended by a state saying no conference is on.
    async fn replay_for_automatic(&self, turned_on: bool) {
        if turned_on && self.conference_on() {
            self.replay_latest_meeting().await;
        }
    }

    /// Whether the bridge's latest state has a conference on.
    fn conference_on(&self) -> bool {
        let st = self.st.borrow();
        st.latest_meeting_state.as_ref().and_then(|state| state.get("active")).is_some_and(py::truthy)
    }

    /// The level lets the Copilot start by itself; unreadable settings do not.
    pub fn read_automatic(&self) -> bool {
        self.settings.resolve().is_ok_and(|task| task.automatic)
    }

    /// Replays the bridge's latest conference state, so a Copilot just allowed starts
    /// for the meeting already on.
    async fn replay_latest_meeting(&self) {
        let latest = self.st.borrow().latest_meeting_state.clone();
        if let Some(latest) = latest
            && latest.get("meeting_id") != Some(&json!(MANUAL_MEETING_ID))
        {
            Box::pin(self.handle_frame(&latest)).await;
        }
    }

    /// Re-adopts a surviving manual session at startup (DW-9). The session's name
    /// carries its kind, so the probe reads it once, while IDLE; the default name is
    /// asked only when the manual one is alive, to arbitrate:
    /// - manual alive, default dead: adopt "manual" and rebind the runner, so the hello
    ///   carries "manual" and the bridge re-arms its manual mirror;
    /// - both alive: the conference wins (one copilot per machine), the stale manual
    ///   session is killed;
    /// - a surviving conference session is left to the `meeting_state` reconciliation.
    ///
    /// Not gated on `autospawn` (D3); a probe failure is logged and adopts nothing.
    pub async fn startup_manual_readoption(&self) {
        if self.state() != State::Idle {
            return;
        }
        let probed = async {
            let manual = self.runner.alive(Some(TMUX_SESSION_MANUAL)).await?;
            let default = manual && self.runner.alive(Some(TMUX_SESSION)).await?;
            Ok::<_, super::tmux::Error>((manual, default))
        };
        let (manual_alive, default_alive) = match probed.await {
            Ok(alive) => alive,
            Err(e) => {
                self.log(&format!("manual-session startup probe failed: {e}"));
                return;
            }
        };
        if !manual_alive {
            return;
        }
        if default_alive {
            self.log(&format!(
                "both '{TMUX_SESSION}' and '{TMUX_SESSION_MANUAL}' tmux sessions alive at startup — \
                 conference wins, killing the stale manual session"
            ));
            if let Err(e) = self.runner.kill(Some(TMUX_SESSION_MANUAL)).await {
                self.log(&format!("stale manual session kill failed: {e}"));
            }
            return;
        }
        self.with_lifecycle(|l| l.adopt(Some(MANUAL_MEETING_ID)));
        self.runner.set_session_name(TMUX_SESSION_MANUAL);
        self.log(&format!("re-adopted live manual copilot session ({TMUX_SESSION_MANUAL})"));
    }

    async fn apply(&self, commands: Vec<Command>) {
        if commands.iter().any(|c| matches!(c, Command::Spawn(_))) {
            // A meeting-driven spawn starts a fresh session: a fresh budget, and no
            // respawn left over from the old one.
            self.reset_respawn_backoff();
        }
        self.sync_grace_timer();
        self.execute(&commands).await;
    }

    pub async fn execute(&self, commands: &[Command]) {
        if commands.is_empty() {
            return;
        }
        // Counted before waiting for the lock: the lifecycle is already SPAWNING, so a
        // watchdog tick must read an OFFLINE session as the expected replacement gap
        // while another command still owns the lock. A count, so that overlapping
        // callers do not clear the flag under each other.
        let _in_flight = if commands.iter().any(|c| matches!(c, Command::Spawn(_))) {
            self.st.borrow_mut().spawn_in_flight_count += 1;
            Some(InFlight(&self.st))
        } else {
            None
        };
        let _guard = self.execute_lock.lock().await;
        self.execute_locked(commands).await;
    }

    /// Callers hold the execute lock.
    async fn execute_locked(&self, commands: &[Command]) {
        for command in commands {
            let result = match command {
                Command::Kill => self.runner.kill(None).await,
                Command::Spawn(meeting_id) => {
                    if !self.read_enabled() {
                        let mut st = self.st.borrow_mut();
                        let current = st.lifecycle.meeting_id.clone();
                        st.lifecycle.stop_now(current.as_deref());
                        continue;
                    }
                    self.runner.spawn(meeting_id.as_deref()).await.map(|_| ())
                }
            };
            if let Err(e) = result {
                self.log(&format!("command {command} failed: {e}"));
            }
        }
    }

    /// `supervisor_hello.current_meeting_id` (D5): the id of a session the machine means
    /// to keep alive. GRACE and IDLE report none: a session already scheduled to die
    /// must not be re-armed by the bridge's reply, or a reconnect inside the grace
    /// window would defeat the user's Stop.
    pub fn live_meeting_id(&self) -> Option<String> {
        let st = self.st.borrow();
        match st.lifecycle.state {
            State::Spawning | State::Running => st.lifecycle.meeting_id.clone(),
            State::Idle | State::Grace => None,
        }
    }

    /// `copilot_agent_status` to the bridge; dropped while disconnected (the bridge
    /// reads supervisor silence as offline, which is the truth then anyway).
    pub fn send_agent_status(&self, status: AgentState) {
        let st = self.st.borrow();
        let Some(ws) = &st.ws else { return };
        let frame = json!({
            "type": protocol::COPILOT_AGENT_STATUS,
            "status": status.as_str(),
            "meeting_id": st.lifecycle.meeting_id,
        });
        if ws.send(Message::text(py::json_dumps(&frame))).is_err() {
            self.log("status send failed: the bridge connection is closed");
        }
    }

    // --- respawn with backoff (session_died from the watchdog) ---

    /// The session vanished while it should be alive: respawn on the per-meeting
    /// budget. A pending respawn absorbs repeated reports; every fresh meeting-driven
    /// spawn resets the budget. Not gated on `autospawn` (D3): this only fires for a
    /// session the machine already decided should live; the IDLE machine, not the flag,
    /// keeps a dead session dead.
    pub fn on_session_died(&self) {
        if self.respawn_pending() {
            return;
        }
        let commands = self.st.borrow_mut().lifecycle.session_died();
        self.sync_grace_timer();
        if !commands.iter().any(|c| matches!(c, Command::Spawn(_))) {
            return;
        }
        let delays = self.tunables.borrow().respawn_backoff_s.clone();
        let mut st = self.st.borrow_mut();
        if st.respawn_attempts >= delays.len() {
            if !st.respawn_exhausted_logged {
                st.respawn_exhausted_logged = true;
                let meeting = py::repr_opt(st.lifecycle.meeting_id.as_deref());
                drop(st);
                self.log(&format!("respawn attempts exhausted for meeting {meeting} — leaving session down"));
            }
            return;
        }
        let delay = delays[st.respawn_attempts];
        st.respawn_attempts += 1;
        let meeting_id = st.lifecycle.meeting_id.clone();
        let me = self.me();
        st.respawn_task = Some(spawn_local(async move { me.respawn_after(delay, meeting_id, commands).await }));
    }

    async fn respawn_after(&self, delay: f64, meeting_id: Option<String>, commands: Vec<Command>) {
        tokio::time::sleep(seconds(delay)).await;
        self.st.borrow_mut().respawn_task = None;
        // The meeting may have ended or changed during the sleep: respawn only if the
        // machine still waits for this very session.
        let waiting = {
            let st = self.st.borrow();
            st.lifecycle.state == State::Spawning && st.lifecycle.meeting_id == meeting_id
        };
        if waiting {
            self.execute(&commands).await;
        }
    }

    fn reset_respawn_backoff(&self) {
        let mut st = self.st.borrow_mut();
        st.respawn_attempts = 0;
        st.respawn_exhausted_logged = false;
        if let Some(task) = st.respawn_task.take() {
            task.abort();
        }
    }

    // --- grace timer ---

    fn sync_grace_timer(&self) {
        let mut st = self.st.borrow_mut();
        if st.lifecycle.state == State::Grace {
            if !is_pending(&st.grace_task) {
                let me = self.me();
                st.grace_task = Some(spawn_local(async move { me.grace_expire().await }));
            }
        } else if let Some(task) = st.grace_task.take() {
            task.abort();
        }
    }

    async fn grace_expire(&self) {
        let grace = self.tunables.borrow().grace_s;
        tokio::time::sleep(seconds(grace)).await;
        let commands = {
            let mut st = self.st.borrow_mut();
            st.grace_task = None;
            st.lifecycle.timer_expired()
        };
        if commands.is_empty() {
            return;
        }
        let _guard = self.execute_lock.lock().await;
        // A frame-driven spawn or adopt may have claimed the still-live session while
        // this waited for the lock (state left IDLE); the queued kill would destroy it.
        if self.state() != State::Idle {
            return;
        }
        self.execute_locked(&commands).await;
    }

    // --- run loop ---

    /// Starts [`Self::run`] on the current `LocalSet`.
    pub fn start(&self) {
        if self.st.borrow().run_task.is_some() {
            return;
        }
        let me = self.me();
        let task = spawn_local(async move { me.run().await });
        self.st.borrow_mut().run_task = Some(task);
    }

    /// Stops the run loop and every timer.
    pub fn stop(&self) {
        let mut st = self.st.borrow_mut();
        for task in [
            st.settings_task.take(),
            st.grace_task.take(),
            st.respawn_task.take(),
            st.watchdog_task.take(),
            st.run_task.take(),
        ]
        .into_iter()
        .flatten()
        {
            task.abort();
        }
        st.ws = None;
    }

    /// The supervisor's life: the startup kind-probe, the Copilot switch, the watchdog,
    /// then the bridge connection, reconnected with backoff.
    pub async fn run(&self) {
        // Before the watchdog and the first connect, so the hello already reports an
        // adopted manual session.
        self.startup_manual_readoption().await;
        self.refresh_enabled().await;
        let me = self.me();
        let settings = spawn_local(async move {
            loop {
                me.refresh_enabled().await;
                let poll = me.tunables.borrow().settings_poll_s;
                tokio::time::sleep(seconds(poll)).await;
            }
        });
        self.st.borrow_mut().settings_task = Some(settings);
        // The watchdog polls tmux for the supervisor's whole life; its statuses reach
        // the bridge only while connected.
        self.start_watchdog();
        let mut delay = self.tunables.borrow().initial_backoff_s;
        loop {
            if let Err(e) = super::bridge::serve(self, &mut delay).await {
                self.on_failure(&e, delay);
            }
            self.set_ws(None);
            tokio::time::sleep(seconds(delay)).await;
            delay = (delay * 2.0).min(self.tunables.borrow().max_backoff_s);
        }
    }

    fn start_watchdog(&self) {
        if !self.runner.observable() || is_pending(&self.st.borrow().watchdog_task) {
            return;
        }
        let me = self.me();
        let clock = watchdog::tokio_clock();
        let task = spawn_local(async move {
            let mut watchdog = Watchdog::new(clock);
            loop {
                if let Err(e) = watchdog.tick(&*me, &*me.runner).await {
                    me.log(&format!("watchdog tick failed: {e}"));
                }
                let poll = me.tunables.borrow().watchdog_poll_s;
                tokio::time::sleep(seconds(poll)).await;
            }
        });
        self.st.borrow_mut().watchdog_task = Some(task);
    }

    pub(crate) fn on_connected(&self) {
        let verb = {
            let mut st = self.st.borrow_mut();
            st.failure_streak = 0;
            let verb = if st.connected_once { "reconnected" } else { "connected" };
            st.connected_once = true;
            verb
        };
        self.log(&format!("{verb} to {}", self.bridge_url));
    }

    fn on_failure(&self, error: &super::bridge::BridgeError, delay: f64) {
        let now = tokio::time::Instant::now();
        let throttle = seconds(self.tunables.borrow().log_throttle_s);
        let due = {
            let mut st = self.st.borrow_mut();
            st.failure_streak += 1;
            let due = st.failure_streak == 1 || st.last_failure_log.is_none_or(|t| now.duration_since(t) >= throttle);
            if due {
                st.last_failure_log = Some(now);
            }
            due
        };
        if due {
            self.log(&format!("bridge unavailable ({}: {}); retry in {delay:.1}s", error.kind, error.message));
        }
    }
}

impl<R: Runner + 'static> WatchdogHost for Supervisor<R> {
    fn lifecycle_state(&self) -> State {
        self.state()
    }

    fn session_ready(&self) {
        self.with_lifecycle(SessionLifecycle::session_ready);
    }

    fn spawn_in_flight(&self) -> bool {
        Supervisor::spawn_in_flight(self)
    }

    async fn send_status(&self, status: AgentState) {
        self.send_agent_status(status);
    }

    async fn on_session_died(&self) {
        Supervisor::on_session_died(self);
    }
}
