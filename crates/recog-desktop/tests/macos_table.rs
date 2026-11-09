//! The real process table of this Mac (`system::table()`, psutil's macOS answers) on
//! processes the test owns: itself, a child it starts, one it reaped, and the kernel's
//! own. The expected values are psutil's on the same processes.
#![forbid(unsafe_code)]
#![cfg(target_os = "macos")]

use std::io::Read as _;
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream, UdpSocket};
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

#[test]
fn real_table_given_sockets_of_this_process_when_listeners_read_then_its_tcp_listeners_only() {
    // given: a loopback listener with an accepted connection and a UDP socket on its
    // port, and an IPv6 loopback listener
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    let _accepted = listener.accept().unwrap();
    let _udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let listener6 = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
    let port6 = listener6.local_addr().unwrap().port();
    // when
    let listed = system::table().tcp_listeners(i64::from(std::process::id())).unwrap();
    // then
    let count = |address: &str, port: u16| listed.iter().filter(|(a, p)| a == address && *p == port).count();
    assert_eq!(count("127.0.0.1", port), 1, "{listed:?}");
    assert_eq!(count("::1", port6), 1, "{listed:?}");
    assert_eq!(count("127.0.0.1", client.local_addr().unwrap().port()), 0, "{listed:?}");
}

#[test]
fn real_table_given_a_reaped_child_when_read_then_no_such_process() {
    // given
    let mut child = Command::new("/usr/bin/true").spawn().unwrap();
    child.wait().unwrap();
    let pid = i64::from(child.id());
    let table = system::table();
    // when
    let failures = [
        table.open(pid).err(),
        table.name(pid).err(),
        table.uid_real(pid).err(),
        table.create_time(pid).err(),
        table.exe(pid).err(),
        table.cmdline(pid).err(),
        table.environ(pid).err(),
        table.tcp_listeners(pid).err(),
    ];
    // then
    assert_eq!(failures.map(|failure| failure.map(|e| e.kind)), [Some("NoSuchProcess"); 8]);
    assert!(!table.pids().contains(&pid));
}

#[test]
fn real_table_given_pids_out_of_range_when_opened_then_refused_like_psutil() {
    // given
    let table = system::table();
    // when
    let huge = [table.open(1 << 40).err(), table.exe(1 << 40).err()];
    let negative = table.open(-1).unwrap_err();
    // then
    assert_eq!(huge.map(|failure| failure.map(|e| e.kind)), [Some("NoSuchProcess"); 2]);
    assert_eq!((negative.kind, negative.message.as_str()), ("ValueError", "pid must be a positive integer (got -1)"));
}

#[test]
fn real_table_given_the_kernel_task_when_read_then_named_and_its_executable_denied() {
    // given
    let table = system::table();
    // when
    let opened = table.open(0);
    // then
    assert_eq!(opened, Ok(()));
    assert_eq!(table.name(0).unwrap(), "kernel_task");
    assert_eq!(table.uid_real(0).unwrap(), 0);
    assert_eq!(table.exe(0).unwrap_err().kind, "AccessDenied");
}
