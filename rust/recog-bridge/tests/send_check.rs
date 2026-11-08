//! The handler futures are `Send`: `recog-live` may spawn them on any runtime.

use std::sync::Arc;

fn assert_send<T: Send>(_: T) {}

#[allow(dead_code)]
fn handlers_are_send(bridge: Arc<recog_bridge::Bridge>, c: recog_bridge::Connection, m: recog_core::pyjson::Map) {
    assert_send(bridge.handle_client(c));
    assert_send(bridge.process_message(&m, None));
    assert_send(bridge.end_meeting_on_extension_gone());
    assert_send(bridge.recompute_agent_state(None));
}
