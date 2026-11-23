//! The pure lifecycle state machine of `recog/copilot_supervisor.py` (`SessionLifecycle`).
//!
//! ```text
//! IDLE ──meeting_started(A)──► SPAWNING ──session_ready──► RUNNING(A)
//! RUNNING(A) ──meeting_ended(A)──► GRACE(A, timer)
//! GRACE(A) ──meeting_started(A)──► RUNNING(A)      (flap: same id — keep session)
//! GRACE(A) ──meeting_started(B)──► kill + SPAWNING(B)
//! GRACE(A) ──timer expired──► kill + IDLE
//! RUNNING(A) ──meeting_started(B)──► kill + SPAWNING(B)
//! RUNNING(A) ──session_died──► SPAWNING(A)          (respawn; backoff in the glue)
//! RUNNING/SPAWNING/GRACE(A) ──stop_now(A)──► kill + IDLE   (explicit Stop: no grace)
//! ```
//!
//! No I/O and no timers: every input returns the commands for the caller to execute;
//! the grace timer lives in the glue, which keeps it in step with `State::Grace`.

use std::fmt;

use recog_core::protocol;

/// Synthetic lifecycle id of a webapp-driven (manual) session. Twin of
/// `ktalk_bridge.MANUAL_MEETING_ID`: the bridge keys its manual mirror and its
/// `meeting_state` reply off the very same string.
pub const MANUAL_MEETING_ID: &str = protocol::MANUAL_MEETING_ID;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Spawning,
    Running,
    Grace,
}

/// An output of the machine, executed by the supervisor glue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Spawn(Option<String>),
    Kill,
}

impl fmt::Display for Command {
    /// The Python dataclass repr, which the error log lines carry.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Spawn(id) => write!(f, "Spawn(meeting_id={})", crate::py::repr_opt(id.as_deref())),
            Command::Kill => f.write_str("Kill()"),
        }
    }
}

/// `meeting_id == None` while not IDLE means "adopted session, meeting unknown"
/// (supervisor restart): it matches any incoming id, so a rejoin flap keeps the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLifecycle {
    pub state: State,
    pub meeting_id: Option<String>,
}

impl Default for SessionLifecycle {
    fn default() -> Self {
        SessionLifecycle { state: State::Idle, meeting_id: None }
    }
}

impl SessionLifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    fn same_meeting(&self, meeting_id: Option<&str>) -> bool {
        self.meeting_id.is_none() || self.meeting_id.as_deref() == meeting_id
    }

    pub fn meeting_started(&mut self, meeting_id: &str) -> Vec<Command> {
        if self.state == State::Idle {
            self.state = State::Spawning;
            self.meeting_id = Some(meeting_id.to_owned());
            return vec![Command::Spawn(Some(meeting_id.to_owned()))];
        }
        if self.same_meeting(Some(meeting_id)) {
            // Resolves an unknown id after adopt.
            self.meeting_id = Some(meeting_id.to_owned());
            if self.state == State::Grace {
                self.state = State::Running;
            }
            return vec![];
        }
        // A different meeting while a session exists: back-to-back meetings, replace it.
        self.state = State::Spawning;
        self.meeting_id = Some(meeting_id.to_owned());
        vec![Command::Kill, Command::Spawn(Some(meeting_id.to_owned()))]
    }

    /// The same-meeting branch of [`SessionLifecycle::meeting_started`] alone, for when
    /// the Copilot may not start by itself: a rejoin still revives a session in GRACE,
    /// and nothing is ever spawned, killed or replaced.
    pub fn meeting_resumed(&mut self, meeting_id: &str) -> Vec<Command> {
        if self.state != State::Idle && self.same_meeting(Some(meeting_id)) {
            self.meeting_id = Some(meeting_id.to_owned());
            if self.state == State::Grace {
                self.state = State::Running;
            }
        }
        vec![]
    }

    pub fn meeting_ended(&mut self, meeting_id: Option<&str>) -> Vec<Command> {
        if matches!(self.state, State::Running | State::Spawning) && self.same_meeting(meeting_id) {
            self.state = State::Grace;
        }
        vec![]
    }

    pub fn timer_expired(&mut self) -> Vec<Command> {
        if self.state != State::Grace {
            return vec![];
        }
        self.state = State::Idle;
        self.meeting_id = None;
        vec![Command::Kill]
    }

    pub fn session_died(&mut self) -> Vec<Command> {
        match self.state {
            State::Running | State::Spawning => {
                self.state = State::Spawning;
                vec![Command::Spawn(self.meeting_id.clone())]
            }
            State::Grace => {
                // Nothing left to keep alive through grace, nothing to kill either.
                self.state = State::Idle;
                self.meeting_id = None;
                vec![]
            }
            State::Idle => vec![],
        }
    }

    pub fn session_ready(&mut self) -> Vec<Command> {
        if self.state == State::Spawning {
            self.state = State::Running;
        }
        vec![]
    }

    /// An explicit Stop: kill at once. The grace period exists for conference rejoin
    /// flaps; a click is fresh intent. A stop for a session bound to another meeting
    /// stays inert, by the same id rule as `meeting_ended`.
    pub fn stop_now(&mut self, meeting_id: Option<&str>) -> Vec<Command> {
        if self.state == State::Idle || !self.same_meeting(meeting_id) {
            return vec![];
        }
        self.state = State::Idle;
        self.meeting_id = None;
        vec![Command::Kill]
    }

    /// Attach to a live tmux session without spawning (reconciliation after a restart).
    pub fn adopt(&mut self, meeting_id: Option<&str>) -> Vec<Command> {
        self.state = State::Running;
        self.meeting_id = meeting_id.map(str::to_owned);
        vec![]
    }
}

/// The supervisor's reaction to the bridge's `meeting_state` reply (`_reconcile`).
///
/// `meeting_id` is `str()` of the frame's id when it is truthy, else `None`.
/// - session exists, no active meeting: a synthetic `meeting_ended`, RUNNING→GRACE and
///   not an instant kill; after a bridge restart `active=false` is transient and the
///   same-id flap revives the session.
/// - no session, meeting active: spawn.
/// - session alive, meeting active: adopt; a restart mid-meeting must not kill it.
pub fn reconcile(
    machine: &mut SessionLifecycle,
    active: bool,
    meeting_id: Option<&str>,
    session_alive: bool,
) -> Vec<Command> {
    let session_exists = machine.state != State::Idle || session_alive;
    if let (true, Some(meeting_id)) = (active, meeting_id) {
        if !session_exists {
            return machine.meeting_started(meeting_id);
        }
        if machine.state == State::Idle {
            return machine.adopt(Some(meeting_id));
        }
        // Same id: no-op or GRACE revival; another id: kill + spawn.
        return machine.meeting_started(meeting_id);
    }
    no_active_meeting(machine, session_exists)
}

/// [`reconcile`] below the `automatic` level: a running session is still adopted and
/// ended, but none is spawned, killed or replaced for a conference.
pub fn reconcile_without_autostart(
    machine: &mut SessionLifecycle,
    active: bool,
    meeting_id: Option<&str>,
    session_alive: bool,
) -> Vec<Command> {
    let session_exists = machine.state != State::Idle || session_alive;
    if let (true, Some(meeting_id)) = (active, meeting_id) {
        if !session_exists {
            return vec![];
        }
        if machine.state == State::Idle {
            return machine.adopt(Some(meeting_id));
        }
        return machine.meeting_resumed(meeting_id);
    }
    no_active_meeting(machine, session_exists)
}

/// The bridge reports no active meeting: a session that exists goes to GRACE.
fn no_active_meeting(machine: &mut SessionLifecycle, session_exists: bool) -> Vec<Command> {
    if session_exists {
        if machine.state == State::Idle {
            machine.adopt(None);
        }
        let current = machine.meeting_id.clone();
        return machine.meeting_ended(current.as_deref());
    }
    vec![]
}
