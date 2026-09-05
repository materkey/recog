"""Tests for recog/web/bridge_subscriber.py — bidirectional WS client to the bridge."""
import asyncio
import json
import sys
from pathlib import Path

import pytest
import websockets

PROJECT_ROOT = Path(__file__).resolve().parent.parent

from recog.web.bridge_subscriber import BridgeSubscriber


async def _start_mock_bridge(handler):
    """Start a websockets server on an ephemeral port and return (server, port)."""
    server = await websockets.serve(handler, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    return server, port


@pytest.mark.asyncio
async def test_subscriber_forwards_transcript_refine_only():
    """given: mock bridge sends a transcript_refine and a transcript frame
    when: BridgeSubscriber processes the stream
    then: only transcript_refine is forwarded; transcript is dropped
    """
    forwarded: list[str] = []

    async def forward(message: str) -> None:
        forwarded.append(message)

    refine_payload = {
        "type": "transcript_refine",
        "updates": [{"segment_id": 7, "text": "уточнённый"}],
        "refined_at": "2026-04-24T12:00:00",
    }

    async def handler(websocket):
        await websocket.send(json.dumps(refine_payload, ensure_ascii=False))
        await websocket.send(json.dumps({"type": "transcript", "speaker": "X", "text": "y"}))
        await asyncio.sleep(0.5)

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    await sub.start()

    try:
        for _ in range(40):
            if forwarded:
                break
            await asyncio.sleep(0.05)
        assert len(forwarded) == 1
        relayed = json.loads(forwarded[0])
        assert relayed == refine_payload
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_subscriber_reconnects_after_bridge_drop():
    """given: bridge accepts a connection, drops, then accepts again
    when: BridgeSubscriber is running
    then: it reconnects and continues forwarding refine events
    """
    forwarded: list[str] = []

    async def forward(message: str) -> None:
        forwarded.append(message)

    connection_count = 0

    async def handler(websocket):
        nonlocal connection_count
        connection_count += 1
        if connection_count == 1:
            # Drop immediately after a hello-style frame to force reconnect.
            await websocket.send(json.dumps({"type": "hello"}))
            await websocket.close()
            return
        # Second connection: deliver the refine frame.
        await websocket.send(json.dumps({
            "type": "transcript_refine",
            "updates": [{"segment_id": 1, "text": "после reconnect"}],
            "refined_at": "2026-04-24T12:00:01",
        }, ensure_ascii=False))
        await asyncio.sleep(0.5)

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    sub.INITIAL_BACKOFF_S = 0.05  # speed up the test
    await sub.start()

    try:
        for _ in range(120):
            if forwarded:
                break
            await asyncio.sleep(0.05)
        assert len(forwarded) == 1
        assert json.loads(forwarded[0])["updates"][0]["segment_id"] == 1
        assert connection_count >= 2
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_subscriber_starts_when_bridge_unavailable():
    """given: no bridge listening on the URL
    when: BridgeSubscriber.start() is called
    then: it does not raise; the run loop retries silently in the background
    """
    forwarded: list[str] = []

    async def forward(message: str) -> None:
        forwarded.append(message)

    sub = BridgeSubscriber(
        bridge_url="ws://127.0.0.1:1",  # unlikely to listen
        forward_callback=forward,
    )
    sub.INITIAL_BACKOFF_S = 0.05

    await sub.start()
    try:
        # Give the loop a chance to attempt and fail at least once.
        await asyncio.sleep(0.2)
        assert forwarded == []
        # Confirm the task is still running (didn't crash on first failure).
        assert sub._task is not None
        assert not sub._task.done()
    finally:
        await sub.stop()


@pytest.mark.asyncio
async def test_subscriber_sends_subscriber_hello_on_connect():
    """given: mock bridge receives connections
    when: BridgeSubscriber connects
    then: the first frame it sends is `{"type": "subscriber_hello"}` so the
          bridge can move the connection out of its extension-client set
          (preserving meeting-lifecycle semantics)
    """
    received: list[str] = []

    async def handler(websocket):
        try:
            first = await asyncio.wait_for(websocket.recv(), timeout=1.0)
            received.append(first)
        except Exception:
            pass
        # Keep the connection alive briefly so the subscriber doesn't
        # immediately reconnect and race the assert.
        await asyncio.sleep(0.2)

    async def forward(message: str) -> None:
        pass

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    await sub.start()

    try:
        for _ in range(20):
            if received:
                break
            await asyncio.sleep(0.05)
        assert received, "subscriber did not send any frame"
        payload = json.loads(received[0])
        assert payload == {"type": "subscriber_hello"}
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_subscriber_drops_malformed_frames():
    """given: bridge sends a non-JSON frame followed by a valid refine
    when: subscriber processes the stream
    then: malformed frame is silently dropped; valid frame is forwarded
    """
    forwarded: list[str] = []

    async def forward(message: str) -> None:
        forwarded.append(message)

    async def handler(websocket):
        await websocket.send("not-json")
        await websocket.send(json.dumps({
            "type": "transcript_refine",
            "updates": [{"segment_id": 9, "text": "ok"}],
            "refined_at": "2026-04-24T12:00:02",
        }))
        await asyncio.sleep(0.5)

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    await sub.start()

    try:
        for _ in range(40):
            if forwarded:
                break
            await asyncio.sleep(0.05)
        assert len(forwarded) == 1
        assert json.loads(forwarded[0])["updates"][0]["segment_id"] == 9
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


# ---------------------------------------------------------------------------
# Bidirectional group: FORWARD_TYPES widening + best-effort send()
# ---------------------------------------------------------------------------


async def _wait_for(predicate, attempts: int = 120, interval: float = 0.05) -> bool:
    """Poll `predicate()` until true or the attempt budget runs out."""
    for _ in range(attempts):
        if predicate():
            return True
        await asyncio.sleep(interval)
    return predicate()


@pytest.mark.asyncio
async def test_subscriber_forwards_copilot_reply_and_drops_unknown():
    """given: connected bridge sends copilot_reply plus an unknown-type frame
    when: BridgeSubscriber processes the stream
    then: copilot_reply is forwarded verbatim; the unknown type is dropped
    """
    forwarded: list[str] = []

    async def forward(message: str) -> None:
        forwarded.append(message)

    reply_payload = {
        "type": "copilot_reply",
        "request_id": "req-1",
        "text": "ответ копайлота",
    }

    async def handler(websocket):
        await websocket.send(json.dumps({"type": "totally_unknown", "x": 1}))
        await websocket.send(json.dumps(reply_payload, ensure_ascii=False))
        await asyncio.sleep(0.5)

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    await sub.start()

    try:
        await _wait_for(lambda: forwarded)
        assert len(forwarded) == 1
        assert json.loads(forwarded[0]) == reply_payload
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_subscriber_forwards_copilot_status_variants():
    """given: connected bridge sends several copilot_status variants
    when: BridgeSubscriber processes the stream
    then: every variant is forwarded to the callback (no per-variant filtering)
    """
    forwarded: list[str] = []

    async def forward(message: str) -> None:
        forwarded.append(message)

    status_payloads = [
        {"type": "copilot_status", "request_id": "req-2", "status": "working"},
        {
            "type": "copilot_status",
            "request_id": "",
            "status": "agent_state",
            "agent_state": "online",
        },
        {"type": "copilot_status", "request_id": "", "status": "channel_connected"},
    ]

    async def handler(websocket):
        for payload in status_payloads:
            await websocket.send(json.dumps(payload, ensure_ascii=False))
        await asyncio.sleep(0.5)

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    await sub.start()

    try:
        await _wait_for(lambda: len(forwarded) >= len(status_payloads))
        assert [json.loads(f) for f in forwarded] == status_payloads
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_send_delivers_frame_to_connected_bridge():
    """given: subscriber connected to a mock bridge
    when: await send(raw) is called
    then: the bridge socket receives raw verbatim (past subscriber_hello) and
          send returns True
    """
    received: list[str] = []

    async def handler(websocket):
        try:
            first = await asyncio.wait_for(websocket.recv(), timeout=2.0)
            assert json.loads(first) == {"type": "subscriber_hello"}
            nxt = await asyncio.wait_for(websocket.recv(), timeout=2.0)
            received.append(nxt)
        except Exception:
            pass
        await asyncio.sleep(0.2)

    async def forward(message: str) -> None:
        pass

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    await sub.start()

    try:
        assert await _wait_for(lambda: sub._ws is not None)
        raw = json.dumps({"type": "copilot_chat_message", "request_id": "r1", "text": "hi"})
        ok = await sub.send(raw)
        assert ok is True
        assert await _wait_for(lambda: received)
        assert received[0] == raw
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_send_waits_for_subscriber_hello_before_connection_is_published(monkeypatch):
    """given: the subscriber_hello send is blocked on a new connection
    when: send(raw) is awaited before the hello completes
    then: it returns False; after hello completes, later sends succeed and
          subscriber_hello remains the first frame
    """

    class _ControlledWS:
        def __init__(self):
            self.hello_started = asyncio.Event()
            self.release_hello = asyncio.Event()
            self.keep_open = asyncio.Event()
            self.sent: list[str] = []

        async def send(self, raw):
            if json.loads(raw).get("type") == "subscriber_hello":
                self.hello_started.set()
                await self.release_hello.wait()
            self.sent.append(raw)

        def __aiter__(self):
            return self

        async def __anext__(self):
            await self.keep_open.wait()
            raise StopAsyncIteration

    class _ConnectContext:
        async def __aenter__(self):
            return controlled_ws

        async def __aexit__(self, exc_type, exc, traceback):
            return False

    async def forward(message: str) -> None:
        pass

    controlled_ws = _ControlledWS()
    monkeypatch.setattr(websockets, "connect", lambda *args, **kwargs: _ConnectContext())
    sub = BridgeSubscriber(
        bridge_url="ws://127.0.0.1:1",
        forward_callback=forward,
    )
    await sub.start()

    try:
        await asyncio.wait_for(controlled_ws.hello_started.wait(), timeout=1.0)
        raw = json.dumps({"type": "copilot_chat_message", "request_id": "too-early"})
        assert await sub.send(raw) is False
        assert controlled_ws.sent == []

        controlled_ws.release_hello.set()
        assert await _wait_for(lambda: sub._ws is controlled_ws)
        sentinel = json.dumps({"type": "copilot_chat_message", "request_id": "after-hello"})
        assert await sub.send(sentinel) is True
        assert [json.loads(frame)["type"] for frame in controlled_ws.sent] == [
            "subscriber_hello",
            "copilot_chat_message",
        ]
    finally:
        await sub.stop()


@pytest.mark.asyncio
async def test_send_before_start_returns_false():
    """given: a BridgeSubscriber that was never started
    when: send(raw) is awaited
    then: it returns False immediately without raising
    """

    async def forward(message: str) -> None:
        pass

    sub = BridgeSubscriber(
        bridge_url="ws://127.0.0.1:1",
        forward_callback=forward,
    )
    assert await sub.send('{"type": "copilot_chat_message"}') is False


@pytest.mark.asyncio
async def test_send_while_bridge_unavailable_returns_false():
    """given: subscriber started against a dead bridge URL
    when: send(raw) is awaited while the run loop retries in the background
    then: it returns False immediately without raising
    """

    async def forward(message: str) -> None:
        pass

    sub = BridgeSubscriber(
        bridge_url="ws://127.0.0.1:1",  # unlikely to listen
        forward_callback=forward,
    )
    sub.INITIAL_BACKOFF_S = 0.05
    await sub.start()
    try:
        await asyncio.sleep(0.2)
        assert await sub.send('{"type": "copilot_spawn_request"}') is False
        # Run loop must still be alive despite the failed send.
        assert sub._task is not None
        assert not sub._task.done()
    finally:
        await sub.stop()


@pytest.mark.asyncio
async def test_send_during_backoff_fails_fast_and_never_queues():
    """given: the bridge dropped and the subscriber is waiting to reconnect
    when: send(raw) is awaited during the backoff window
    then: it returns False; after the bridge comes back and the subscriber
          reconnects, a sentinel sent post-reconnect is the only frame the
          bridge ever sees (no queueing across reconnects) and the reconnect
          cycle is unperturbed
    """
    received_frames: list[str] = []

    async def handler(websocket):
        try:
            async for msg in websocket:
                if json.loads(msg).get("type") == "subscriber_hello":
                    continue
                received_frames.append(msg)
        except Exception:
            pass

    async def forward(message: str) -> None:
        pass

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    sub.INITIAL_BACKOFF_S = 0.05
    await sub.start()

    try:
        assert await _wait_for(lambda: sub._ws is not None)

        # Drop the bridge: closes the listener AND the live connection.
        server.close()
        await server.wait_closed()
        assert await _wait_for(lambda: sub._ws is None)

        raw = json.dumps({"type": "copilot_chat_message", "request_id": "lost"})
        assert await sub.send(raw) is False

        # Restore the bridge on the same port; the subscriber must reconnect.
        server = await websockets.serve(handler, "127.0.0.1", port)
        assert await _wait_for(lambda: sub._ws is not None)

        # Ordering proof: a sentinel sent after the reconnect must be the
        # only frame the bridge ever sees — a queued late flush of the failed
        # frame would land ahead of it in received_frames.
        sentinel = json.dumps({"type": "copilot_chat_message", "request_id": "sentinel"})
        assert await sub.send(sentinel) is True
        assert await _wait_for(lambda: received_frames)
        assert received_frames == [sentinel]
        # And nothing trails in behind the sentinel either.
        await asyncio.sleep(0.2)
        assert received_frames == [sentinel]
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_send_failure_does_not_perturb_live_run_loop():
    """given: a running subscriber connected to a live bridge, with the ws
           attribute shadowed by a stub whose send raises
    when: send(raw) fails on the stub
    then: it returns False; the run loop task is still alive and a subsequent
          frame from the bridge (real ws path restored) is still forwarded
    """
    forwarded: list[str] = []
    send_follow_up = asyncio.Event()

    async def forward(message: str) -> None:
        forwarded.append(message)

    followup_payload = {
        "type": "transcript_refine",
        "updates": [{"segment_id": 3, "text": "после сбоя send"}],
        "refined_at": "2026-07-25T12:00:00",
    }

    async def handler(websocket):
        try:
            await asyncio.wait_for(send_follow_up.wait(), timeout=5.0)
            await websocket.send(json.dumps(followup_payload, ensure_ascii=False))
            await asyncio.sleep(0.5)
        except Exception:
            pass

    class _DeadWS:
        async def send(self, raw):
            raise ConnectionError("socket just closed")

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    await sub.start()

    try:
        assert await _wait_for(lambda: sub._ws is not None)
        real_ws = sub._ws
        sub._ws = _DeadWS()
        assert await sub.send('{"type": "copilot_chat_message"}') is False
        # Restore the real connection path and prove the loop is unperturbed:
        # the task must still be running and the recv loop still forwarding.
        sub._ws = real_ws
        assert sub._task is not None
        assert not sub._task.done()
        send_follow_up.set()
        assert await _wait_for(lambda: forwarded)
        assert json.loads(forwarded[0]) == followup_payload
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_subscriber_drops_unhashable_type_and_keeps_connection():
    """given: bridge sends a frame whose type is an unhashable list, then a refine
    when: subscriber processes the stream
    then: the bad frame is dropped without killing the recv loop; the refine on
          the SAME connection is still forwarded (no reconnect happened)
    """
    forwarded: list[str] = []
    connection_count = 0

    async def forward(message: str) -> None:
        forwarded.append(message)

    async def handler(websocket):
        nonlocal connection_count
        connection_count += 1
        await websocket.send(json.dumps({"type": ["not", "hashable"]}))
        await websocket.send(json.dumps({
            "type": "transcript_refine",
            "updates": [{"segment_id": 11, "text": "после мусорного type"}],
            "refined_at": "2026-07-25T12:00:01",
        }, ensure_ascii=False))
        await asyncio.sleep(0.5)

    server, port = await _start_mock_bridge(handler)
    sub = BridgeSubscriber(
        bridge_url=f"ws://127.0.0.1:{port}",
        forward_callback=forward,
    )
    await sub.start()

    try:
        assert await _wait_for(lambda: forwarded)
        assert len(forwarded) == 1
        assert json.loads(forwarded[0])["updates"][0]["segment_id"] == 11
        assert connection_count == 1
    finally:
        await sub.stop()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_send_mid_death_race_returns_false(capsys):
    """given: the ws attribute is still set but the socket already died
    when: send(raw) is awaited and the underlying send raises
    then: the exception is caught, send returns False, and the failure is
          logged to stderr
    """

    class _DeadWS:
        async def send(self, raw):
            raise ConnectionError("socket just closed")

    async def forward(message: str) -> None:
        pass

    sub = BridgeSubscriber(
        bridge_url="ws://127.0.0.1:1",
        forward_callback=forward,
    )
    sub._ws = _DeadWS()

    assert await sub.send('{"type": "copilot_chat_message"}') is False
    captured = capsys.readouterr()
    assert "send failed" in captured.err
