//! The real process table of this Mac (`system::table()`, psutil's macOS answers) on
//! a child the test starts. The expected values are psutil's on the same process.
#![forbid(unsafe_code)]
#![cfg(target_os = "macos")]

use std::io::Read as _;
use std::net::{Ipv6Addr, TcpListener};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use recog_desktop::system;

/// Set in the child's environment, which makes the child wait instead of testing.
const PROBE: &str = "RECOG_DESKTOP_PROBE";
const PROBE_VALUE: &str = "value=with=equals";
const CHILD_TEST: &str = "real_table_given_a_child_of_this_test_when_read_then_psutil_answers";

/// The child, killed and reaped however the test ends.
struct Started(Child);

impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

#[test]
fn real_table_given_a_child_of_this_test_when_read_then_psutil_answers() {
    if std::env::var_os(PROBE).is_some() {
        // The child: wait until the parent closes stdin.
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        return;
    }
    // given: this test binary again, a child that only waits, with an environment of
    // its own (a platform binary such as /bin/sleep shows none to its user), and a
    // listener of the parent's
    let program = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let mut environment = vec![(PROBE.to_owned(), PROBE_VALUE.to_owned())];
    // A coverage run keeps its profile path, or the child would write one into the crate.
    if let Ok(profile) = std::env::var("LLVM_PROFILE_FILE") {
        environment.push(("LLVM_PROFILE_FILE".to_owned(), profile));
    }
    let _listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let before = now();
    let child = Command::new(&program)
        .args(["--exact", CHILD_TEST])
        .env_clear()
        .envs(environment.clone())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let after = now();
    let child = Started(child);
    let pid = i64::from(child.0.id());
    let table = system::table();
    // when
    let started = table.create_time(pid).unwrap();
    let mut environ = table.environ(pid).unwrap();
    // then
    let program = program.to_str().unwrap();
    let file_name = program.rsplit('/').next().unwrap();
    assert!(file_name.len() > 16, "a name the kernel truncates: {file_name}");
    assert!(table.pids().contains(&pid));
    table.open(pid).unwrap();
    assert_eq!(table.name(pid).unwrap(), file_name, "the truncated name extended from the first argument");
    assert_eq!(table.uid_real(pid).unwrap(), nix::unistd::getuid().as_raw());
    assert!(before - 0.001 <= started && started <= after + 0.001, "{before} <= {started} <= {after}");
    assert_eq!(table.create_time(pid).unwrap(), started, "a recorded target is recognised by it");
    assert_eq!(table.exe(pid).unwrap(), program);
    assert_eq!(table.cmdline(pid).unwrap(), [program, "--exact", CHILD_TEST]);
    environ.sort();
    environment.sort();
    assert_eq!(environ, environment, "without the kernel's strings after the environment");
    assert_eq!(table.tcp_listeners(pid).unwrap(), [], "the parent's listener is not the child's");
}
