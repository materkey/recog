//! Screen classification and the watchdog tick (tests/test_copilot_supervisor.py,
//! "Watchdog: screen classification + tick behaviour").
#![forbid(unsafe_code)]

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use common::*;
use recog_copilot::supervisor::watchdog::{DISMISS_RETRY_S, SPAWN_DEADLINE_S};
use recog_copilot::supervisor::{
    AgentState, Error, Runner, SessionLifecycle, State, Watchdog, WatchdogHost, classify_screen, trust_dialog_keys,
};

#[test]
fn each_fixture_screen_classifies_as_python_does() {
    // given
    let cases = [
        (Some(DEV_CHANNELS_SCREEN), AgentState::Blocked), // known dev-channels dialog
        (Some(TRUST_DIALOG_SCREEN), AgentState::Blocked), // known workspace-trust dialog
        (Some(TRUST_DIALOG_NUMBERED_SCREEN), AgentState::Blocked), // older numbered layout
        (Some(MODEL_DIALOG_SCREEN), AgentState::Blocked), // unknown selection dialog
        (Some(SPINNER_SCREEN), AgentState::Working),      // spinner beats the prompt line
        (Some(REPL_PROMPT_SCREEN), AgentState::Online),   // idle prompt
        (Some(""), AgentState::Spawning),                 // empty pane is transitional
        (Some(STARTUP_SCREEN), AgentState::Spawning),     // startup, no markers yet
        (None, AgentState::Offline),                      // tmux session gone
    ];
    for (screen, expected) in cases {
        // when / then
        assert_eq!(classify_screen(screen), expected, "{screen:?}");
    }
}

#[test]
fn each_agent_state_when_reported_then_bridge_status_word() {
    // given
    let cases = [
        (AgentState::Spawning, "spawning"),
        (AgentState::Online, "online"),
        (AgentState::Working, "working"),
        (AgentState::Blocked, "blocked"),
        (AgentState::Offline, "offline"),
    ];
    for (state, word) in cases {
        // when / then
        assert_eq!(state.as_str(), word);
    }
}

#[test]
fn trust_dialog_with_cursor_below_the_accept_option_moves_up_then_confirms() {
    // given
    let screen = "   Yes, I trust this folder\n ❯ No, exit\n Enter to confirm\n";
    // when / then
    assert_eq!(trust_dialog_keys(screen), ["Up", "Enter"]);
}

#[test]
fn trust_dialog_without_a_recognisable_accept_option_keeps_the_fixed_answer() {
    // given
    let screen = " Do you trust the files in this folder?\n ❯ 1. Proceed\n   2. Exit\n";
    // when / then
    assert_eq!(trust_dialog_keys(screen), ["1"]);
}

/// Programmable runner: alive flag, pane text, key log (`WatchdogRunner`).
struct WatchdogRunner {
    alive: Cell<bool>,
    screen: RefCell<String>,
    sent_keys: RefCell<Vec<Vec<String>>>,
}

impl WatchdogRunner {
    fn new(alive: bool, screen: &str) -> Self {
        WatchdogRunner { alive: Cell::new(alive), screen: RefCell::new(screen.into()), sent_keys: RefCell::default() }
    }

    fn show(&self, screen: &str) {
        *self.screen.borrow_mut() = screen.to_owned();
    }

    fn keys(&self) -> Vec<Vec<String>> {
        self.sent_keys.borrow().clone()
    }
}

impl Runner for WatchdogRunner {
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
        self.sent_keys.borrow_mut().push(keys.to_vec());
        Ok(())
    }
    fn session_name(&self) -> String {
        "copilot".into()
    }
    fn set_session_name(&self, _: &str) {}
}

/// A bare lifecycle with recorded statuses and deaths (`_make_watchdog`).
struct Host {
    lifecycle: RefCell<SessionLifecycle>,
    statuses: RefCell<Vec<AgentState>>,
    died: Cell<usize>,
    in_flight: Cell<bool>,
}

impl Host {
    fn new(state: State, meeting_id: Option<&str>) -> Self {
        Host {
            lifecycle: RefCell::new(SessionLifecycle { state, meeting_id: meeting_id.map(str::to_owned) }),
            statuses: RefCell::default(),
            died: Cell::new(0),
            in_flight: Cell::new(false),
        }
    }

    fn statuses(&self) -> Vec<AgentState> {
        self.statuses.borrow().clone()
    }

    fn state(&self) -> State {
        self.lifecycle.borrow().state
    }
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

/// A clock the test advances (`FakeClock`).
fn fake_clock() -> (Rc<Cell<f64>>, Watchdog) {
    let now = Rc::new(Cell::new(0.0));
    let read = now.clone();
    (now, Watchdog::new(Box::new(move || read.get())))
}

fn keys(names: &[&str]) -> Vec<String> {
    names.iter().map(|k| (*k).to_owned()).collect()
}

#[tokio::test]
async fn dev_channels_dialog_on_two_ticks_gets_one_keypress_and_reports_spawning_not_blocked() {
    // given
    let runner = WatchdogRunner::new(true, DEV_CHANNELS_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(runner.keys(), vec![keys(&["1"])]);
    assert_eq!(host.statuses(), vec![AgentState::Spawning]);
}

#[tokio::test]
async fn dialog_reappearing_after_the_prompt_is_answered_again() {
    // given
    let runner = WatchdogRunner::new(true, DEV_CHANNELS_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap(); // dialog → "1"
    runner.show(REPL_PROMPT_SCREEN);
    watchdog.tick(&host, &runner).await.unwrap(); // prompt → clears the latch
    runner.show(DEV_CHANNELS_SCREEN);
    watchdog.tick(&host, &runner).await.unwrap(); // dialog again → "1" again
    // then
    assert_eq!(runner.keys(), vec![keys(&["1"]), keys(&["1"])]);
}

#[tokio::test]
async fn trust_dialog_on_two_ticks_moves_to_accept_and_confirms_once_reporting_spawning() {
    // given
    let runner = WatchdogRunner::new(true, TRUST_DIALOG_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(runner.keys(), vec![keys(&["Down", "Enter"])]);
    assert_eq!(host.statuses(), vec![AgentState::Spawning]);
}

#[tokio::test]
async fn numbered_trust_dialog_with_cursor_on_accept_is_confirmed_in_place() {
    // given
    let runner = WatchdogRunner::new(true, TRUST_DIALOG_NUMBERED_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(runner.keys(), vec![keys(&["Enter"])]);
}

#[tokio::test]
async fn dialog_still_up_after_the_retry_interval_is_answered_again() {
    // given
    let runner = WatchdogRunner::new(true, TRUST_DIALOG_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    clock.set(DISMISS_RETRY_S - 1.0);
    watchdog.tick(&host, &runner).await.unwrap(); // below the interval → no resend
    clock.set(DISMISS_RETRY_S);
    watchdog.tick(&host, &runner).await.unwrap(); // interval elapsed → resend
    // then
    assert_eq!(runner.keys(), vec![keys(&["Down", "Enter"]), keys(&["Down", "Enter"])]);
    assert!(!host.statuses().contains(&AgentState::Blocked));
}

#[tokio::test]
async fn trust_then_dev_channels_dialogs_back_to_back_each_get_their_own_keypress() {
    // given
    let runner = WatchdogRunner::new(true, TRUST_DIALOG_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    runner.show(DEV_CHANNELS_SCREEN);
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(runner.keys(), vec![keys(&["Down", "Enter"]), keys(&["1"])]);
    assert!(!host.statuses().contains(&AgentState::Blocked));
}

#[tokio::test]
async fn unknown_dialog_reports_blocked_and_is_not_answered() {
    // given
    let runner = WatchdogRunner::new(true, MODEL_DIALOG_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(host.statuses(), vec![AgentState::Blocked]);
    assert!(runner.keys().is_empty());
}

#[tokio::test]
async fn stable_prompt_reports_once_then_heartbeats_at_the_interval_and_again_on_change() {
    // given
    let runner = WatchdogRunner::new(true, REPL_PROMPT_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (clock, mut watchdog) = fake_clock();
    // when / then
    watchdog.tick(&host, &runner).await.unwrap(); // t=0: online, first report
    assert_eq!(host.statuses(), vec![AgentState::Online]);
    assert_eq!(host.state(), State::Running); // online confirms session_ready
    clock.set(2.0);
    watchdog.tick(&host, &runner).await.unwrap(); // below the heartbeat → silent
    assert_eq!(host.statuses(), vec![AgentState::Online]);
    clock.set(5.0);
    watchdog.tick(&host, &runner).await.unwrap(); // heartbeat repeats the status
    assert_eq!(host.statuses(), vec![AgentState::Online, AgentState::Online]);
    runner.show(SPINNER_SCREEN);
    clock.set(6.0);
    watchdog.tick(&host, &runner).await.unwrap(); // working: a change
    assert_eq!(host.statuses().last(), Some(&AgentState::Working));
}

#[tokio::test]
async fn vanished_session_reports_offline_and_feeds_session_died() {
    // given
    let runner = WatchdogRunner::new(false, "");
    let host = Host::new(State::Running, Some("A"));
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(host.statuses(), vec![AgentState::Offline]);
    assert_eq!(host.died.get(), 1);
}

#[tokio::test]
async fn offline_pane_while_a_spawn_is_in_flight_reports_spawning_and_is_not_a_death() {
    // given
    let runner = WatchdogRunner::new(false, "");
    let host = Host::new(State::Spawning, Some("A"));
    host.in_flight.set(true);
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(host.statuses(), vec![AgentState::Spawning]);
    assert_eq!(host.died.get(), 0);
    assert_eq!(host.state(), State::Spawning);
}

#[tokio::test]
async fn live_pane_during_a_queued_spawn_does_not_mark_the_replacement_ready_until_it_settles() {
    // given
    let runner = WatchdogRunner::new(true, REPL_PROMPT_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    host.in_flight.set(true);
    let (_clock, mut watchdog) = fake_clock();
    // when / then
    watchdog.tick(&host, &runner).await.unwrap();
    assert_eq!(host.statuses(), vec![AgentState::Spawning]);
    assert_eq!(host.state(), State::Spawning);
    assert_eq!(host.died.get(), 0);
    host.in_flight.set(false);
    watchdog.tick(&host, &runner).await.unwrap();
    assert_eq!(host.statuses().last(), Some(&AgentState::Online));
    assert_eq!(host.state(), State::Running);
}

#[tokio::test]
async fn offline_pane_after_the_spawn_settled_is_a_death() {
    // given
    let runner = WatchdogRunner::new(false, "");
    let host = Host::new(State::Running, Some("A"));
    let (_clock, mut watchdog) = fake_clock();
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(host.statuses(), vec![AgentState::Offline]);
    assert_eq!(host.died.get(), 1);
}

#[tokio::test]
async fn pane_spawning_past_the_deadline_reports_blocked_without_respawn() {
    // given
    let runner = WatchdogRunner::new(true, STARTUP_SCREEN);
    let host = Host::new(State::Spawning, Some("A"));
    let (clock, mut watchdog) = fake_clock();
    // when / then
    watchdog.tick(&host, &runner).await.unwrap();
    assert_eq!(host.statuses(), vec![AgentState::Spawning]);
    clock.set(SPAWN_DEADLINE_S);
    watchdog.tick(&host, &runner).await.unwrap();
    assert_eq!(host.statuses().last(), Some(&AgentState::Blocked));
    assert_eq!(host.state(), State::Spawning);
    assert_eq!(host.died.get(), 0);
}

#[tokio::test]
async fn idle_lifecycle_closes_the_chip_to_offline_once_and_never_probes_tmux() {
    // given
    let runner = WatchdogRunner::new(true, REPL_PROMPT_SCREEN);
    let host = Host::new(State::Idle, None);
    let (_clock, mut watchdog) = fake_clock();
    watchdog.last_reported = Some(AgentState::Online); // a prior live report to close out
    // when
    watchdog.tick(&host, &runner).await.unwrap();
    watchdog.tick(&host, &runner).await.unwrap();
    // then
    assert_eq!(host.statuses(), vec![AgentState::Offline]);
    assert!(runner.keys().is_empty());
}
