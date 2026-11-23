//! What the Codex harness does with each Copilot event and app-server event, without
//! I/O (docs/plans/2026-11-23-codex-copilot.md, D2 and D4).
//!
//! One turn runs at a time. A user message steers a running turn at once (Codex adds
//! it to the turn); a transcript tick waits for the turn, and only the latest one is
//! kept. A spent subscription or a lost login blocks the harness: ticks are dropped
//! until a later turn succeeds, user messages still try.

use recog_core::protocol;

use super::app_server::{Event, Failure, TurnStatus};

/// The start of the pane's last line, which the supervisor's screen classifier reads.
pub const STATUS_PREFIX: &str = "recog-copilot-codex: ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Spawning,
    Online,
    Working,
    Blocked(String),
}

impl Status {
    /// The status line, on one line whatever the reason says.
    pub fn line(&self) -> String {
        let state = match self {
            Status::Spawning => "spawning".to_owned(),
            Status::Online => "online".to_owned(),
            Status::Working => "working".to_owned(),
            Status::Blocked(reason) => format!("blocked: {}", reason.split_whitespace().collect::<Vec<_>>().join(" ")),
        };
        format!("{STATUS_PREFIX}{state}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `turn/start` with this text: a new turn, or a steer of the running one.
    StartTurn(String),
    /// Replace the thread, whose context is full, with a fresh one.
    NewThread,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    UserMessage,
    Tick,
    MeetingEnded,
    Other,
}

impl Kind {
    pub fn of(event_type: &str) -> Kind {
        match event_type {
            protocol::COPILOT_USER_MESSAGE => Kind::UserMessage,
            protocol::TRANSCRIPT_TICK => Kind::Tick,
            protocol::MEETING_ENDED => Kind::MeetingEnded,
            _ => Kind::Other,
        }
    }
}

const LOGIN: &str = "not signed in to Codex: run codex login";

#[derive(Debug)]
pub struct Harness {
    status: Status,
    busy: bool,
    held_tick: Option<String>,
    ending: bool,
    failure: Option<Failure>,
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    pub fn new() -> Self {
        Harness { status: Status::Spawning, busy: false, held_tick: None, ending: false, failure: None }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The thread exists; `logged_in` from `account/read`.
    pub fn ready(&mut self, logged_in: bool) {
        self.status = if logged_in { Status::Online } else { Status::Blocked(LOGIN.into()) };
    }

    pub fn on_copilot(&mut self, kind: Kind, text: String) -> Vec<Command> {
        if kind == Kind::Tick && (self.ending || matches!(self.status, Status::Blocked(_))) {
            return Vec::new();
        }
        if kind == Kind::Tick && self.busy {
            self.held_tick = Some(text);
            return Vec::new();
        }
        self.ending |= kind == Kind::MeetingEnded;
        self.start(text)
    }

    pub fn on_app(&mut self, event: &Event) -> Vec<Command> {
        match event {
            Event::Error { failure, will_retry: false } => self.failure = Some(failure.clone()),
            Event::TurnCompleted { status, error, .. } => return self.completed(*status, error.clone()),
            _ => {}
        }
        Vec::new()
    }

    fn start(&mut self, text: String) -> Vec<Command> {
        self.busy = true;
        if !matches!(self.status, Status::Blocked(_)) {
            self.status = Status::Working;
        }
        vec![Command::StartTurn(text)]
    }

    fn completed(&mut self, status: TurnStatus, error: Option<Failure>) -> Vec<Command> {
        self.busy = false;
        let failure = error.or_else(|| self.failure.take());
        self.failure = None;
        let mut commands = Vec::new();
        self.status = Status::Online;
        if status == TurnStatus::Failed
            && let Some(info) = failure.and_then(|failure| failure.info)
        {
            match info.as_str() {
                "usageLimitExceeded" => self.status = Status::Blocked("Codex usage limit reached".into()),
                "rateLimitExceeded" => self.status = Status::Blocked("Codex rate limit reached".into()),
                "unauthorized" => self.status = Status::Blocked(LOGIN.into()),
                "contextWindowExceeded" => commands.push(Command::NewThread),
                _ => {}
            }
        }
        if self.ending {
            commands.push(Command::Exit);
        } else if let Some(tick) = self.held_tick.take()
            && !matches!(self.status, Status::Blocked(_))
        {
            commands.extend(self.start(tick));
        }
        commands
    }
}
