//! The supervisor glue without a bridge: grace timer, autospawn, reconciliation, manual
//! sessions, respawn backoff, the execute lock and the Copilot switch
//! (tests/test_copilot_supervisor.py and tests/test_copilot_toggle.py).
//!
//! On tokio's paused clock: the Python tests' sleeps (0.05 s grace, 0.15 s waits) keep
//! their values and run instantly, in the same order.
#![forbid(unsafe_code)]

mod common;

use std::rc::Rc;
use std::time::Duration;

use common::*;
use recog_copilot::log;
use recog_copilot::supervisor::{
    AgentState, Command, MANUAL_MEETING_ID, State, Supervisor, TMUX_SESSION, TMUX_SESSION_MANUAL, Tunables, Watchdog,
};
use tokio::task::spawn_local;
use tokio_tungstenite::tungstenite::Message;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

async fn send(sup: &FakeSupervisor, text: &str) {
    sup.handle_frame(&frame(text)).await;
}

const STARTED_A: &str = r#"{"type": "meeting_started", "meeting_id": "A"}"#;
const ENDED_A: &str = r#"{"type": "meeting_ended", "meeting_id": "A"}"#;
const STARTED_B: &str = r#"{"type": "meeting_started", "meeting_id": "B"}"#;
const SPAWN_R1: &str = r#"{"type": "copilot_spawn_request", "request_id": "r1"}"#;
const SPAWN_R2: &str = r#"{"type": "copilot_spawn_request", "request_id": "r2"}"#;
const STOP_R2: &str = r#"{"type": "copilot_stop_request", "request_id": "r2"}"#;

fn manual(n: usize) -> Vec<Option<String>> {
    vec![Some(MANUAL_MEETING_ID.to_owned()); n]
}

// --- grace timer, autospawn switch ---

#[tokio::test(start_paused = true)]
async fn running_session_whose_meeting_ended_is_killed_when_grace_expires() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(0.05, true);
        send(&sup, STARTED_A).await;
        assert_eq!(runner.spawned(), ids(&["A"]));
        // when
        send(&sup, ENDED_A).await;
        assert_eq!(sup.state(), State::Grace);
        assert_eq!(runner.kills.get(), 0);
        tokio::time::sleep(ms(150)).await;
        // then
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn same_meeting_restarting_within_grace_cancels_the_kill() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(0.1, true);
        send(&sup, STARTED_A).await;
        send(&sup, ENDED_A).await;
        // when
        send(&sup, STARTED_A).await;
        assert_eq!(sup.state(), State::Running);
        tokio::time::sleep(ms(250)).await;
        // then
        assert_eq!(runner.kills.get(), 0);
        assert_eq!(runner.spawned(), ids(&["A"]));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn another_meeting_during_grace_kills_and_spawns_and_cancels_the_timer() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        send(&sup, STARTED_A).await;
        send(&sup, ENDED_A).await;
        // when
        send(&sup, STARTED_B).await;
        // then
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(runner.spawned(), ids(&["A", "B"]));
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Spawning, Some("B")));
        assert!(!sup.grace_pending());
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn stale_grace_kill_parked_on_the_lock_is_skipped_when_a_spawn_claims_the_live_session() {
    local(async {
        // given: an adopted session in GRACE whose timer expires while the lock is held
        let sup = tmux_supervisor(FakeTmux::named(&[TMUX_SESSION]), 0.05, true);
        sup.with_lifecycle(|l| l.adopt(Some("A")));
        sup.handle_frame(&frame(ENDED_A)).await;
        assert_eq!(sup.state(), State::Grace);
        let guard = sup.execute_lock().lock().await;
        assert!(wait_for(|| sup.state() == State::Idle).await);
        // when: the frame takes the IDLE branch and queues its spawn behind the stale kill
        let frame_task = {
            let sup = sup.clone();
            spawn_local(async move { sup.handle_frame(&frame(STARTED_B)).await })
        };
        assert!(wait_for(|| sup.state() == State::Spawning).await);
        drop(guard);
        frame_task.await.unwrap();
        settle().await;
        // then: no kill, and B adopted the surviving session
        let fake = sup.runner.command_runner();
        assert!(fake.kill_targets().is_empty());
        assert!(!fake.subcommands().iter().any(|s| s == "new-session"));
        assert!(fake.live().contains(TMUX_SESSION));
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Spawning, Some("B")));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn stale_grace_kill_is_skipped_when_the_claimed_session_reached_running() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(0.05, true);
        send(&sup, STARTED_A).await;
        sup.with_lifecycle(|l| l.session_ready());
        send(&sup, ENDED_A).await;
        let guard = sup.execute_lock().lock().await;
        assert!(wait_for(|| sup.state() == State::Idle).await);
        let frame_task = {
            let sup = sup.clone();
            spawn_local(async move { sup.handle_frame(&frame(STARTED_B)).await })
        };
        assert!(wait_for(|| sup.state() == State::Spawning).await);
        // when: the watchdog confirms the claimed session before the kill runs
        sup.with_lifecycle(|l| l.session_ready());
        assert_eq!(sup.state(), State::Running);
        drop(guard);
        frame_task.await.unwrap();
        settle().await;
        // then
        assert_eq!(runner.kills.get(), 0);
        assert_eq!(runner.spawned(), ids(&["A", "B"]));
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Running, Some("B")));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn stale_grace_kill_is_skipped_when_the_claim_went_back_to_grace() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(0.05, true);
        send(&sup, STARTED_A).await;
        sup.with_lifecycle(|l| l.session_ready());
        send(&sup, ENDED_A).await;
        let guard = sup.execute_lock().lock().await;
        assert!(wait_for(|| sup.state() == State::Idle).await);
        let frame_task = {
            let sup = sup.clone();
            spawn_local(async move { sup.handle_frame(&frame(STARTED_B)).await })
        };
        assert!(wait_for(|| sup.state() == State::Spawning).await);
        // when: B ends before its queued spawn runs; its own grace timer is cancelled
        let end_task = {
            let sup = sup.clone();
            spawn_local(
                async move { sup.handle_frame(&frame(r#"{"type": "meeting_ended", "meeting_id": "B"}"#)).await },
            )
        };
        assert!(wait_for(|| sup.state() == State::Grace).await);
        sup.stop();
        drop(guard);
        frame_task.await.unwrap();
        end_task.await.unwrap();
        settle().await;
        // then: only IDLE permits the queued kill
        assert_eq!(runner.kills.get(), 0);
        assert_eq!(runner.spawned(), ids(&["A", "B"]));
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Grace, Some("B")));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn spawn_arriving_during_an_in_flight_grace_kill_waits_for_it_and_creates_a_fresh_session() {
    local(async {
        // given: the grace kill holds the lock mid-kill
        let fake = FakeTmux::with_session(true);
        let gate = Rc::new(Gate::default());
        *fake.kill_gate.borrow_mut() = Some(gate.clone());
        let sup = tmux_supervisor(fake, 0.05, true);
        sup.with_lifecycle(|l| l.adopt(Some("A")));
        sup.handle_frame(&frame(ENDED_A)).await;
        gate.wait_started().await;
        // when
        let frame_task = {
            let sup = sup.clone();
            spawn_local(async move { sup.handle_frame(&frame(STARTED_B)).await })
        };
        assert!(wait_for(|| sup.state() == State::Spawning && sup.spawn_in_flight()).await);
        Watchdog::new(Box::new(|| 0.0)).tick(&*sup, &*sup.runner).await.unwrap();
        assert_eq!(sup.respawn_attempts(), 0);
        assert!(!sup.respawn_pending());
        assert!(!sup.runner.command_runner().subcommands().iter().any(|s| s == "new-session"));
        gate.open();
        frame_task.await.unwrap();
        settle().await;
        // then
        let fake = sup.runner.command_runner();
        assert_eq!(fake.subcommands().iter().filter(|s| *s == "kill-session").count(), 1);
        assert_eq!(fake.subcommands().iter().filter(|s| *s == "new-session").count(), 1);
        assert!(fake.has_session.get());
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Spawning, Some("B")));
        assert!(!sup.spawn_in_flight());
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn autospawn_off_makes_meeting_frames_inert() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(0.05, false);
        // when
        send(&sup, STARTED_A).await;
        send(&sup, r#"{"type": "meeting_state", "active": true, "meeting_id": "A"}"#).await;
        send(&sup, ENDED_A).await;
        // then
        assert!(runner.spawned().is_empty());
        assert_eq!(runner.kills.get(), 0);
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn meeting_without_tmux_or_a_copilot_cli_starts_no_session_and_says_what_is_missing() {
    local(async {
        // given: autospawn on, tmux not installed
        let (sup, runner) = fake_supervisor(0.05, true);
        runner.missing.set(Some("tmux"));
        let capture = log::Capture::start();
        // when
        send(&sup, STARTED_A).await;
        send(&sup, r#"{"type": "meeting_state", "active": true, "meeting_id": "A"}"#).await;
        send(&sup, ENDED_A).await;
        // then: no session the watchdog would probe for the whole meeting
        assert!(runner.spawned().is_empty());
        assert_eq!(sup.state(), State::Idle);
        assert!(capture.text().contains("tmux is not installed: no Copilot for meeting 'A'"), "{}", capture.text());
        // when: the tool is installed before the next meeting
        runner.missing.set(None);
        send(&sup, STARTED_B).await;
        // then
        assert_eq!(runner.spawned(), ids(&["B"]));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn autospawn_off_still_runs_manual_spawn_and_stop() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(0.05, false);
        // when
        send(&sup, SPAWN_R1).await;
        assert_eq!(runner.spawned(), manual(1));
        send(&sup, STOP_R2).await;
        // then
        assert_eq!(runner.spawned(), manual(1));
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

// --- reconciliation via meeting_state ---

#[tokio::test(start_paused = true)]
async fn bridge_restart_reporting_no_meeting_moves_a_running_session_to_grace_and_the_rejoin_revives_it() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        sup.with_lifecycle(|l| l.adopt(Some("A")));
        // when
        send(&sup, r#"{"type": "meeting_state", "active": false, "meeting_id": null}"#).await;
        assert_eq!(sup.state(), State::Grace);
        assert_eq!(runner.kills.get(), 0);
        send(&sup, STARTED_A).await;
        // then
        assert_eq!(sup.state(), State::Running);
        assert_eq!(runner.kills.get(), 0);
        assert!(runner.spawned().is_empty());
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn active_meeting_without_a_session_spawns_for_it() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        // when
        send(&sup, r#"{"type": "meeting_state", "active": true, "meeting_id": "A"}"#).await;
        // then
        assert_eq!(runner.spawned(), ids(&["A"]));
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Spawning, Some("A")));
    })
    .await;
}

fn probed_supervisor(alive: bool, grace_s: f64) -> (FakeSupervisor, Rc<FakeRunner>) {
    let runner = FakeRunner::new();
    let sup = Supervisor::with_alive_probe(
        "ws://127.0.0.1:1",
        runner.clone(),
        TestSettings::enabled(),
        true,
        Tunables::new(grace_s),
        Box::new(move || alive),
    );
    (sup, runner)
}

#[tokio::test(start_paused = true)]
async fn live_session_after_a_supervisor_restart_mid_meeting_is_adopted_without_spawn_or_kill() {
    local(async {
        // given
        let (sup, runner) = probed_supervisor(true, 10.0);
        // when
        send(&sup, r#"{"type": "meeting_state", "active": true, "meeting_id": "A"}"#).await;
        // then
        assert!(runner.spawned().is_empty());
        assert_eq!(runner.kills.get(), 0);
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Running, Some("A")));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn orphan_session_without_a_meeting_is_adopted_into_grace_and_killed_on_expiry() {
    local(async {
        // given
        let (sup, runner) = probed_supervisor(true, 0.05);
        // when
        send(&sup, r#"{"type": "meeting_state", "active": false, "meeting_id": null}"#).await;
        assert_eq!(sup.state(), State::Grace);
        assert_eq!(runner.kills.get(), 0);
        tokio::time::sleep(ms(150)).await;
        // then
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn idle_supervisor_without_session_or_meeting_ignores_the_state() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        // when
        send(&sup, r#"{"type": "meeting_state", "active": false, "meeting_id": null}"#).await;
        // then
        assert!(runner.spawned().is_empty());
        assert_eq!(runner.kills.get(), 0);
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn reconciliation_without_an_explicit_probe_asks_the_runner_and_adopts_the_live_session() {
    local(async {
        // given
        let sup = tmux_supervisor(FakeTmux::with_session(true), 10.0, true);
        // when
        sup.handle_frame(&frame(r#"{"type": "meeting_state", "active": true, "meeting_id": "A"}"#)).await;
        // then
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Running, Some("A")));
        assert!(!sup.runner.command_runner().subcommands().iter().any(|s| s == "new-session"));
    })
    .await;
}

// --- manual copilot lifecycle ---

#[tokio::test(start_paused = true)]
async fn manual_spawn_request_spawns_under_the_manual_id_with_autospawn_on_or_off() {
    for autospawn in [true, false] {
        local(async {
            // given
            let (sup, runner) = fake_supervisor(10.0, autospawn);
            // when
            send(&sup, SPAWN_R1).await;
            // then
            assert_eq!(runner.spawned(), manual(1));
            assert_eq!(runner.kills.get(), 0);
            assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Spawning, Some(MANUAL_MEETING_ID)));
            sup.stop();
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn second_manual_spawn_request_is_the_same_id_flap_and_does_nothing() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, false);
        send(&sup, SPAWN_R1).await;
        sup.with_lifecycle(|l| l.session_ready());
        // when
        send(&sup, SPAWN_R2).await;
        // then
        assert_eq!(runner.spawned(), manual(1));
        assert_eq!(runner.kills.get(), 0);
        assert_eq!(sup.state(), State::Running);
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_spawn_after_a_manual_stop_starts_a_fresh_session() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, false);
        send(&sup, SPAWN_R1).await;
        sup.with_lifecycle(|l| l.session_ready());
        send(&sup, STOP_R2).await;
        assert_eq!(sup.state(), State::Idle);
        assert_eq!(runner.kills.get(), 1);
        // when
        send(&sup, r#"{"type": "copilot_spawn_request", "request_id": "r3"}"#).await;
        // then
        assert_eq!(sup.state(), State::Spawning);
        assert_eq!(runner.spawned(), manual(2));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_stop_request_kills_at_once_without_a_grace_timer_and_logs_it() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, false);
        send(&sup, SPAWN_R1).await;
        sup.with_lifecycle(|l| l.session_ready());
        let capture = log::Capture::start();
        // when
        send(&sup, STOP_R2).await;
        // then
        assert_eq!(sup.state(), State::Idle);
        assert!(!sup.grace_pending());
        assert_eq!(runner.spawned(), manual(1));
        assert_eq!(runner.kills.get(), 1);
        assert!(capture.text().contains("manual stop request 'r2' — killing session now"), "{}", capture.text());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn conference_meeting_preempts_a_live_manual_session() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        send(&sup, SPAWN_R1).await;
        sup.with_lifecycle(|l| l.session_ready());
        // when
        send(&sup, STARTED_B).await;
        // then
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(runner.spawned(), vec![Some(MANUAL_MEETING_ID.to_owned()), Some("B".to_owned())]);
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Spawning, Some("B")));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn live_manual_session_reported_by_the_bridge_after_a_restart_is_adopted() {
    local(async {
        // given
        let (sup, runner) = probed_supervisor(true, 10.0);
        // when
        send(&sup, r#"{"type": "meeting_state", "active": true, "meeting_id": "manual"}"#).await;
        // then
        assert!(runner.spawned().is_empty());
        assert_eq!(runner.kills.get(), 0);
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Running, Some(MANUAL_MEETING_ID)));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_session_crashing_with_autospawn_off_respawns_on_the_three_attempt_budget() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, false);
        sup.tunables().respawn_backoff_s = vec![0.01, 0.01, 0.01];
        send(&sup, SPAWN_R1).await;
        sup.with_lifecycle(|l| l.session_ready());
        // when
        for _ in 0..6 {
            sup.on_session_died();
            tokio::time::sleep(ms(50)).await;
        }
        // then: the initial spawn and three respawns
        assert_eq!(runner.spawned(), manual(4));
        assert_eq!(sup.state(), State::Spawning);
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_spawn_request_after_an_exhausted_budget_resets_it_so_the_next_death_respawns() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, false);
        sup.tunables().respawn_backoff_s = vec![0.01, 0.01, 0.01];
        send(&sup, SPAWN_R1).await;
        for _ in 0..6 {
            sup.on_session_died();
            tokio::time::sleep(ms(50)).await;
        }
        assert_eq!(runner.spawned(), manual(4));
        // when
        send(&sup, SPAWN_R2).await;
        assert_eq!(runner.spawned(), manual(4)); // the flap itself spawns nothing
        assert_eq!(sup.respawn_attempts(), 0);
        sup.on_session_died();
        tokio::time::sleep(ms(50)).await;
        // then
        assert_eq!(runner.spawned(), manual(5));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_spawn_request_keeps_a_pending_respawn_and_its_spent_budget() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, false);
        sup.tunables().respawn_backoff_s = vec![0.1, 0.1, 0.1];
        send(&sup, SPAWN_R1).await;
        sup.with_lifecycle(|l| l.session_ready());
        sup.on_session_died();
        let pending = sup.respawn_task_id();
        assert!(pending.is_some());
        assert_eq!(sup.respawn_attempts(), 1);
        // when
        send(&sup, SPAWN_R2).await;
        // then
        assert_eq!(sup.respawn_task_id(), pending);
        assert_eq!(sup.respawn_attempts(), 1);
        tokio::time::sleep(ms(150)).await;
        assert_eq!(runner.spawned(), manual(2));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn stray_manual_stop_during_a_conference_session_is_inert() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(0.05, true);
        send(&sup, r#"{"type": "meeting_started", "meeting_id": "B"}"#).await;
        sup.with_lifecycle(|l| l.session_ready());
        // when
        send(&sup, r#"{"type": "copilot_stop_request", "request_id": "r1"}"#).await;
        // then
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Running, Some("B")));
        assert_eq!(runner.kills.get(), 0);
        assert!(!sup.grace_pending());
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn malformed_frames_are_ignored() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, false);
        // when
        send(&sup, r#"["copilot_spawn_request"]"#).await;
        send(&sup, r#"{"type": "copilot_unknown_request", "request_id": "r1"}"#).await;
        send(&sup, "{}").await;
        // then
        assert!(runner.spawned().is_empty());
        assert_eq!(runner.kills.get(), 0);
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

#[test]
fn hello_reports_an_id_only_for_a_session_meant_to_live() {
    // given: the lifecycle parked in each state
    let cases = [
        (State::Idle, None, None),
        (State::Spawning, Some("manual"), Some("manual")),
        (State::Running, Some("manual"), Some("manual")),
        (State::Running, Some("mtg-1"), Some("mtg-1")),
        // an adopted session of unknown provenance is never claimed manual
        (State::Running, None, None),
        (State::Grace, Some("manual"), None),
    ];
    for (state, meeting_id, expected) in cases {
        let (sup, _runner) = fake_supervisor(90.0, true);
        sup.with_lifecycle(|l| {
            l.state = state;
            l.meeting_id = meeting_id.map(str::to_owned);
        });
        // when / then
        assert_eq!(sup.live_meeting_id().as_deref(), expected, "{state:?}");
    }
}

// --- startup manual re-adoption (DW-9) ---

#[tokio::test(start_paused = true)]
async fn failing_startup_probe_is_logged_and_adopts_nothing() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(90.0, true);
        runner.fail_alive.set(true);
        let capture = log::Capture::start();
        // when
        sup.startup_manual_readoption().await;
        // then
        assert_eq!(sup.state(), State::Idle);
        assert!(capture.text().contains("startup probe failed"), "{}", capture.text());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn both_sessions_alive_at_startup_with_a_failing_kill_is_logged_and_startup_continues_idle() {
    local(async {
        // given
        let fake = FakeTmux::named(&[TMUX_SESSION, TMUX_SESSION_MANUAL]);
        fake.respond("kill-session", 1, "boom");
        let sup = tmux_supervisor(fake, 90.0, true);
        let capture = log::Capture::start();
        // when
        sup.startup_manual_readoption().await;
        // then
        assert_eq!(sup.state(), State::Idle);
        assert_eq!(recog_copilot::supervisor::Runner::session_name(&*sup.runner), TMUX_SESSION);
        assert!(capture.text().contains("stale manual session kill failed"), "{}", capture.text());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn conference_preempting_a_manual_session_kills_the_manual_name_and_spawns_the_default_one() {
    local(async {
        // given
        let sup = tmux_supervisor(FakeTmux::named(&[]), 10.0, true);
        sup.handle_frame(&frame(SPAWN_R1)).await;
        let fake = sup.runner.command_runner();
        assert_eq!(recog_copilot::supervisor::Runner::session_name(&*sup.runner), TMUX_SESSION_MANUAL);
        assert_eq!(fake.live(), [TMUX_SESSION_MANUAL.to_owned()].into());
        sup.with_lifecycle(|l| l.session_ready());
        // when
        sup.handle_frame(&frame(STARTED_B)).await;
        // then
        assert_eq!(fake.kill_targets(), [TMUX_SESSION_MANUAL]);
        assert_eq!(fake.new_session_names(), [TMUX_SESSION_MANUAL, TMUX_SESSION]);
        assert_eq!(recog_copilot::supervisor::Runner::session_name(&*sup.runner), TMUX_SESSION);
        assert_eq!(fake.live(), [TMUX_SESSION.to_owned()].into());
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Spawning, Some("B")));
        sup.stop();
    })
    .await;
}

// --- execute: failures, the in-flight flag, the lock ---

#[tokio::test(start_paused = true)]
async fn kill_failing_after_its_retries_is_logged_per_command_without_crashing() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(90.0, true);
        runner.fail_kill.set(true);
        let capture = log::Capture::start();
        // when
        sup.execute(&[Command::Kill]).await;
        // then
        let text = capture.text();
        assert!(text.contains("command Kill() failed"), "{text}");
        assert!(text.contains("rc=1") && text.contains("boom"), "{text}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn grace_expiry_whose_tmux_kill_runs_out_of_retries_logs_it_and_stays_idle() {
    local(async {
        // given
        let fake = FakeTmux::with_session(true);
        *fake.kill_results.borrow_mut() = vec![(1, "still alive".to_owned()); 5];
        let sup = tmux_supervisor(fake, 0.0, true);
        sup.with_lifecycle(|l| l.adopt(Some("A")));
        let capture = log::Capture::start();
        // when
        sup.handle_frame(&frame(ENDED_A)).await;
        tokio::time::sleep(ms(10)).await;
        // then
        let text = capture.text();
        assert!(
            text.contains("command Kill() failed") && text.contains("rc=1") && text.contains("still alive"),
            "{text}"
        );
        let kills = sup.runner.command_runner().subcommands().iter().filter(|s| *s == "kill-session").count();
        assert_eq!(kills, 3);
        assert_eq!(sup.state(), State::Idle);
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn spawn_in_flight_is_set_while_a_spawn_runs_and_cleared_after() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(90.0, true);
        let gate = runner.gate("A");
        assert!(!sup.spawn_in_flight());
        // when
        let task = {
            let sup = sup.clone();
            spawn_local(async move { sup.execute(&[Command::Spawn(Some("A".into()))]).await })
        };
        gate.wait_started().await;
        // then
        assert!(sup.spawn_in_flight());
        gate.open();
        task.await.unwrap();
        assert!(!sup.spawn_in_flight());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn empty_command_list_returns_without_waiting_for_the_lock() {
    local(async {
        // given
        let (sup, _runner) = fake_supervisor(90.0, true);
        let _guard = sup.execute_lock().lock().await;
        // when / then
        tokio::time::timeout(ms(100), sup.execute(&[])).await.expect("no wait");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn spawn_in_flight_stays_set_until_every_queued_spawn_finished() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(90.0, true);
        let (first, second) = (runner.gate("A"), runner.gate("B"));
        let first_task = {
            let sup = sup.clone();
            spawn_local(async move { sup.execute(&[Command::Spawn(Some("A".into()))]).await })
        };
        first.wait_started().await;
        let second_task = {
            let sup = sup.clone();
            spawn_local(async move { sup.execute(&[Command::Spawn(Some("B".into()))]).await })
        };
        tokio::task::yield_now().await;
        assert!(sup.spawn_in_flight());
        // when / then
        first.open();
        second.wait_started().await;
        first_task.await.unwrap();
        assert!(sup.spawn_in_flight());
        second.open();
        second_task.await.unwrap();
        assert!(!sup.spawn_in_flight());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn agent_status_goes_out_on_the_connected_socket_with_the_current_meeting() {
    local(async {
        // given
        let (sup, _runner) = fake_supervisor(90.0, true);
        sup.with_lifecycle(|l| l.adopt(Some("mtg-9")));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        sup.set_ws(Some(tx));
        // when
        sup.send_agent_status(AgentState::Working);
        // then
        let Some(Message::Text(text)) = rx.recv().await else { panic!("a text frame") };
        assert_eq!(text.as_str(), r#"{"type": "copilot_agent_status", "status": "working", "meeting_id": "mtg-9"}"#);
        assert!(rx.try_recv().is_err());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn agent_status_while_disconnected_is_dropped_silently() {
    local(async {
        // given
        let (sup, _runner) = fake_supervisor(90.0, true);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        sup.set_ws(Some(tx));
        sup.set_ws(None);
        // when
        sup.send_agent_status(AgentState::Online);
        // then
        assert!(rx.try_recv().is_err());
    })
    .await;
}

// --- respawn with backoff ---

#[tokio::test(start_paused = true)]
async fn session_dying_more_often_than_the_budget_respawns_three_times_then_gives_up() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        sup.tunables().respawn_backoff_s = vec![0.01, 0.01, 0.01];
        send(&sup, STARTED_A).await;
        let capture = log::Capture::start();
        // when
        for _ in 0..6 {
            sup.on_session_died();
            tokio::time::sleep(ms(50)).await;
        }
        // then
        assert_eq!(runner.spawned(), ids(&["A", "A", "A", "A"]));
        assert_eq!(sup.state(), State::Spawning);
        assert_eq!(capture.text().matches("respawn attempts exhausted for meeting 'A'").count(), 1);
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_default_backoff_waits_5_then_15_then_30_seconds() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        send(&sup, STARTED_A).await;
        let mut waits = Vec::new();
        // when
        for _ in 0..3 {
            let start = tokio::time::Instant::now();
            let before = runner.spawned().len();
            sup.on_session_died();
            while runner.spawned().len() == before {
                tokio::time::sleep(ms(100)).await;
            }
            waits.push(start.elapsed().as_secs_f64().round());
        }
        // then
        assert_eq!(waits, [5.0, 15.0, 30.0]);
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn deaths_reported_while_a_respawn_is_pending_add_no_respawn() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        sup.tunables().respawn_backoff_s = vec![0.05, 0.05, 0.05];
        send(&sup, STARTED_A).await;
        // when
        sup.on_session_died();
        sup.on_session_died();
        sup.on_session_died();
        tokio::time::sleep(ms(150)).await;
        // then
        assert_eq!(runner.spawned(), ids(&["A", "A"]));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn new_meeting_cancels_the_pending_respawn_and_resets_the_budget() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        sup.tunables().respawn_backoff_s = vec![0.05, 0.05, 0.05];
        send(&sup, STARTED_A).await;
        sup.on_session_died();
        // when
        send(&sup, STARTED_B).await;
        tokio::time::sleep(ms(150)).await;
        // then
        assert_eq!(runner.spawned(), ids(&["A", "B"]));
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(sup.respawn_attempts(), 0);
        sup.on_session_died();
        tokio::time::sleep(ms(150)).await;
        assert_eq!(runner.spawned(), ids(&["A", "B", "B"]));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn meeting_ending_during_the_backoff_cancels_the_respawn_into_grace() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        sup.tunables().respawn_backoff_s = vec![0.05, 0.05, 0.05];
        send(&sup, STARTED_A).await;
        sup.on_session_died();
        // when
        send(&sup, ENDED_A).await;
        assert_eq!(sup.state(), State::Grace);
        tokio::time::sleep(ms(150)).await;
        // then
        assert_eq!(runner.spawned(), ids(&["A"]));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn death_reported_while_idle_spawns_nothing() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, false);
        // when
        sup.on_session_died();
        tokio::time::sleep(ms(20)).await;
        // then
        assert!(runner.spawned().is_empty());
        assert!(!sup.respawn_pending());
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

// --- MCP channel recovery ---

#[tokio::test(start_paused = true)]
async fn lost_mcp_channel_replaces_the_session_on_the_respawn_budget_and_stops_when_spent() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(90.0, false);
        sup.tunables().respawn_backoff_s = vec![0.01];
        sup.with_lifecycle(|l| l.adopt(Some("meeting-1")));
        let lost = r#"{"type": "copilot_channel_unavailable", "meeting_id": "meeting-1"}"#;
        // when
        send(&sup, lost).await;
        send(&sup, lost).await;
        tokio::time::sleep(ms(30)).await;
        // then
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(runner.spawned(), ids(&["meeting-1"]));
        sup.with_lifecycle(|l| l.session_ready());
        send(&sup, lost).await;
        assert_eq!(runner.kills.get(), 1); // a spent budget must not kill again
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn channel_loss_reported_for_an_older_meeting_leaves_the_current_session_alone() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(90.0, true);
        sup.with_lifecycle(|l| l.adopt(Some("new-meeting")));
        // when
        send(&sup, r#"{"type": "copilot_channel_unavailable", "meeting_id": "old-meeting"}"#).await;
        // then
        assert_eq!(runner.kills.get(), 0);
    })
    .await;
}

// --- the Copilot switch (test_copilot_toggle.py) ---

fn toggle_supervisor(autospawn: bool) -> (Rc<Supervisor<LiveRunner>>, Rc<LiveRunner>, Rc<TestSettings>) {
    let runner = Rc::new(LiveRunner::default());
    let settings = TestSettings::enabled();
    let sup = Supervisor::new("ws://127.0.0.1:1", runner.clone(), settings.clone(), autospawn, Tunables::new(90.0));
    (sup, runner, settings)
}

#[tokio::test(start_paused = true)]
async fn switching_copilot_off_stops_the_running_session_and_no_path_can_start_it_again() {
    for event in [r#"{"type": "meeting_started", "meeting_id": "meeting"}"#, r#"{"type": "copilot_spawn_request"}"#] {
        local(async {
            // given
            let (sup, runner, settings) = toggle_supervisor(true);
            sup.handle_frame(&frame(event)).await;
            assert!(runner.running.get());
            // when
            settings.set_enabled(false);
            sup.handle_frame(&frame(r#"{"type": "copilot_settings_changed"}"#)).await;
            // then
            assert_eq!(runner.kills.get(), 1);
            assert!(!runner.running.get());
            assert_eq!(sup.state(), State::Idle);
            sup.handle_frame(&frame(event)).await;
            sup.on_session_died();
            sup.execute(&[Command::Spawn(Some("queued-before-disable".into()))]).await;
            assert_eq!(runner.spawns.borrow().len(), 1);
            assert!(!sup.respawn_pending());
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn copilot_off_at_startup_reaps_a_session_left_running() {
    local(async {
        // given
        let (sup, runner, settings) = toggle_supervisor(false);
        settings.set_enabled(false);
        runner.running.set(true);
        // when
        let enabled = sup.refresh_enabled().await;
        // then
        assert!(!enabled);
        assert!(!runner.running.get());
        assert_eq!(runner.kills.get(), 1);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn switching_copilot_on_during_a_conference_starts_its_session() {
    local(async {
        // given
        let (sup, runner, settings) = toggle_supervisor(true);
        settings.set_enabled(false);
        sup.handle_frame(&frame(r#"{"type": "meeting_started", "meeting_id": "current"}"#)).await;
        assert!(runner.spawns.borrow().is_empty());
        // when
        settings.set_enabled(true);
        sup.refresh_enabled().await;
        // then
        assert_eq!(*runner.spawns.borrow(), ids(&["current"]));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn switching_copilot_off_cancels_a_pending_retry() {
    local(async {
        // given
        let (sup, _runner, settings) = toggle_supervisor(true);
        sup.handle_frame(&frame(r#"{"type": "meeting_started", "meeting_id": "meeting"}"#)).await;
        sup.on_session_died();
        assert!(sup.respawn_pending());
        // when
        settings.set_enabled(false);
        sup.refresh_enabled().await;
        // then
        assert!(sup.respawn_task_id().is_none());
        assert!(!sup.respawn_pending());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn unreadable_settings_count_as_copilot_off_and_are_logged_once() {
    local(async {
        // given
        let (sup, runner, settings) = toggle_supervisor(true);
        *settings.0.borrow_mut() = Err("Invalid AI settings JSON".into());
        let capture = log::Capture::start();
        // when
        sup.handle_frame(&frame(r#"{"type": "meeting_started", "meeting_id": "A"}"#)).await;
        sup.handle_frame(&frame(r#"{"type": "meeting_started", "meeting_id": "A"}"#)).await;
        // then
        assert!(runner.spawns.borrow().is_empty());
        assert!(!sup.is_enabled());
        assert_eq!(capture.text().matches("cannot read Copilot switch: Invalid AI settings JSON").count(), 1);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn non_string_meeting_ids_are_used_as_python_str_and_never_match_an_end() {
    local(async {
        // given
        let (sup, runner) = fake_supervisor(10.0, true);
        // when: a numeric id is truthy, spawned as str(); its end never equals the string
        send(&sup, r#"{"type": "meeting_started", "meeting_id": 5}"#).await;
        send(&sup, r#"{"type": "meeting_ended", "meeting_id": 5}"#).await;
        send(&sup, r#"{"type": "meeting_started", "meeting_id": 0}"#).await;
        // then
        assert_eq!(runner.spawned(), ids(&["5"]));
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Spawning, Some("5")));
        sup.stop();
    })
    .await;
}

// --- the AI functions level below `automatic` ---

/// A supervisor at the `manual` level; `alive` answers the session probe when given.
fn manual_level_supervisor(grace_s: f64, alive: Option<bool>) -> (FakeSupervisor, Rc<FakeRunner>, Rc<TestSettings>) {
    let runner = FakeRunner::new();
    let settings = TestSettings::enabled();
    settings.set_automatic(false);
    let sup = match alive {
        Some(alive) => Supervisor::with_alive_probe(
            "ws://127.0.0.1:1",
            runner.clone(),
            settings.clone(),
            true,
            Tunables::new(grace_s),
            Box::new(move || alive),
        ),
        None => Supervisor::new("ws://127.0.0.1:1", runner.clone(), settings.clone(), true, Tunables::new(grace_s)),
    };
    (sup, runner, settings)
}

#[tokio::test(start_paused = true)]
async fn manual_level_when_meeting_started_then_no_spawn() {
    local(async {
        let (sup, runner, _settings) = manual_level_supervisor(10.0, None);
        send(&sup, STARTED_A).await;
        assert!(runner.spawned().is_empty());
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_level_when_meeting_state_active_without_session_then_no_spawn() {
    local(async {
        let (sup, runner, _settings) = manual_level_supervisor(10.0, Some(false));
        send(&sup, r#"{"type": "meeting_state", "active": true, "meeting_id": "A"}"#).await;
        assert!(runner.spawned().is_empty());
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_level_and_live_conference_session_when_meeting_ended_then_grace_then_kill() {
    local(async {
        let (sup, runner, _settings) = manual_level_supervisor(0.05, None);
        sup.with_lifecycle(|l| l.adopt(Some("A")));
        send(&sup, ENDED_A).await;
        assert_eq!(sup.state(), State::Grace);
        tokio::time::sleep(ms(150)).await;
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(sup.state(), State::Idle);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_level_when_spawn_requested_then_manual_session_spawns() {
    local(async {
        let (sup, runner, _settings) = manual_level_supervisor(10.0, None);
        send(&sup, SPAWN_R1).await;
        assert_eq!(runner.spawned(), manual(1));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_level_and_a_manual_session_when_a_reconnect_reports_a_conference_then_it_keeps_running() {
    local(async {
        let (sup, runner, _settings) = manual_level_supervisor(10.0, Some(true));
        sup.with_lifecycle(|l| l.adopt(Some(MANUAL_MEETING_ID)));
        send(&sup, r#"{"type": "meeting_state", "active": true, "meeting_id": "A"}"#).await;
        assert_eq!(runner.kills.get(), 0);
        assert!(runner.spawned().is_empty());
        assert_eq!((sup.state(), sup.meeting_id().as_deref()), (State::Running, Some(MANUAL_MEETING_ID)));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_level_and_a_conference_session_put_in_grace_by_a_bridge_restart_when_the_same_meeting_starts_then_it_keeps_running()
 {
    local(async {
        let (sup, runner, _settings) = manual_level_supervisor(10.0, None);
        sup.with_lifecycle(|l| l.adopt(Some("A")));
        send(&sup, r#"{"type": "meeting_state", "active": false, "meeting_id": null}"#).await;
        assert_eq!(sup.state(), State::Grace);
        send(&sup, STARTED_A).await;
        assert_eq!(sup.state(), State::Running);
        assert!(!sup.grace_pending());
        assert_eq!(runner.kills.get(), 0);
        assert!(runner.spawned().is_empty());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_level_then_automatic_during_a_conference_when_settings_change_then_the_conference_session_spawns() {
    local(async {
        let (sup, runner, settings) = manual_level_supervisor(10.0, None);
        send(&sup, STARTED_A).await;
        assert!(runner.spawned().is_empty());
        settings.set_automatic(true);
        send(&sup, r#"{"type": "copilot_settings_changed"}"#).await;
        assert_eq!(runner.spawned(), ids(&["A"]));
        sup.stop();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn manual_level_and_a_manual_session_when_automatic_is_chosen_without_a_conference_then_it_keeps_running() {
    // The bridge's last word was "no conference": its hello reply, or a conference that ended.
    for latest in [r#"{"type": "meeting_state", "active": false, "meeting_id": null}"#, ENDED_A] {
        local(async {
            let (sup, runner, settings) = manual_level_supervisor(0.05, Some(true));
            send(&sup, latest).await;
            sup.with_lifecycle(|l| l.adopt(Some(MANUAL_MEETING_ID)));
            settings.set_automatic(true);
            send(&sup, r#"{"type": "copilot_settings_changed"}"#).await;
            tokio::time::sleep(ms(150)).await;
            assert_eq!(runner.kills.get(), 0, "{latest}");
            assert_eq!(
                (sup.state(), sup.meeting_id().as_deref()),
                (State::Running, Some(MANUAL_MEETING_ID)),
                "{latest}"
            );
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn manual_level_and_a_manual_session_when_automatic_is_chosen_during_a_conference_then_the_conference_takes_it_over()
 {
    local(async {
        let (sup, runner, settings) = manual_level_supervisor(10.0, Some(true));
        sup.with_lifecycle(|l| l.adopt(Some(MANUAL_MEETING_ID)));
        send(&sup, STARTED_A).await;
        assert_eq!(sup.meeting_id().as_deref(), Some(MANUAL_MEETING_ID));
        settings.set_automatic(true);
        send(&sup, r#"{"type": "copilot_settings_changed"}"#).await;
        assert_eq!(runner.kills.get(), 1);
        assert_eq!(runner.spawned(), ids(&["A"]));
        sup.stop();
    })
    .await;
}
