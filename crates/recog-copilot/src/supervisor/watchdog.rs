//! Polls the Copilot pane while a session is expected to live (`Watchdog`).
//!
//! Each tick classifies the pane (`offline` when the session is gone), answers the
//! known startup dialogs (dev-channels: `1`; workspace trust: the cursor onto the
//! accept option and Enter) once per dialog occurrence, again every
//! [`DISMISS_RETRY_S`] while it stays up, reported as `spawning` and not `blocked`;
//! reports `copilot_agent_status` on every change plus a heartbeat every
//! [`HEARTBEAT_S`] (the bridge measures the supervisor's freshness by it), and feeds
//! `session_ready` / `session_died` back. A pane stuck in `spawning` past
//! [`SPAWN_DEADLINE_S`] is reported `blocked`: fail-visible, a respawn would not help.

use super::lifecycle::State;
use super::screen::{
    AUTO_DISMISS_MARKERS, AgentState, TRUST_DIALOG_MARKERS, classify_screen, dialog_shown, trust_dialog_keys,
};
use super::tmux::{Error, Runner};

pub const POLL_INTERVAL_S: f64 = 2.0;
pub const HEARTBEAT_S: f64 = 5.0;
pub const SPAWN_DEADLINE_S: f64 = 60.0;
pub const DISMISS_RETRY_S: f64 = 10.0;

/// What the watchdog reads and drives: the supervisor in production, a bare lifecycle
/// in tests.
#[allow(async_fn_in_trait)]
pub trait WatchdogHost {
    fn lifecycle_state(&self) -> State;
    fn session_ready(&self);
    /// A spawn command is queued or executing.
    fn spawn_in_flight(&self) -> bool;
    async fn send_status(&self, status: AgentState);
    async fn on_session_died(&self);
}

/// Seconds on a monotonic clock.
pub type Clock = Box<dyn Fn() -> f64>;

/// The tokio clock (which tests can pause), in seconds since the first call.
pub fn tokio_clock() -> Clock {
    let start = tokio::time::Instant::now();
    Box::new(move || start.elapsed().as_secs_f64())
}

pub struct Watchdog {
    clock: Clock,
    pub last_reported: Option<AgentState>,
    last_report_ts: f64,
    dismissed_dialog: Option<&'static str>,
    dismissed_at: f64,
    spawning_since: Option<f64>,
}

impl Watchdog {
    pub fn new(clock: Clock) -> Self {
        Watchdog {
            clock,
            last_reported: None,
            last_report_ts: 0.0,
            dismissed_dialog: None,
            dismissed_at: 0.0,
            spawning_since: None,
        }
    }

    pub async fn tick<H: WatchdogHost, R: Runner>(&mut self, host: &H, runner: &R) -> Result<(), Error> {
        let now = (self.clock)();
        if host.lifecycle_state() == State::Idle {
            self.idle_tick(host, now).await;
            return Ok(());
        }

        let mut observed = self.observe(runner).await?;
        if host.spawn_in_flight() {
            // The visible pane still belongs to the previous session (or to a
            // half-created replacement): it cannot mark the replacement ready yet.
            observed = AgentState::Spawning;
        }
        if matches!(observed, AgentState::Online | AgentState::Working) {
            host.session_ready();
        }

        let effective = self.past_spawn_deadline(observed, now);
        if Some(effective) != self.last_reported || now - self.last_report_ts >= HEARTBEAT_S {
            self.report(host, effective, now).await;
        }
        if observed == AgentState::Offline {
            host.on_session_died().await;
        }
        Ok(())
    }

    /// No session should exist: no probing, no heartbeats (the bridge reads silence as
    /// offline), the chip closed once.
    async fn idle_tick<H: WatchdogHost>(&mut self, host: &H, now: f64) {
        self.dismissed_dialog = None;
        self.spawning_since = None;
        if self.last_reported.is_some_and(|s| s != AgentState::Offline) {
            self.report(host, AgentState::Offline, now).await;
        }
    }

    /// The state to report: a session spawning for longer than the deadline counts as
    /// blocked.
    fn past_spawn_deadline(&mut self, observed: AgentState, now: f64) -> AgentState {
        if observed != AgentState::Spawning {
            self.spawning_since = None;
            return observed;
        }
        let since = *self.spawning_since.get_or_insert(now);
        if now - since >= SPAWN_DEADLINE_S { AgentState::Blocked } else { observed }
    }

    async fn observe<R: Runner>(&mut self, runner: &R) -> Result<AgentState, Error> {
        if !runner.alive(None).await? {
            self.dismissed_dialog = None;
            return Ok(AgentState::Offline);
        }
        let text = runner.capture().await?;
        let state = classify_screen(Some(&text));
        // Only a dialog is answered, whatever else blocks the pane.
        let lowered = text.to_lowercase();
        if dialog_shown(&lowered)
            && let Some(marker) = AUTO_DISMISS_MARKERS.iter().find(|m| lowered.contains(*m))
        {
            self.dismiss(runner, marker, &text).await?;
            return Ok(AgentState::Spawning);
        }
        self.dismissed_dialog = None;
        Ok(state)
    }

    /// The latch is per dialog kind, not global: startup dialogs come back to back
    /// (trust, then dev-channels), each needs its own keypress; the same dialog is
    /// answered again only when still up after [`DISMISS_RETRY_S`], since a dropped
    /// keypress would otherwise wedge the session.
    async fn dismiss<R: Runner>(&mut self, runner: &R, marker: &'static str, text: &str) -> Result<(), Error> {
        let now = (self.clock)();
        if self.dismissed_dialog == Some(marker) && now - self.dismissed_at < DISMISS_RETRY_S {
            return Ok(());
        }
        self.dismissed_dialog = Some(marker);
        self.dismissed_at = now;
        let keys = if TRUST_DIALOG_MARKERS.contains(&marker) { trust_dialog_keys(text) } else { vec!["1".to_owned()] };
        runner.send_keys(&keys).await
    }

    async fn report<H: WatchdogHost>(&mut self, host: &H, state: AgentState, now: f64) {
        self.last_reported = Some(state);
        self.last_report_ts = now;
        host.send_status(state).await;
    }
}
