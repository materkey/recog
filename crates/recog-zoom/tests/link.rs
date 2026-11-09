//! The helper link, the pump to the bridge, the log, reconnects and helper restarts,
//! with a stand-in helper on the real Unix socket and channels for the bridge. Ported
//! from the helper-link, pump, serve and run parts of `tests/test_zoom_app.py`.
#![forbid(unsafe_code)]

mod common;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use recog_zoom::ZoomAppSession;
use recog_zoom::bridge::{self, BridgeLink, Connector, Incoming, PumpError};
use recog_zoom::helper_app::{CommandOutcome, HELPER_SOURCE, Toolchain};
use recog_zoom::helper_link::{HelperLines, Launcher, helper_lines};
use recog_zoom::log::{Logger, StatusLog};
use recog_zoom::run::{
    Options, RunError, Timing, run_from, run_until_interrupted, run_with, spawn_source_thread, supervise,
    supervise_blocking,
};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// What a launched stand-in helper does.
enum Script {
    /// Connect and write each line sent on the channel; close when it closes.
    Lines(mpsc::UnboundedReceiver<String>),
    /// Never connect.
    Absent,
    /// `open` fails with this message.
    Fail(&'static str),
}

fn lines_script(lines: &[Value]) -> Script {
    let (tx, rx) = mpsc::unbounded_channel();
    for line in lines {
        tx.send(format!("{line}\n")).unwrap();
    }
    Script::Lines(rx)
}

/// `open` that starts a stand-in helper as LaunchServices would start the real one.
#[derive(Default)]
struct StandIns {
    scripts: Mutex<VecDeque<Script>>,
    launched: Mutex<Vec<Vec<String>>>,
}

impl StandIns {
    fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        Self { scripts: Mutex::new(scripts.into_iter().collect()), launched: Mutex::default() }
    }

    fn launched(&self) -> Vec<Vec<String>> {
        self.launched.lock().unwrap().clone()
    }
}

impl Launcher for StandIns {
    async fn launch(&self, command: Vec<String>) -> std::io::Result<CommandOutcome> {
        let socket = PathBuf::from(&command[command.iter().position(|arg| arg == "--socket").unwrap() + 1]);
        self.launched.lock().unwrap().push(command);
        let script = self.scripts.lock().unwrap().pop_front().expect("a script per launch");
        match script {
            Script::Fail(message) => return Ok(CommandOutcome { success: false, stderr: format!("{message}\n") }),
            Script::Absent => {}
            Script::Lines(mut lines) => {
                tokio::spawn(async move {
                    let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
                    while let Some(line) = lines.recv().await {
                        if stream.write_all(line.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        }
        Ok(CommandOutcome { success: true, stderr: String::new() })
    }
}

fn app(root: &Path) -> PathBuf {
    root.join("RecogZoomHelper.app")
}

async fn connect(stand_ins: &StandIns, root: &Path) -> HelperLines {
    helper_lines(&app(root), true, stand_ins, Duration::from_secs(5)).await.unwrap()
}

/// The bridge end of a link: messages to push, frames received.
struct BridgeEnd {
    incoming: mpsc::Sender<Incoming>,
    sent: mpsc::Receiver<String>,
}

impl BridgeEnd {
    fn sent_types(&mut self) -> Vec<String> {
        let mut types = Vec::new();
        while let Ok(frame) = self.sent.try_recv() {
            types.push(serde_json::from_str::<Value>(&frame).unwrap()["type"].as_str().unwrap().to_owned());
        }
        types
    }
}

fn link() -> (BridgeLink, BridgeEnd) {
    let (incoming, incoming_rx) = mpsc::channel(16);
    let (outgoing, sent) = mpsc::channel(16);
    (BridgeLink { incoming: incoming_rx, outgoing }, BridgeEnd { incoming, sent })
}

fn lines_of(buffer: &Arc<Mutex<Vec<u8>>>) -> Vec<String> {
    String::from_utf8(buffer.lock().unwrap().clone()).unwrap().lines().map(String::from).collect()
}

// --- helper link ------------------------------------------------------------------

#[tokio::test]
async fn given_helper_application_when_started_then_it_is_launched_by_launchservices_and_its_lines_arrive() {
    let root = tempfile::tempdir().unwrap();
    let stand_ins = StandIns::new([lines_script(&[json!({"state": "idle"})])]);

    let mut lines = connect(&stand_ins, root.path()).await;
    let first = lines.next_line().await.unwrap().unwrap();
    let rest = lines.next_line().await.unwrap();
    let socket = lines.socket_path().to_path_buf();
    drop(lines);

    assert_eq!(serde_json::from_slice::<Value>(&first).unwrap(), json!({"state": "idle"}));
    assert_eq!(rest, None); // the helper went away: the reader sees the end
    let command = &stand_ins.launched()[0];
    assert_eq!(command[..5], ["open", "-g", "-n", "-a", &app(root.path()).display().to_string()]);
    assert_eq!(command[5..7], ["--args", "--socket"]);
    assert!(socket.file_name().is_some_and(|name| name == "probe.sock"));
    assert!(!socket.parent().unwrap().exists()); // the private directory is removed
}

#[tokio::test]
async fn given_restart_after_a_missing_permission_when_started_then_the_helper_is_told_not_to_prompt() {
    let root = tempfile::tempdir().unwrap();
    let stand_ins = StandIns::new([lines_script(&[])]);

    let mut lines = helper_lines(&app(root.path()), false, &stand_ins, Duration::from_secs(5)).await.unwrap();
    assert_eq!(lines.next_line().await.unwrap(), None);

    assert_eq!(stand_ins.launched()[0].last().unwrap(), "--no-prompt");
}

#[tokio::test]
async fn given_helper_never_connects_when_started_then_error_says_so() {
    let root = tempfile::tempdir().unwrap();
    let stand_ins = StandIns::new([Script::Absent]);

    let error = helper_lines(&app(root.path()), true, &stand_ins, Duration::from_millis(50)).await.err().unwrap();

    assert_eq!(error.0, "RecogZoomHelper.app did not connect within 0 s");
}

#[tokio::test]
async fn given_launch_fails_when_started_then_error_carries_the_reason() {
    let root = tempfile::tempdir().unwrap();
    let stand_ins = StandIns::new([Script::Fail("Unable to find application")]);

    let error = helper_lines(&app(root.path()), true, &stand_ins, Duration::from_secs(5)).await.err().unwrap();

    assert_eq!(error.0, "cannot launch RecogZoomHelper.app: Unable to find application");
}

#[tokio::test]
async fn given_a_second_helper_connects_when_one_is_served_then_it_is_turned_away() {
    let root = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let stand_ins = StandIns::new([Script::Lines(rx)]);
    let mut lines = connect(&stand_ins, root.path()).await;

    let mut intruder = tokio::net::UnixStream::connect(lines.socket_path()).await.unwrap();
    let _ = intruder.write_all(b"{\"state\": \"meeting\"}\n").await;
    tx.send("{\"state\": \"idle\"}\n".into()).unwrap();

    assert_eq!(serde_json::from_slice::<Value>(&lines.next_line().await.unwrap().unwrap()).unwrap()["state"], "idle");
}

// --- pump -------------------------------------------------------------------------

#[tokio::test]
async fn given_helper_snapshots_when_pumped_then_frames_reach_the_bridge_and_eof_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let stand_ins =
        StandIns::new([lines_script(&[meeting(&[tile("Host", &[])]), meeting(&[tile("Bob", &["active speaker"])])])]);
    let mut lines = connect(&stand_ins, root.path()).await;
    let (mut link, mut bridge_end) = link();
    let (logger, _) = Logger::buffer();

    let ended = bridge::pump(
        &mut link,
        &mut lines,
        &mut ZoomAppSession::default(),
        &mut StatusLog::new(logger.clone()),
        &logger,
    )
    .await;

    assert_eq!(ended, PumpError::HelperGone);
    assert_eq!(ended.to_string(), "the Zoom helper disconnected");
    assert_eq!(bridge_end.sent_types(), ["meeting_join", "participants_update", "speaker_update"]);
}

#[tokio::test]
async fn given_dialog_and_unreadable_tile_when_pumped_then_the_log_says_what_was_understood() {
    let root = tempfile::tempdir().unwrap();
    let stand_ins = StandIns::new([lines_script(&[
        meeting_in("minimal", &[group("Reactions", &["Clap"])], &[]),
        meeting(&[group("Alice, Video on", &["Open Alice's card"])]),
    ])]);
    let mut lines = connect(&stand_ins, root.path()).await;
    let (mut link, _bridge_end) = link();
    let (logger, buffer) = Logger::buffer();

    bridge::pump(&mut link, &mut lines, &mut ZoomAppSession::default(), &mut StatusLog::new(logger.clone()), &logger)
        .await;

    let logged = lines_of(&buffer);
    assert_eq!(logged[0], "[zoom-app] Zoom is running, no meeting");
    assert_eq!(logged[1], "[zoom-app] Zoom meeting detected");
    assert_eq!(
        logged[2],
        "[zoom-app] 1 participant tile(s) ignored: Zoom names them in a way this version does not know, e.g. \
         {\"description\": \"Alice, Video on\", \"children\": [{\"role\": \"AXImage\"}, \
         {\"role\": \"AXButton\", \"description\": \"Open Alice's card\"}]}"
    );
}

#[tokio::test]
async fn given_bridge_rejects_the_join_when_pumped_then_no_state_frames_follow() {
    let root = tempfile::tempdir().unwrap();
    let (helper, rx) = mpsc::unbounded_channel();
    let stand_ins = StandIns::new([Script::Lines(rx)]);
    let mut lines = connect(&stand_ins, root.path()).await;
    let (mut link, mut bridge_end) = link();
    let (logger, buffer) = Logger::buffer();
    let mut session = ZoomAppSession::default();
    let mut log = StatusLog::new(logger.clone());
    helper.send(format!("{}\n", meeting(&[tile("Host", &[])]))).unwrap();

    let pumping = bridge::pump(&mut link, &mut lines, &mut session, &mut log, &logger);
    let driving = async {
        let join = bridge_end.sent.recv().await.unwrap();
        assert!(join.contains("meeting_join"));
        let rejected = json!({"type": "meeting_join_rejected", "reason": "another_tab_active"});
        bridge_end.incoming.send(Incoming::Message(rejected.to_string().into_bytes())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        helper.send(format!("{}\n", meeting(&[tile("Bob", &["active speaker"])]))).unwrap();
        drop(helper);
    };
    let (ended, ()) = tokio::join!(pumping, driving);

    assert_eq!(ended, PumpError::HelperGone);
    assert!(bridge_end.sent_types().is_empty());
    assert!(!session.joined);
    assert!(lines_of(&buffer).contains(&"[zoom-app] join rejected: another_tab_active".to_owned()));
}

#[tokio::test]
async fn given_a_line_python_cannot_read_when_pumped_then_the_run_fails() {
    let root = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tx.send("not json\n".to_owned()).unwrap();
    let stand_ins = StandIns::new([Script::Lines(rx)]);
    let mut lines = connect(&stand_ins, root.path()).await;
    let (mut link, _bridge_end) = link();
    let (logger, _) = Logger::buffer();

    let ended = bridge::pump(
        &mut link,
        &mut lines,
        &mut ZoomAppSession::default(),
        &mut StatusLog::new(logger.clone()),
        &logger,
    )
    .await;

    assert!(matches!(ended, PumpError::Fatal(_)), "{ended:?}");
}

/// Pump one helper line into a bridge that stopped reading frames; `then` is what the
/// bridge end does once the frames could not be sent.
async fn pump_into_closed_bridge(then: Option<Incoming>) -> PumpError {
    let root = tempfile::tempdir().unwrap();
    let (helper, rx) = mpsc::unbounded_channel();
    let stand_ins = StandIns::new([Script::Lines(rx)]);
    let mut lines = connect(&stand_ins, root.path()).await;
    let (mut link, bridge_end) = link();
    let BridgeEnd { incoming, sent } = bridge_end;
    drop(sent); // the bridge's writer is gone, its reader is not yet
    let (logger, _) = Logger::buffer();
    let mut session = ZoomAppSession::default();
    let mut log = StatusLog::new(logger.clone());
    helper.send(format!("{}\n", meeting(&[tile("Host", &[])]))).unwrap();

    let pumping = bridge::pump(&mut link, &mut lines, &mut session, &mut log, &logger);
    let driving = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        match then {
            Some(message) => incoming.send(message).await.unwrap(),
            None => drop(incoming),
        }
    };
    let (ended, ()) = tokio::join!(pumping, driving);
    drop(helper);
    ended
}

#[tokio::test]
async fn given_bridge_stops_reading_when_a_frame_is_sent_then_the_reason_it_reports_ends_the_pump() {
    let ended = pump_into_closed_bridge(Some(Incoming::Lost("received 1001 (going away)".into()))).await;

    assert_eq!(ended, PumpError::Link("received 1001 (going away)".into()));
    assert_eq!(ended.to_string(), "bridge link lost: received 1001 (going away)");
}

#[tokio::test]
async fn given_bridge_stops_reading_and_closes_without_a_reason_when_a_frame_is_sent_then_connection_closed() {
    let ended = pump_into_closed_bridge(None).await;

    assert_eq!(ended, PumpError::Link("connection closed".into()));
}

#[tokio::test]
async fn given_an_idle_helper_when_the_bridge_link_is_lost_then_its_reason_ends_the_pump() {
    let root = tempfile::tempdir().unwrap();
    let (helper, rx) = mpsc::unbounded_channel::<String>();
    let stand_ins = StandIns::new([Script::Lines(rx)]);
    let mut lines = connect(&stand_ins, root.path()).await;
    let (mut link, bridge_end) = link();
    let (logger, _) = Logger::buffer();
    bridge_end.incoming.send(Incoming::Lost("keepalive ping timeout".into())).await.unwrap();

    let ended = bridge::pump(
        &mut link,
        &mut lines,
        &mut ZoomAppSession::default(),
        &mut StatusLog::new(logger.clone()),
        &logger,
    )
    .await;

    assert_eq!(ended, PumpError::Link("keepalive ping timeout".into()));
    drop(helper);
}

// --- status log -------------------------------------------------------------------

#[test]
fn given_helper_state_changes_when_logged_then_one_line_per_change() {
    let (logger, buffer) = Logger::buffer();
    let mut log = StatusLog::new(logger);

    for state in ["not_trusted", "not_trusted", "idle", "meeting", "weird"] {
        log.note(Some(&json!(state))).unwrap();
    }

    let logged = lines_of(&buffer);
    assert_eq!(logged.len(), 4);
    assert!(logged[0].contains("Accessibility"));
    assert!(logged[3].contains("unknown helper state: weird"));
}

#[test]
fn given_unreadable_tiles_when_logged_then_one_line_per_meeting_with_a_sample() {
    let (logger, buffer) = Logger::buffer();
    let mut log = StatusLog::new(logger);
    let renamed = group("Alice, Video on", &["Open Alice's card"]);
    let meeting = json!("meeting");

    log.note(Some(&meeting)).unwrap();
    log.note_unread(&[]);
    log.note_unread(&[renamed.clone(), renamed.clone()]);
    log.note_unread(std::slice::from_ref(&renamed));
    log.note(Some(&json!("idle"))).unwrap();
    log.note(Some(&meeting)).unwrap();
    log.note_unread(&[renamed]);

    let ignored: Vec<_> = lines_of(&buffer).into_iter().filter(|line| line.contains("ignored")).collect();
    assert_eq!(ignored.len(), 2);
    assert!(ignored[0].starts_with("[zoom-app] 2 participant tile(s) ignored"));
    assert!(ignored[0].contains("Open Alice's card"));
}

#[test]
fn given_unknown_menu_audio_item_when_logged_then_one_line_per_meeting() {
    let (logger, buffer) = Logger::buffer();
    let mut log = StatusLog::new(logger);
    let meeting = json!("meeting");

    log.note(Some(&meeting)).unwrap();
    log.note_own_mic_unknown(false);
    log.note_own_mic_unknown(true);
    log.note_own_mic_unknown(true);
    log.note(Some(&json!("idle"))).unwrap();
    log.note(Some(&meeting)).unwrap();
    log.note_own_mic_unknown(true);

    assert_eq!(lines_of(&buffer).iter().filter(|line| line.contains("audio item")).count(), 2);
}

// --- serve and run: bridge link, helper restarts ----------------------------------

/// Links the test hands out one per attempt: `None` refuses the connection.
#[derive(Default)]
struct Links {
    plan: Mutex<VecDeque<Option<BridgeLink>>>,
    attempts: Mutex<Vec<String>>,
    // Once the plan is used up: open links whose bridge end stays open.
    open_ends: Mutex<Vec<BridgeEnd>>,
}

impl Connector for Links {
    async fn connect(&self, url: &str) -> Result<BridgeLink, String> {
        self.attempts.lock().unwrap().push(url.to_owned());
        match self.plan.lock().unwrap().pop_front() {
            Some(Some(link)) => Ok(link),
            Some(None) => Err("connection refused".into()),
            None => {
                let (link, end) = link();
                self.open_ends.lock().unwrap().push(end);
                Ok(link)
            }
        }
    }
}

#[tokio::test]
async fn given_bridge_down_then_up_when_serving_then_it_reconnects_until_the_helper_is_gone() {
    let root = tempfile::tempdir().unwrap();
    let (helper, rx) = mpsc::unbounded_channel();
    let stand_ins = StandIns::new([Script::Lines(rx)]);
    let mut lines = connect(&stand_ins, root.path()).await;
    let (closed, closed_end) = link();
    drop(closed_end); // the bridge closes this one at once
    let (third, mut third_end) = link();
    let links = Links { plan: Mutex::new(VecDeque::from([None, Some(closed), Some(third)])), ..Links::default() };
    let (logger, buffer) = Logger::buffer();
    let mut session = joined_session(100.0);
    helper.send(format!("{}\n", meeting(&[tile("Host", &[])]))).unwrap();
    drop(helper);

    let ended = bridge::serve(
        &links,
        "ws://bridge.test",
        &mut lines,
        &mut session,
        &mut StatusLog::new(logger.clone()),
        &logger,
        Duration::ZERO,
    )
    .await;

    assert_eq!(ended, PumpError::HelperGone);
    assert_eq!(*links.attempts.lock().unwrap(), ["ws://bridge.test"; 3]);
    assert!(lines_of(&buffer).contains(&"[zoom-app] bridge link lost: connection refused".to_owned()));
    // on_disconnect ran between the links: the meeting is joined again on the third.
    assert_eq!(third_end.sent_types(), ["meeting_join"]);
}

fn timing() -> Timing {
    Timing {
        reconnect: Duration::ZERO,
        permission_retry: Duration::ZERO,
        helper_connect: Duration::from_secs(5),
        restart: Duration::ZERO,
    }
}

#[tokio::test]
async fn given_permission_missing_then_granted_when_running_then_a_fresh_helper_is_started_quietly() {
    // A running helper cannot see the grant (macOS caches the answer per process), so
    // it exits and a new one is started; only the first may prompt.
    let root = tempfile::tempdir().unwrap();
    let not_trusted = json!({"state": "not_trusted"});
    let stand_ins = StandIns::new([
        lines_script(std::slice::from_ref(&not_trusted)),
        lines_script(&[not_trusted]),
        lines_script(&[json!({"state": "idle"})]),
    ]);
    let (logger, _) = Logger::buffer();

    let ended = run_with(
        app(root.path()),
        ZoomAppSession::default(),
        &stand_ins,
        &Links::default(),
        "ws://bridge.test",
        &timing(),
        &logger,
    )
    .await;

    assert_eq!(ended, RunError::Helper(PumpError::HelperGone));
    let prompts: Vec<bool> =
        stand_ins.launched().iter().map(|command| !command.contains(&"--no-prompt".to_owned())).collect();
    assert_eq!(prompts, [true, false, false]);
}

#[tokio::test]
async fn given_helper_with_permission_dies_when_running_then_the_failure_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let stand_ins = StandIns::new([lines_script(&[meeting(&[tile("Host", &[])])])]);
    let (logger, _) = Logger::buffer();

    let ended = run_with(
        app(root.path()),
        ZoomAppSession::default(),
        &stand_ins,
        &Links::default(),
        "ws://bridge.test",
        &timing(),
        &logger,
    )
    .await;

    assert_eq!(ended.to_string(), "the Zoom helper disconnected");
    assert_eq!(stand_ins.launched().len(), 1);
}

#[tokio::test]
async fn given_source_fails_when_supervised_then_it_is_logged_and_started_over() {
    let attempts = Arc::new(Mutex::new(0));
    let stop = Arc::new(tokio::sync::Notify::new());
    let (logger, buffer) = Logger::buffer();
    let start = || {
        let attempts = Arc::clone(&attempts);
        let stop = Arc::clone(&stop);
        async move {
            *attempts.lock().unwrap() += 1;
            if *attempts.lock().unwrap() == 2 {
                stop.notify_one();
            }
            RunError::Helper(PumpError::HelperGone)
        }
    };

    supervise(start, Duration::ZERO, &logger, stop.notified()).await;

    assert_eq!(*attempts.lock().unwrap(), 2);
    assert!(lines_of(&buffer)[0].contains("stopped: the Zoom helper disconnected; starting over in 0 s"));
}

/// A machine without the compiler and the signing tool.
struct NoTools;

impl Toolchain for NoTools {
    fn which(&self, _name: &str) -> Option<PathBuf> {
        None
    }

    fn run(&self, command: &[std::ffi::OsString]) -> std::io::Result<CommandOutcome> {
        panic!("nothing may be built here: {command:?}")
    }
}

fn checkout(built: bool) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join(HELPER_SOURCE);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(source, "// source").unwrap();
    if built {
        let signature = app(root.path()).join("Contents/_CodeSignature");
        std::fs::create_dir_all(&signature).unwrap();
        std::fs::write(signature.join("CodeResources"), "seal").unwrap();
    }
    root
}

fn run_options(root: &Path) -> Options {
    Options {
        zoom_apps: Vec::new(),
        bridge_url: "ws://bridge.test".into(),
        timing: timing(),
        ..Options::new(root.into())
    }
}

/// An environment that sets nothing but an absent configuration file in the checkout.
fn bare_environment(name: &str) -> Option<String> {
    (name == "RECOG_CONFIG_FILE").then(|| "absent-config.toml".into())
}

#[tokio::test]
async fn given_a_built_helper_when_run_from_options_then_it_is_launched_and_its_end_surfaces() {
    let root = checkout(true);
    let stand_ins = StandIns::new([lines_script(&[meeting(&[tile("Host", &[])])])]);
    let links = Links::default();
    let (logger, _) = Logger::buffer();

    let ended = run_from(&run_options(root.path()), &bare_environment, &NoTools, &stand_ins, &links, &logger).await;

    assert_eq!(ended, RunError::Helper(PumpError::HelperGone));
    assert_eq!(stand_ins.launched()[0][4], app(root.path()).display().to_string());
    assert_eq!(*links.attempts.lock().unwrap(), ["ws://bridge.test"]);
}

#[tokio::test]
async fn given_no_helper_and_no_compiler_when_run_from_options_then_nothing_is_launched() {
    let root = checkout(false);
    let stand_ins = StandIns::new([]);
    let (logger, _) = Logger::buffer();

    let ended =
        run_from(&run_options(root.path()), &bare_environment, &NoTools, &stand_ins, &Links::default(), &logger).await;

    assert_eq!(ended.to_string(), "swiftc not found — cannot build RecogZoomHelper.app (xcode-select --install)");
    assert!(stand_ins.launched().is_empty());
}

#[test]
fn given_source_fails_when_run_until_interrupted_then_the_reason_is_logged_and_it_exits_1() {
    let (logger, buffer) = Logger::buffer();

    let code = run_until_interrupted(
        async { RunError::Build("cannot read the source".into()) },
        std::future::pending::<()>(),
        &logger,
    );

    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(lines_of(&buffer), ["[zoom-app] stopped: cannot read the source"]);
}

#[test]
fn given_interrupt_when_running_until_interrupted_then_it_exits_0_quietly() {
    let (logger, buffer) = Logger::buffer();

    let code = run_until_interrupted(std::future::pending::<RunError>(), async {}, &logger);

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(lines_of(&buffer).is_empty());
}

#[test]
fn given_source_fails_when_supervised_on_a_runtime_of_its_own_then_it_starts_over_until_stopped() {
    let attempts = Arc::new(Mutex::new(0));
    let stop = Arc::new(tokio::sync::Notify::new());
    let (logger, buffer) = Logger::buffer();
    let start = || {
        let attempts = Arc::clone(&attempts);
        let stop = Arc::clone(&stop);
        async move {
            *attempts.lock().unwrap() += 1;
            if *attempts.lock().unwrap() == 3 {
                stop.notify_one();
            }
            RunError::Build("no compiler".into())
        }
    };

    supervise_blocking(start, Duration::ZERO, &logger, stop.notified());

    assert_eq!(*attempts.lock().unwrap(), 3);
    assert_eq!(lines_of(&buffer)[1], "[zoom-app] stopped: no compiler; starting over in 0 s");
}

#[test]
fn given_a_body_when_started_in_the_source_thread_then_it_runs_there_with_the_logger() {
    let (logger, buffer) = Logger::buffer();

    let thread = spawn_source_thread(logger, |logger| {
        logger.line(&format!("running in {}", std::thread::current().name().unwrap_or("an unnamed thread")));
    });

    thread.expect("the source thread starts").join().unwrap();
    assert_eq!(lines_of(&buffer), ["[zoom-app] running in zoom-app"]);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn given_other_os_when_started_in_background_then_nothing_starts() {
    let options = recog_zoom::run::Options::new(PathBuf::from("/nonexistent"));
    assert!(recog_zoom::run::spawn_in_background(options).is_none());
}
