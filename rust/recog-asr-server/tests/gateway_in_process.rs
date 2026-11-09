//! The gateway's recorded steps (tests/gateway_frozen.rs) against `gateway::serve` in
//! the test's own process, configured from the same environment as the binary there.
//! The binary is killed at the end of that test and leaves no coverage profile; here
//! the library code that answers is the code measured, against the same recorded
//! Python answers.

mod gateway_support;
mod support;

use std::sync::Arc;

use gateway_support::{FakeBackend, ask, fixture_path, gateway_env, steps, steps_sha256};
use recog_asr_server::config::{Environ, GatewayConfig};
use recog_asr_server::gateway::{self, Gateway};
use support::Diff;

/// `gateway::serve` on a free port, in a thread of its own, for the rest of the test.
fn start_in_process(fake: u16, refused: u16) -> u16 {
    let listener = support::listener();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let env: Environ = gateway_env(port, fake, refused).into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    let gateway = Arc::new(Gateway::new(GatewayConfig::from_environ(&env).unwrap()));
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            gateway::serve(listener, gateway, std::future::pending()).await.unwrap();
        });
    });
    port
}

#[test]
fn recorded_python_answers_when_the_gateway_in_process_is_asked_the_same_then_the_same_answers_and_requests() {
    // given
    let steps = steps();
    let fixture = support::read_fixture(
        &fixture_path(),
        &steps_sha256(&steps),
        "RECOG_RECORD_ASR_SERVER_FIXTURE=1 cargo test -p recog-asr-server --test gateway_differential",
    );
    let fake = FakeBackend::start();
    let refused = support::free_port();
    let port = start_in_process(fake.port, refused);
    // when
    let got = ask(port, &fake, refused, &steps);
    // then
    let mut diff = Diff::new("recorded Python");
    diff.compare(fixture["answers"].as_array().unwrap(), &got);
    diff.finish();
}
