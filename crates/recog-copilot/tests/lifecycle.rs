//! `SessionLifecycle`, the pure state machine (tests/test_copilot_supervisor.py,
//! "SessionLifecycle: pure state machine, table-driven").
#![forbid(unsafe_code)]

use recog_copilot::supervisor::{Command, SessionLifecycle, State, reconcile};

fn spawn(id: &str) -> Command {
    Command::Spawn(Some(id.to_owned()))
}

fn machine(state: State, meeting_id: Option<&str>) -> SessionLifecycle {
    SessionLifecycle { state, meeting_id: meeting_id.map(str::to_owned) }
}

type Event = fn(&mut SessionLifecycle, Option<&str>) -> Vec<Command>;
type Id = Option<&'static str>;
/// (start state, start id, input, argument, end state, end id, commands)
type Row = (State, Id, Event, Id, State, Id, Vec<Command>);

fn started(m: &mut SessionLifecycle, id: Option<&str>) -> Vec<Command> {
    m.meeting_started(id.expect("an id"))
}
fn ended(m: &mut SessionLifecycle, id: Option<&str>) -> Vec<Command> {
    m.meeting_ended(id)
}
fn expired(m: &mut SessionLifecycle, _: Option<&str>) -> Vec<Command> {
    m.timer_expired()
}
fn died(m: &mut SessionLifecycle, _: Option<&str>) -> Vec<Command> {
    m.session_died()
}
fn ready(m: &mut SessionLifecycle, _: Option<&str>) -> Vec<Command> {
    m.session_ready()
}
fn stop(m: &mut SessionLifecycle, id: Option<&str>) -> Vec<Command> {
    m.stop_now(id)
}

#[test]
fn every_state_and_input_of_the_table_gives_the_python_transition_and_commands() {
    use State::*;
    // given
    let table: Vec<Row> = vec![
        // IDLE + meeting_started → SPAWNING + spawn
        (Idle, None, started, Some("A"), Spawning, Some("A"), vec![spawn("A")]),
        // flap with the same id: GRACE → RUNNING, no kill
        (Grace, Some("A"), started, Some("A"), Running, Some("A"), vec![]),
        // another meeting from GRACE: kill + spawn
        (Grace, Some("A"), started, Some("B"), Spawning, Some("B"), vec![Command::Kill, spawn("B")]),
        // back-to-back meetings from RUNNING: kill + spawn
        (Running, Some("A"), started, Some("B"), Spawning, Some("B"), vec![Command::Kill, spawn("B")]),
        // duplicate meeting_started while RUNNING or SPAWNING: no-op
        (Running, Some("A"), started, Some("A"), Running, Some("A"), vec![]),
        (Spawning, Some("A"), started, Some("A"), Spawning, Some("A"), vec![]),
        // another meeting while still SPAWNING: kill + spawn
        (Spawning, Some("A"), started, Some("B"), Spawning, Some("B"), vec![Command::Kill, spawn("B")]),
        // an adopted session (unknown id) revived from GRACE by any id
        (Grace, None, started, Some("A"), Running, Some("A"), vec![]),
        // meeting_ended from RUNNING or SPAWNING → GRACE, the timer decides
        (Running, Some("A"), ended, Some("A"), Grace, Some("A"), vec![]),
        (Spawning, Some("A"), ended, Some("A"), Grace, Some("A"), vec![]),
        // meeting_ended for another id is stale
        (Running, Some("A"), ended, Some("B"), Running, Some("A"), vec![]),
        (Idle, None, ended, Some("A"), Idle, None, vec![]),
        // grace expiry → kill + IDLE; a stale timer is a no-op
        (Grace, Some("A"), expired, None, Idle, None, vec![Command::Kill]),
        (Running, Some("A"), expired, None, Running, Some("A"), vec![]),
        (Idle, None, expired, None, Idle, None, vec![]),
        // session died mid-meeting or while spawning → respawn the same meeting
        (Running, Some("A"), died, None, Spawning, Some("A"), vec![spawn("A")]),
        (Spawning, Some("A"), died, None, Spawning, Some("A"), vec![spawn("A")]),
        // session died during grace → IDLE, nothing to kill
        (Grace, Some("A"), died, None, Idle, None, vec![]),
        (Idle, None, died, None, Idle, None, vec![]),
        // the watchdog confirms the session: SPAWNING → RUNNING, else a no-op
        (Spawning, Some("A"), ready, None, Running, Some("A"), vec![]),
        (Running, Some("A"), ready, None, Running, Some("A"), vec![]),
        // explicit stop: immediate kill from RUNNING, SPAWNING and GRACE
        (Running, Some("A"), stop, Some("A"), Idle, None, vec![Command::Kill]),
        (Spawning, Some("A"), stop, Some("A"), Idle, None, vec![Command::Kill]),
        (Grace, Some("A"), stop, Some("A"), Idle, None, vec![Command::Kill]),
        // an adopted session matches any stop, as meeting_ended
        (Running, None, stop, Some("A"), Idle, None, vec![Command::Kill]),
        // a stop for another id is stale; a stop in IDLE is a no-op
        (Running, Some("A"), stop, Some("B"), Running, Some("A"), vec![]),
        (Idle, None, stop, Some("A"), Idle, None, vec![]),
    ];
    for (i, (start, start_id, event, arg, end, end_id, commands)) in table.into_iter().enumerate() {
        let mut m = machine(start, start_id);
        // when
        let result = event(&mut m, arg);
        // then
        assert_eq!(result, commands, "row {i}");
        assert_eq!(m.state, end, "row {i}");
        assert_eq!(m.meeting_id.as_deref(), end_id, "row {i}");
    }
}

#[test]
fn a_fresh_machine_through_a_whole_meeting_gives_spawn_then_kill_and_returns_to_idle() {
    // given
    let mut m = SessionLifecycle::new();
    // when / then
    assert_eq!(m.meeting_started("A"), vec![spawn("A")]);
    assert_eq!(m.session_ready(), vec![]);
    assert_eq!(m.state, State::Running);
    assert_eq!(m.meeting_ended(Some("A")), vec![]);
    assert_eq!(m.state, State::Grace);
    assert_eq!(m.timer_expired(), vec![Command::Kill]);
    assert_eq!(m.state, State::Idle);
}

#[test]
fn a_running_session_ending_and_restarting_within_grace_issues_no_commands_and_keeps_running() {
    // given
    let mut m = SessionLifecycle::new();
    m.meeting_started("A");
    m.session_ready();
    // when / then
    assert_eq!(m.meeting_ended(Some("A")), vec![]);
    assert_eq!(m.meeting_started("A"), vec![]);
    assert_eq!(m.state, State::Running);
    assert_eq!(m.meeting_id.as_deref(), Some("A"));
}

#[test]
fn adopt_attaches_without_commands_and_runs_the_meeting() {
    // given
    let mut m = SessionLifecycle::new();
    // when / then
    assert_eq!(m.adopt(Some("A")), vec![]);
    assert_eq!(m.state, State::Running);
    assert_eq!(m.meeting_id.as_deref(), Some("A"));
}

#[test]
fn reconcile_without_session_and_with_active_meeting_spawns() {
    // given
    let mut m = SessionLifecycle::new();
    // when / then
    assert_eq!(reconcile(&mut m, true, Some("A"), false), vec![spawn("A")]);
    assert_eq!(m.state, State::Spawning);
}

#[test]
fn reconcile_with_live_session_and_active_meeting_adopts_without_commands() {
    // given
    let mut m = SessionLifecycle::new();
    // when / then
    assert_eq!(reconcile(&mut m, true, Some("A"), true), vec![]);
    assert_eq!((m.state, m.meeting_id.as_deref()), (State::Running, Some("A")));
}

#[test]
fn reconcile_with_live_session_and_no_meeting_goes_to_grace_without_kill() {
    // given
    let mut idle = SessionLifecycle::new();
    let mut running = machine(State::Running, Some("A"));
    // when / then: an orphan tmux session is adopted into GRACE, a tracked one too
    assert_eq!(reconcile(&mut idle, false, None, true), vec![]);
    assert_eq!((idle.state, idle.meeting_id.as_deref()), (State::Grace, None));
    assert_eq!(reconcile(&mut running, false, None, false), vec![]);
    assert_eq!(running.state, State::Grace);
}

#[test]
fn reconcile_idle_without_session_or_meeting_is_a_no_op() {
    // given
    let mut m = SessionLifecycle::new();
    // when / then
    assert_eq!(reconcile(&mut m, false, None, false), vec![]);
    assert_eq!(m.state, State::Idle);
}

#[test]
fn commands_print_as_the_python_dataclass_repr() {
    // given / when / then: the error log lines carry this form
    assert_eq!(Command::Kill.to_string(), "Kill()");
    assert_eq!(spawn("A").to_string(), "Spawn(meeting_id='A')");
    assert_eq!(Command::Spawn(None).to_string(), "Spawn(meeting_id=None)");
}
