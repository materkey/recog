"""Tests for recog/web/server.py copilot proxy (CAP-5, story 5).

`/api/live/stream` used to discard everything the browser sends. Now the
receive loop routes frames through `handle_client_frame`: a three-type
inbound whitelist relays accepted frames verbatim via `subscriber.send(raw)`
and answers `copilot_status {status: "bridge_disconnected", request_id}` to
the originating client on failure. The server is a pure relay (D5) — the only
state it keeps is a primer cache of the last
`copilot_status {status: "agent_state"}` bridge frame, delivered right after
`init` on each new live-stream connection.
"""
import json
import sys
from pathlib import Path
from unittest.mock import Mock

import pytest
from starlette.datastructures import Headers
from starlette.testclient import TestClient
from starlette.websockets import WebSocketDisconnect

PROJECT_ROOT = Path(__file__).resolve().parent.parent

from recog.web import server  # noqa: E402


# =============================================================================
# Helpers
# =============================================================================

class FakeSubscriber:
    """Records send() calls; returns a scripted bool."""

    def __init__(self, send_result: bool = True):
        self.sent: list[str] = []
        self.send_result = send_result

    async def send(self, raw: str) -> bool:
        self.sent.append(raw)
        return self.send_result


class RecordingReply:
    """Records reply_cb invocations."""

    def __init__(self):
        self.replies: list[str] = []

    async def __call__(self, raw: str) -> None:
        self.replies.append(raw)


class FakeStorage:
    """No-active-meetings storage so the init branch never touches meetings/."""

    def list_active_meetings(self) -> list:
        return []


class FakeActiveStorage:
    """Storage with one active meeting for the `if current:` init branch."""

    MEETING = {"id": "m-active", "source": "ktalk"}

    def list_active_meetings(self) -> list:
        return [dict(self.MEETING)]

    def get_meeting(self, meeting_id: str) -> dict:
        return {"segments": []}


@pytest.fixture(autouse=True)
def reset_primer_cache():
    """Reset module globals mutated by tests around every test."""
    previous_observer = server.observer
    previous_subscriber = server.bridge_subscriber
    previous_transcript_loop = server.transcript_watcher._loop
    previous_live_loop = server.live_watcher._loop
    server._agent_state_primer = None
    yield
    server._agent_state_primer = None
    server.observer = previous_observer
    server.bridge_subscriber = previous_subscriber
    server.transcript_watcher._loop = previous_transcript_loop
    server.live_watcher._loop = previous_live_loop


def agent_state_frame(state: str = "online") -> str:
    return json.dumps({
        "type": "copilot_status",
        "status": "agent_state",
        "agent_state": state,
    })


# =============================================================================
# handle_client_frame: inbound whitelist + relay
# =============================================================================

@pytest.mark.parametrize("frame_type", [
    "copilot_chat_message",
    "copilot_spawn_request",
    "copilot_stop_request",
])
async def test_whitelisted_frame_relayed_verbatim(frame_type):
    """given: a subscriber whose send succeeds
    when: a whitelisted copilot frame arrives from the client
    then: exactly one subscriber.send with the verbatim raw string, no reply
    """
    raw = json.dumps({"type": frame_type, "request_id": "r1", "text": "hi"})
    subscriber = FakeSubscriber(send_result=True)
    reply = RecordingReply()

    await server.handle_client_frame(raw, subscriber, reply)

    assert subscriber.sent == [raw]
    assert reply.replies == []


@pytest.mark.parametrize("raw", [
    json.dumps({"type": "transcript", "text": "x"}),
    json.dumps({"type": "no_such_type"}),
])
async def test_non_whitelisted_type_dropped(raw):
    """given: a frame whose type is not in the inbound whitelist
    when: handle_client_frame processes it
    then: dropped — subscriber.send not called, no reply
    """
    subscriber = FakeSubscriber(send_result=True)
    reply = RecordingReply()

    await server.handle_client_frame(raw, subscriber, reply)

    assert subscriber.sent == []
    assert reply.replies == []


@pytest.mark.parametrize("raw", [
    "not json at all {",
    json.dumps([1, 2, 3]),
    json.dumps("just a string"),
    json.dumps({"type": 5}),
    json.dumps({"type": ["copilot_chat_message"]}),
    json.dumps({"no_type": True}),
    "[" * 2000 + "0" + "]" * 2000,
])
async def test_malformed_frame_dropped_without_exception(raw):
    """given: non-JSON raw, JSON non-dict, or a non-string type
    when: handle_client_frame processes it
    then: dropped — no relay, no reply, no exception
    """
    subscriber = FakeSubscriber(send_result=True)
    reply = RecordingReply()

    await server.handle_client_frame(raw, subscriber, reply)

    assert subscriber.sent == []
    assert reply.replies == []


# =============================================================================
# handle_client_frame: failure replies
# =============================================================================

@pytest.mark.parametrize("frame_type", [
    "copilot_chat_message",
    "copilot_spawn_request",
    "copilot_stop_request",
])
async def test_send_failure_replies_bridge_disconnected(frame_type):
    """given: a subscriber whose send returns False (bridge link down)
    when: a whitelisted frame with request_id r1 arrives
    then: reply_cb receives copilot_status bridge_disconnected echoing r1
    """
    raw = json.dumps({"type": frame_type, "request_id": "r1"})
    subscriber = FakeSubscriber(send_result=False)
    reply = RecordingReply()

    await server.handle_client_frame(raw, subscriber, reply)

    assert subscriber.sent == [raw]
    assert len(reply.replies) == 1
    assert json.loads(reply.replies[0]) == {
        "type": "copilot_status",
        "status": "bridge_disconnected",
        "request_id": "r1",
    }


async def test_no_subscriber_treated_as_send_failure():
    """given: subscriber is None (lifespan not run / shutdown)
    when: a whitelisted frame arrives
    then: same bridge_disconnected reply as a send failure
    """
    raw = json.dumps({"type": "copilot_chat_message", "request_id": "r2"})
    reply = RecordingReply()

    await server.handle_client_frame(raw, None, reply)

    assert len(reply.replies) == 1
    payload = json.loads(reply.replies[0])
    assert payload["type"] == "copilot_status"
    assert payload["status"] == "bridge_disconnected"
    assert payload["request_id"] == "r2"


async def test_missing_request_id_echoed_as_null():
    """given: a whitelisted frame without request_id and a failing send
    when: the failure reply is built
    then: the reply carries request_id null (echo of absent)
    """
    raw = json.dumps({"type": "copilot_spawn_request"})
    subscriber = FakeSubscriber(send_result=False)
    reply = RecordingReply()

    await server.handle_client_frame(raw, subscriber, reply)

    assert len(reply.replies) == 1
    payload = json.loads(reply.replies[0])
    assert payload["request_id"] is None


async def test_dead_originator_reply_exception_swallowed():
    """given: reply_cb raises on the failure reply (originator gone)
    when: handle_client_frame sends the reply
    then: relay and reply were attempted exactly once, the exception is
          swallowed, and the helper returns normally
    """
    raw = json.dumps({"type": "copilot_chat_message", "request_id": "r3"})
    subscriber = FakeSubscriber(send_result=False)
    reply_attempts: list[str] = []

    async def dead_reply(reply_raw: str) -> None:
        reply_attempts.append(reply_raw)
        raise RuntimeError("originator gone")

    await server.handle_client_frame(raw, subscriber, dead_reply)

    assert subscriber.sent == [raw]
    assert len(reply_attempts) == 1


# =============================================================================
# _on_bridge_frame: agent_state primer cache
# =============================================================================

async def test_agent_state_frame_cached(monkeypatch):
    """given: a bridge copilot_status agent_state frame
    when: it passes through the _on_bridge_frame wrapper
    then: the raw frame is cached as the primer and still forwarded
    """
    forwarded: list[str] = []

    async def record_forward(raw: str) -> None:
        forwarded.append(raw)

    monkeypatch.setattr(server, "_forward_bridge_message", record_forward)
    raw = agent_state_frame("online")

    await server._on_bridge_frame(raw)

    assert server._agent_state_primer == raw
    assert forwarded == [raw]


async def test_cache_not_clobbered_by_other_frames(monkeypatch):
    """given: the cache holds an agent_state frame
    when: copilot_reply / per-request copilot_status / transcript_refine pass
    then: the primer cache still holds the last agent_state frame
    """
    async def noop_forward(raw: str) -> None:
        pass

    monkeypatch.setattr(server, "_forward_bridge_message", noop_forward)
    primer = agent_state_frame("working")
    await server._on_bridge_frame(primer)

    for other in [
        json.dumps({"type": "copilot_reply", "request_id": "r1", "text": "hi"}),
        json.dumps({"type": "copilot_status", "status": "queued", "request_id": "r1"}),
        json.dumps({"type": "transcript_refine", "segments": []}),
    ]:
        await server._on_bridge_frame(other)

    assert server._agent_state_primer == primer


async def test_malformed_bridge_frame_leaves_cache_and_still_forwards(monkeypatch):
    """given: the wrapper receives non-JSON raw
    when: _on_bridge_frame processes it
    then: cache unchanged, frame still forwarded to _forward_bridge_message
    """
    forwarded: list[str] = []

    async def record_forward(raw: str) -> None:
        forwarded.append(raw)

    monkeypatch.setattr(server, "_forward_bridge_message", record_forward)
    primer = agent_state_frame("online")
    await server._on_bridge_frame(primer)

    await server._on_bridge_frame("garbage {not json")

    assert server._agent_state_primer == primer
    assert forwarded == [primer, "garbage {not json"]


async def test_primer_send_failure_swallowed():
    """given: a cached primer and a client that disconnects during its send
    when: _send_agent_state_primer attempts delivery
    then: the send is attempted once and the exception does not escape
    """
    class DeadWebSocket:
        def __init__(self):
            self.send_attempts = 0

        async def send_text(self, raw: str) -> None:
            self.send_attempts += 1
            raise RuntimeError("client gone")

    websocket = DeadWebSocket()
    server._agent_state_primer = agent_state_frame("online")

    await server._send_agent_state_primer(websocket)

    assert websocket.send_attempts == 1


# =============================================================================
# /api/live/stream endpoint: primer delivery + inbound routing
# =============================================================================

def test_primer_delivered_after_init(monkeypatch):
    """given: the cache holds an agent_state frame and a new WS client connects
    when: the live-stream handshake completes
    then: the client receives init first, then the cached agent_state frame
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())
    primer = agent_state_frame("online")
    server._agent_state_primer = primer

    client = TestClient(server.app)
    with client.websocket_connect("/api/live/stream") as ws:
        init = json.loads(ws.receive_text())
        assert init["type"] == "init"
        assert json.loads(ws.receive_text()) == json.loads(primer)


def test_no_primer_when_cache_empty(monkeypatch):
    """given: an empty primer cache (no agent_state seen yet)
    when: a new client connects and sends a whitelisted frame
    then: only init precedes the bridge_disconnected reply — no primer frame
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())

    client = TestClient(server.app)
    with client.websocket_connect("/api/live/stream") as ws:
        init = json.loads(ws.receive_text())
        assert init["type"] == "init"
        # No subscriber is running (lifespan not entered) so the reply to the
        # probe frame is bridge_disconnected — and it must be the very next
        # frame, proving no primer was inserted.
        ws.send_text(json.dumps({
            "type": "copilot_chat_message", "request_id": "probe",
        }))
        reply = json.loads(ws.receive_text())
        assert reply == {
            "type": "copilot_status",
            "status": "bridge_disconnected",
            "request_id": "probe",
        }


def test_acceptance_init_primer_then_bridge_disconnected(monkeypatch):
    """given: cache holds agent_state and no subscriber is running
    when: a client connects and sends a whitelisted copilot frame
    then: it observes init, then the primer, then copilot_status
          bridge_disconnected echoing its request_id
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())
    primer = agent_state_frame("online")
    server._agent_state_primer = primer

    client = TestClient(server.app)
    with client.websocket_connect("/api/live/stream") as ws:
        init = json.loads(ws.receive_text())
        assert init["type"] == "init"

        assert json.loads(ws.receive_text()) == json.loads(primer)

        ws.send_text(json.dumps({
            "type": "copilot_spawn_request", "request_id": "click-1",
        }))
        reply = json.loads(ws.receive_text())
        assert reply == {
            "type": "copilot_status",
            "status": "bridge_disconnected",
            "request_id": "click-1",
        }


def test_non_whitelisted_frame_over_endpoint_dropped(monkeypatch):
    """given: a connected live-stream client
    when: it sends a non-whitelisted frame then a whitelisted probe
    then: only the probe produces a reply (the first frame was dropped)
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())

    client = TestClient(server.app)
    with client.websocket_connect("/api/live/stream") as ws:
        assert json.loads(ws.receive_text())["type"] == "init"
        ws.send_text(json.dumps({"type": "transcript", "request_id": "x"}))
        ws.send_text(json.dumps({
            "type": "copilot_stop_request", "request_id": "probe-2",
        }))
        reply = json.loads(ws.receive_text())
        assert reply["status"] == "bridge_disconnected"
        assert reply["request_id"] == "probe-2"


def test_endpoint_relays_to_live_subscriber(monkeypatch):
    """given: a live module-global bridge_subscriber whose send succeeds
    when: a client sends a whitelisted frame over the real WS endpoint
    then: the exact raw string lands in subscriber.sent with no reply for
          it — proven by a second frame whose send is scripted to fail:
          the only reply is the bridge_disconnected echo for the SECOND
          request_id
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())

    class ScriptedSubscriber(FakeSubscriber):
        """send succeeds except for the frame marked to fail."""

        async def send(self, raw: str) -> bool:
            self.sent.append(raw)
            return json.loads(raw).get("request_id") != "fail-2"

    subscriber = ScriptedSubscriber()
    monkeypatch.setattr(server, "bridge_subscriber", subscriber)

    first = json.dumps({"type": "copilot_chat_message", "request_id": "ok-1"})
    second = json.dumps({"type": "copilot_chat_message", "request_id": "fail-2"})

    client = TestClient(server.app)
    with client.websocket_connect("/api/live/stream") as ws:
        assert json.loads(ws.receive_text())["type"] == "init"

        ws.send_text(first)
        ws.send_text(second)

        # WS frames are processed in order: receiving the reply for the
        # second frame proves the first one was already handled — and
        # produced no reply of its own.
        reply = json.loads(ws.receive_text())
        assert reply == {
            "type": "copilot_status",
            "status": "bridge_disconnected",
            "request_id": "fail-2",
        }
        assert subscriber.sent == [first, second]


def test_given_manual_spawn_when_live_stream_disconnects_then_bridge_gets_no_stop(
    monkeypatch,
):
    """given: a live-stream client wired to a recording bridge subscriber
    when: the client sends a manual spawn frame (positive control proving
          the recorder is wired) and then disconnects (F5 / tab close)
    then: subscriber.sent holds only that frame — the server-side disconnect
          path emitted nothing to the bridge (the browser half — app.js
          having no unload-time stop sender — is not covered by this
          test) (DW-14)
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())
    subscriber = FakeSubscriber(send_result=True)
    monkeypatch.setattr(server, "bridge_subscriber", subscriber)

    frame = json.dumps({"type": "copilot_spawn_request", "request_id": "pre-f5"})

    client = TestClient(server.app)
    with client.websocket_connect("/api/live/stream") as ws:
        assert json.loads(ws.receive_text())["type"] == "init"
        ws.send_text(frame)

    # Starlette's websocket_connect context exit joins the handler task, so
    # the completed disconnect path is fully observable here — the assertion
    # is deterministic, not racy.
    assert subscriber.sent == [frame]


def test_primer_delivered_after_active_meeting_init(monkeypatch):
    """given: an active meeting and a cached agent_state frame
    when: a new WS client connects to the live stream
    then: it receives the meeting init first, then the primer as the very
          next frame
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeActiveStorage())
    primer = agent_state_frame("working")
    server._agent_state_primer = primer

    client = TestClient(server.app)
    with client.websocket_connect("/api/live/stream") as ws:
        init = json.loads(ws.receive_text())
        assert init["type"] == "init"
        assert init["meeting"]["id"] == "m-active"
        assert json.loads(ws.receive_text()) == json.loads(primer)


def test_given_disallowed_origin_when_live_stream_connects_then_denied_before_registration(
    monkeypatch,
):
    """given: a WS client whose handshake carries a disallowed Origin
    when: it connects to /api/live/stream
    then: the handshake is denied before accept() and live_watcher never
          registers the socket
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())
    register = Mock(wraps=server.live_watcher.register)
    log_rejection = Mock()
    monkeypatch.setattr(server.live_watcher, "register", register)
    monkeypatch.setattr(server, "log_rate_limited", log_rejection)

    client = TestClient(server.app)
    with pytest.raises(WebSocketDisconnect):
        with client.websocket_connect(
            "/api/live/stream", headers={"Origin": "https://evil.example"}
        ):
            pass

    register.assert_not_called()
    assert log_rejection.call_args.args[0] == "frontend-ws-origin-reject"


@pytest.mark.asyncio
async def test_given_duplicate_origins_when_live_stream_handles_socket_then_rejected(
    monkeypatch,
):
    """given: a WS handshake carrying duplicate Origin headers
    when: live_stream handles the socket
    then: it closes with 1008 without registering the socket
    """
    class FakeWebSocket:
        headers = Headers(raw=[
            (b"origin", b"http://127.0.0.1:9080"),
            (b"origin", b"https://evil.example"),
        ])

        def __init__(self):
            self.close_calls = []

        async def close(self, code=1000):
            self.close_calls.append(code)

    register = Mock()
    monkeypatch.setattr(server.live_watcher, "register", register)
    websocket = FakeWebSocket()

    await server.live_stream(websocket)

    assert websocket.close_calls == [1008]
    register.assert_not_called()


def test_given_no_origin_when_live_stream_connects_then_init_received(monkeypatch):
    """given: a WS client that sends no Origin header (non-browser client)
    when: it connects to /api/live/stream
    then: behavior is unchanged — the init frame arrives
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())

    client = TestClient(server.app)
    with client.websocket_connect("/api/live/stream") as ws:
        assert json.loads(ws.receive_text())["type"] == "init"


def test_given_webapp_origin_when_live_stream_connects_then_init_received(monkeypatch):
    """given: a WS client whose handshake carries the webapp origin
    when: it connects to /api/live/stream
    then: the handshake is accepted and the init frame arrives
    """
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())

    client = TestClient(server.app)
    with client.websocket_connect(
        "/api/live/stream", headers={"Origin": "http://127.0.0.1:9080"}
    ) as ws:
        assert json.loads(ws.receive_text())["type"] == "init"


async def test_lifespan_wires_bridge_frames_bidirectionally(monkeypatch, tmp_path):
    """given: the frontend lifespan constructs its bridge subscriber
    when: bridge and client frames cross the real live-stream endpoint
    then: the primer reaches the client and client frames reach that subscriber
    """
    class FakeObserver:
        def schedule(self, *args, **kwargs):
            pass

        def start(self):
            pass

        def stop(self):
            pass

        def join(self):
            pass

    class FakeBridgeSubscriber:
        instance = None

        def __init__(self, bridge_url, forward_callback, **_callbacks):
            self.forward_callback = forward_callback
            self.sent = []
            FakeBridgeSubscriber.instance = self

        async def start(self):
            pass

        async def stop(self):
            pass

        async def send(self, raw):
            self.sent.append(raw)
            return json.loads(raw).get("request_id") != "fail-2"

    monkeypatch.setattr(server, "Observer", FakeObserver)
    monkeypatch.setattr(server, "BridgeSubscriber", FakeBridgeSubscriber)
    monkeypatch.setattr(server, "MEETINGS_DIR", tmp_path)
    monkeypatch.setattr(server, "get_storage", lambda: FakeStorage())

    async with server.lifespan(server.app):
        primer = agent_state_frame("online")
        await FakeBridgeSubscriber.instance.forward_callback(primer)

        client = TestClient(server.app)
        with client.websocket_connect("/api/live/stream") as ws:
            assert json.loads(ws.receive_text())["type"] == "init"
            assert json.loads(ws.receive_text()) == json.loads(primer)

            first = json.dumps({
                "type": "copilot_chat_message",
                "request_id": "ok-1",
            })
            second = json.dumps({
                "type": "copilot_chat_message",
                "request_id": "fail-2",
            })
            ws.send_text(first)
            ws.send_text(second)

            assert json.loads(ws.receive_text()) == {
                "type": "copilot_status",
                "status": "bridge_disconnected",
                "request_id": "fail-2",
            }
            assert FakeBridgeSubscriber.instance.sent == [first, second]
