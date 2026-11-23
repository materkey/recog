//! The Codex harness's decisions (docs/plans/completed/2026-11-23-codex-copilot.md, D2 and D4),
//! fed with Copilot events and app-server events, no process involved.

use recog_copilot::codex::app_server::{Event, Failure, TurnStatus};
use recog_copilot::codex::harness::{Command, Harness, Kind, Status};

fn ready() -> Harness {
    let mut harness = Harness::new();
    harness.ready(true);
    harness
}

fn start(text: &str) -> Vec<Command> {
    vec![Command::StartTurn(text.into())]
}

fn completed(status: TurnStatus, info: Option<&str>) -> Event {
    let error = info.map(|info| Failure { info: Some(info.into()), message: format!("{info} message") });
    Event::TurnCompleted { turn_id: "t".into(), status, error }
}

#[test]
fn new_harness_when_thread_ready_then_spawning_turns_online_or_blocked_without_login() {
    let mut harness = Harness::new();
    assert_eq!(harness.status(), &Status::Spawning);
    harness.ready(true);
    assert_eq!(harness.status(), &Status::Online);
    let mut logged_out = Harness::new();
    logged_out.ready(false);
    assert!(matches!(logged_out.status(), Status::Blocked(reason) if reason.contains("codex login")));
}

#[test]
fn idle_harness_when_an_event_arrives_then_one_turn_and_working() {
    let mut harness = ready();
    assert_eq!(harness.on_copilot(Kind::Tick, "tick 1".into()), start("tick 1"));
    assert_eq!(harness.status(), &Status::Working);
}

#[test]
fn running_turn_when_the_user_types_then_the_message_steers_it_at_once() {
    let mut harness = ready();
    harness.on_copilot(Kind::Tick, "tick 1".into());
    assert_eq!(harness.on_copilot(Kind::UserMessage, "вопрос".into()), start("вопрос"));
}

#[test]
fn running_turn_when_ticks_arrive_then_only_the_latest_follows_its_completion() {
    let mut harness = ready();
    harness.on_copilot(Kind::Tick, "tick 1".into());
    assert!(harness.on_copilot(Kind::Tick, "tick 2".into()).is_empty());
    assert!(harness.on_copilot(Kind::Tick, "tick 3".into()).is_empty());
    assert_eq!(harness.on_app(&completed(TurnStatus::Completed, None)), start("tick 3"));
    assert_eq!(harness.status(), &Status::Working);
    assert!(harness.on_app(&completed(TurnStatus::Completed, None)).is_empty());
    assert_eq!(harness.status(), &Status::Online);
}

#[test]
fn meeting_end_when_sent_then_the_harness_exits_after_that_turn() {
    let mut harness = ready();
    harness.on_copilot(Kind::Tick, "tick 1".into());
    assert_eq!(harness.on_copilot(Kind::MeetingEnded, "ended".into()), start("ended"));
    assert!(harness.on_copilot(Kind::Tick, "late tick".into()).is_empty());
    assert_eq!(harness.on_app(&completed(TurnStatus::Completed, None)), vec![Command::Exit]);
}

#[test]
fn spent_subscription_when_a_turn_fails_then_blocked_ticks_dropped_and_a_later_success_clears_it() {
    let mut harness = ready();
    harness.on_copilot(Kind::Tick, "tick 1".into());
    let limit = Failure { info: Some("usageLimitExceeded".into()), message: "You've hit your usage limit.".into() };
    assert!(harness.on_app(&Event::Error { failure: limit, will_retry: false }).is_empty());
    let failed = Event::TurnCompleted { turn_id: "t".into(), status: TurnStatus::Failed, error: None };
    assert!(harness.on_app(&failed).is_empty());
    assert!(matches!(harness.status(), Status::Blocked(reason) if reason.contains("usage limit")));
    assert!(harness.on_copilot(Kind::Tick, "tick 2".into()).is_empty());
    assert_eq!(harness.on_copilot(Kind::UserMessage, "ещё раз".into()), start("ещё раз"));
    harness.on_app(&completed(TurnStatus::Completed, None));
    assert_eq!(harness.status(), &Status::Online);
}

#[test]
fn expired_login_when_a_turn_fails_then_blocked_naming_the_login() {
    let mut harness = ready();
    harness.on_copilot(Kind::UserMessage, "вопрос".into());
    harness.on_app(&completed(TurnStatus::Failed, Some("unauthorized")));
    assert!(matches!(harness.status(), Status::Blocked(reason) if reason.contains("codex login")));
}

#[test]
fn full_context_when_a_turn_fails_then_a_new_thread_before_the_held_tick() {
    let mut harness = ready();
    harness.on_copilot(Kind::Tick, "tick 1".into());
    harness.on_copilot(Kind::Tick, "tick 2".into());
    assert_eq!(
        harness.on_app(&completed(TurnStatus::Failed, Some("contextWindowExceeded"))),
        vec![Command::NewThread, Command::StartTurn("tick 2".into())]
    );
}

#[test]
fn other_failure_or_retry_when_reported_then_still_usable() {
    let mut harness = ready();
    harness.on_copilot(Kind::UserMessage, "вопрос".into());
    let retry = Failure { info: Some("httpConnectionFailed".into()), message: "502".into() };
    assert!(harness.on_app(&Event::Error { failure: retry, will_retry: true }).is_empty());
    assert_eq!(harness.status(), &Status::Working);
    harness.on_app(&completed(TurnStatus::Failed, Some("httpConnectionFailed")));
    assert_eq!(harness.status(), &Status::Online);
}

#[test]
fn statuses_when_written_then_one_line_the_supervisor_reads() {
    assert_eq!(Status::Online.line(), "recog-copilot-codex: online");
    assert_eq!(Status::Working.line(), "recog-copilot-codex: working");
    assert_eq!(Status::Spawning.line(), "recog-copilot-codex: spawning");
    assert_eq!(
        Status::Blocked("Codex usage limit\nreached".into()).line(),
        "recog-copilot-codex: blocked: Codex usage limit reached"
    );
}

#[test]
fn event_types_when_classified_then_kinds() {
    assert_eq!(Kind::of("copilot_user_message"), Kind::UserMessage);
    assert_eq!(Kind::of("transcript_tick"), Kind::Tick);
    assert_eq!(Kind::of("meeting_ended"), Kind::MeetingEnded);
    assert_eq!(Kind::of("copilot_policy_updated"), Kind::Other);
}

#[test]
fn rate_limit_when_a_turn_fails_then_blocked_naming_it() {
    let mut harness = ready();
    harness.on_copilot(Kind::UserMessage, "вопрос".into());
    harness.on_app(&completed(TurnStatus::Failed, Some("rateLimitExceeded")));
    assert!(matches!(harness.status(), Status::Blocked(reason) if reason.contains("rate limit")));
}
