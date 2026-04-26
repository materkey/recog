"""Tests for recog/copilot_channel.py.

Channel-server smoke (`tasks.md` 8.5). Spins up an in-memory fake bridge
WS server with `websockets.serve` on a random port, points the channel
server's `_ws_loop` at it, and exercises the MCP tools through the public
`FastMCP.call_tool` API. The MCP HTTP loop is intentionally not booted —
per the plan, "If the channel server's MCP HTTP loop interferes, gate it
behind a flag and only run the WS-client loop in the test."
"""
from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

import pytest
import websockets

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.copilot_channel import BridgeLink, _build_mcp, _ws_loop


class FakeBridgeServer:
    """Async context manager wrapping `websockets.serve` to capture frames.

    The handler decodes JSON, pushes each frame to `received` (a Queue) and
    optionally invokes `auto_responder` to send a frame back over the same
    socket — that is how `get_recent_transcript_response` is delivered.
    """

    def __init__(self, auto_responder=None):
        self.received: asyncio.Queue[dict] = asyncio.Queue()
        self.connected_event = asyncio.Event()
        self.current_ws = None
        self._auto_responder = auto_responder
        self._server = None
        self.url: str = ""

    async def _handler(self, ws):
        self.current_ws = ws
        self.connected_event.set()
        try:
            async for raw in ws:
                try:
                    data = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                await self.received.put(data)
                if self._auto_responder is not None:
                    response = self._auto_responder(data)
                    if response is not None:
                        await ws.send(json.dumps(response))
        except websockets.ConnectionClosed:
            pass

    async def __aenter__(self) -> "FakeBridgeServer":
        self._server = await websockets.serve(self._handler, "127.0.0.1", 0)
        port = self._server.sockets[0].getsockname()[1]
        self.url = f"ws://127.0.0.1:{port}"
        return self

    async def __aexit__(self, *_exc) -> None:
        assert self._server is not None
        self._server.close()
        await self._server.wait_closed()


async def _start_link(bridge_url: str):
    """Start a BridgeLink + `_ws_loop` pair and a fresh FastMCP wrapper."""
    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    stop = asyncio.Event()
    task = asyncio.create_task(_ws_loop(bridge_url, link, stop))
    return link, mcp, stop, task


async def _stop_link(stop: asyncio.Event, task: asyncio.Task) -> None:
    stop.set()
    try:
        await asyncio.wait_for(task, timeout=2.0)
    except asyncio.TimeoutError:
        task.cancel()
        try:
            await task
        except BaseException:
            pass


async def _wait_connected(link: BridgeLink, timeout: float = 2.0) -> None:
    loop = asyncio.get_event_loop()
    deadline = loop.time() + timeout
    while not link.is_connected():
        if loop.time() >= deadline:
            raise AssertionError("BridgeLink failed to connect to fake bridge")
        await asyncio.sleep(0.02)


def _parse_tool_result(result) -> dict:
    """FastMCP.call_tool returns a Sequence[ContentBlock]; the first block
    holds the JSON-serialised tool return value as text."""
    assert result, "expected at least one ContentBlock"
    return json.loads(result[0].text)


async def test_channel_server_sends_hello_on_connect():
    """
    given: a fake bridge WS server bound to a random port
    when:  the channel server's `_ws_loop` connects to it
    then:  the very first frame received by the bridge is
           `{"type": "copilot_channel_hello"}` (mirrors the real bridge's
           handshake contract — see `_handle_inbound` in
           ktalk_bridge.py:2017).
    """
    async with FakeBridgeServer() as bridge:
        link, _mcp, stop, task = await _start_link(bridge.url)
        try:
            await asyncio.wait_for(bridge.connected_event.wait(), timeout=2.0)
            first = await asyncio.wait_for(bridge.received.get(), timeout=2.0)
            assert first == {"type": "copilot_channel_hello"}
            await _wait_connected(link)
        finally:
            await _stop_link(stop, task)


async def test_send_copilot_message_tool_emits_channel_reply():
    """
    given: the channel server is connected to a fake bridge (hello already
           consumed) and a Claude-style MCP caller invokes
           `send_copilot_message` with kind="manual", text, request_id,
           meeting_id
    when:  the tool body runs through `BridgeLink.send`
    then:  the bridge receives a single `copilot_channel_reply` frame
           preserving every supplied field plus a numeric `created_at`
           (the bridge contract requires it, see ktalk_bridge.py:2178).
    """
    async with FakeBridgeServer() as bridge:
        link, mcp, stop, task = await _start_link(bridge.url)
        try:
            await asyncio.wait_for(bridge.connected_event.wait(), timeout=2.0)
            hello = await asyncio.wait_for(bridge.received.get(), timeout=2.0)
            assert hello["type"] == "copilot_channel_hello"
            await _wait_connected(link)

            tool_result = await mcp.call_tool(
                "send_copilot_message",
                {
                    "kind": "manual",
                    "text": "Алиса спросила про метрики",
                    "request_id": "req-test-42",
                    "meeting_id": "2026-04-26_meet",
                },
            )
            assert _parse_tool_result(tool_result) == {"ok": True}

            reply = await asyncio.wait_for(bridge.received.get(), timeout=2.0)
            assert reply["type"] == "copilot_channel_reply"
            assert reply["request_id"] == "req-test-42"
            assert reply["kind"] == "manual"
            assert reply["text"] == "Алиса спросила про метрики"
            assert reply["meeting_id"] == "2026-04-26_meet"
            assert isinstance(reply["created_at"], (int, float))
        finally:
            await _stop_link(stop, task)


async def test_get_recent_transcript_round_trips_through_bridge():
    """
    given: a fake bridge that, on `get_recent_transcript_request`,
           echoes back a `get_recent_transcript_response` carrying the
           same `request_id` and a one-segment payload
    when:  the MCP `get_recent_transcript` tool is invoked with the
           default args
    then:  the tool resolves with `{"ok": True, "segments": [...]}` —
           proving the future-correlator path in `_handle_inbound` ↔
           `BridgeLink.resolve_pending` works end-to-end over a real WS.
    """
    sample_segment = {
        "segment_id": 5,
        "speaker": "Alice",
        "text": "что только что сказала Алиса?",
        "start_ms": 1000,
        "end_ms": 2000,
        "refined": False,
        "meeting_id": "2026-04-26_meet",
    }

    def auto_respond(frame: dict):
        if frame.get("type") != "get_recent_transcript_request":
            return None
        return {
            "type": "get_recent_transcript_response",
            "request_id": frame["request_id"],
            "segments": [sample_segment],
        }

    async with FakeBridgeServer(auto_responder=auto_respond) as bridge:
        link, mcp, stop, task = await _start_link(bridge.url)
        try:
            await asyncio.wait_for(bridge.connected_event.wait(), timeout=2.0)
            hello = await asyncio.wait_for(bridge.received.get(), timeout=2.0)
            assert hello["type"] == "copilot_channel_hello"
            await _wait_connected(link)

            tool_result = await mcp.call_tool("get_recent_transcript", {})
            payload = _parse_tool_result(tool_result)
            assert payload["ok"] is True
            assert payload["segments"] == [sample_segment]

            request_frame = await asyncio.wait_for(
                bridge.received.get(), timeout=2.0
            )
            assert request_frame["type"] == "get_recent_transcript_request"
            assert request_frame["window_seconds"] == 180
            assert request_frame["since_segment_id"] is None
            assert request_frame["include_refined"] is True
            assert isinstance(request_frame["request_id"], str)
            assert request_frame["request_id"].startswith("snapshot_")
        finally:
            await _stop_link(stop, task)
