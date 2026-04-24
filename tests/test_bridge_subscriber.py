"""Tests for frontend/bridge_subscriber.py — WS client that forwards refine events."""
import asyncio
import json
import sys
from pathlib import Path

import pytest
import websockets

PROJECT_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_ROOT / "frontend"))

from bridge_subscriber import BridgeSubscriber


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
