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

from recog.copilot_channel import (
    BridgeLink,
    SessionCountReporter,
    _build_mcp,
    _ws_loop,
)


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


async def test_get_recent_transcript_cleans_pending_on_cancellation():
    """
    given: the channel server is connected to a fake bridge and `link.send`
           is patched to block (simulating an in-flight WS write that hasn't
           yet flushed to the wire)
    when:  the `get_recent_transcript` tool task is cancelled mid-send
           (mirrors a real MCP HTTP client closing while Claude awaits the
           snapshot)
    then:  `BridgeLink._pending` is empty after cancellation — the
           `register_pending` entry must be removed even though the
           CancelledError unwound the stack before the inner try/except
           blocks ran. Without the wrap-everything try/finally, the future
           would stay pinned until the next `set_socket(None)` reconnect.
    """
    async with FakeBridgeServer() as bridge:
        link, mcp, stop, task = await _start_link(bridge.url)
        try:
            await asyncio.wait_for(bridge.connected_event.wait(), timeout=2.0)
            hello = await asyncio.wait_for(bridge.received.get(), timeout=2.0)
            assert hello["type"] == "copilot_channel_hello"
            await _wait_connected(link)

            send_started = asyncio.Event()

            async def slow_send(payload):
                send_started.set()
                await asyncio.sleep(60)
                return {"ok": True}

            link.send = slow_send  # type: ignore[assignment]

            assert link._pending == {}

            tool_task = asyncio.create_task(
                mcp.call_tool("get_recent_transcript", {})
            )
            await asyncio.wait_for(send_started.wait(), timeout=2.0)
            assert len(link._pending) == 1

            tool_task.cancel()
            with pytest.raises(BaseException):
                await tool_task

            assert link._pending == {}
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


async def test_await_copilot_event_returns_enqueued_bridge_event():
    """
    given: a connected channel server and a fake bridge that pushes a
           copilot_user_message frame after the hello handshake
    when:  Claude calls await_copilot_event(timeout_seconds=2)
    then:  the tool returns {"ok": True, "event": <frame>} with the
           original payload — proves the bridge → Claude wake-up path
           works via the long-poll queue (`_handle_inbound` enqueue + tool
           drain).
    """
    user_message = {
        "type": "copilot_user_message",
        "request_id": "ext-req-99",
        "text": "что нового?",
        "meeting_id": "smoke-meet",
        "meeting_title": "Smoke",
        "transcript_window": "[14:20] X: ...",
        "created_at": 1.0,
    }

    def push_after_hello(frame):
        if frame.get("type") == "copilot_channel_hello":
            return user_message
        return None

    async with FakeBridgeServer(auto_responder=push_after_hello) as bridge:
        link, mcp, stop, task = await _start_link(bridge.url)
        try:
            await asyncio.wait_for(bridge.connected_event.wait(), timeout=2.0)
            hello = await asyncio.wait_for(bridge.received.get(), timeout=2.0)
            assert hello["type"] == "copilot_channel_hello"
            await _wait_connected(link)

            tool_result = await mcp.call_tool(
                "await_copilot_event", {"timeout_seconds": 2}
            )
            payload = _parse_tool_result(tool_result)
            assert payload["ok"] is True
            assert payload["event"] == user_message
        finally:
            await _stop_link(stop, task)


async def test_await_copilot_event_returns_null_on_timeout():
    """
    given: a connected channel server with no events queued
    when:  Claude calls await_copilot_event(timeout_seconds=1)
    then:  the tool resolves with {"ok": True, "event": null} after
           ~1 s — the contract that signals Claude to either re-poll
           or exit the loop after a sustained idle period.
    """
    async with FakeBridgeServer() as bridge:
        link, mcp, stop, task = await _start_link(bridge.url)
        try:
            await asyncio.wait_for(bridge.connected_event.wait(), timeout=2.0)
            hello = await asyncio.wait_for(bridge.received.get(), timeout=2.0)
            assert hello["type"] == "copilot_channel_hello"
            await _wait_connected(link)

            tool_result = await mcp.call_tool(
                "await_copilot_event", {"timeout_seconds": 1}
            )
            payload = _parse_tool_result(tool_result)
            assert payload == {"ok": True, "event": None}
        finally:
            await _stop_link(stop, task)


async def test_await_copilot_event_rejects_non_positive_timeout():
    """
    given: a connected channel server
    when:  Claude calls await_copilot_event with timeout_seconds=0
    then:  the tool returns a structured error rather than blocking
           forever — guards against accidental infinite holds.
    """
    async with FakeBridgeServer() as bridge:
        link, mcp, stop, task = await _start_link(bridge.url)
        try:
            await asyncio.wait_for(bridge.connected_event.wait(), timeout=2.0)
            hello = await asyncio.wait_for(bridge.received.get(), timeout=2.0)
            assert hello["type"] == "copilot_channel_hello"
            await _wait_connected(link)

            tool_result = await mcp.call_tool(
                "await_copilot_event", {"timeout_seconds": 0}
            )
            payload = _parse_tool_result(tool_result)
            assert payload["ok"] is False
            assert "positive int" in payload["error"]
        finally:
            await _stop_link(stop, task)


async def test_event_queue_drops_oldest_on_overflow():
    """
    given: a BridgeLink with a tiny queue (saturated by direct enqueue
           bypassing the WS path so no fake bridge is required)
    when:  one more event arrives via `_handle_inbound`
    then:  the oldest event is evicted and the newest is preserved —
           guarantees freshest meeting state under sustained pressure.
    """
    from recog.copilot_channel import _handle_inbound  # local import to avoid

    link = BridgeLink()
    # Replace the production queue with a tiny one for this test only.
    link.event_queue = asyncio.Queue(maxsize=2)
    for i in range(2):
        link.event_queue.put_nowait({"type": "transcript_tick", "n": i})

    overflow = json.dumps({"type": "transcript_tick", "n": 99})
    await _handle_inbound(overflow, link)

    drained = []
    while not link.event_queue.empty():
        drained.append(link.event_queue.get_nowait())
    # Oldest (n=0) was evicted; n=1 stayed; n=99 appended at tail.
    assert [e["n"] for e in drained] == [1, 99]


async def test_channel_capability_present_in_initialization_options():
    """
    given: a freshly-built FastMCP instance via `_build_mcp`
    when:  the lowlevel server's `create_initialization_options` is called
           (the same code path the SDK uses on `initialize` requests)
    then:  the resulting capabilities include
           `experimental['claude/channel'] = {}` — the only knob Claude
           Code's gateChannelServer inspects to register the channel
           notification handler.
    """
    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    init_opts = mcp._mcp_server.create_initialization_options()
    caps = init_opts.capabilities
    experimental = caps.experimental
    assert experimental is not None, "experimental capabilities missing"
    assert "claude/channel" in experimental, f"got {experimental!r}"
    assert experimental["claude/channel"] == {}


async def test_format_channel_event_for_user_message():
    from recog.copilot_channel import _format_channel_event

    content, meta = _format_channel_event({
        "type": "copilot_user_message",
        "request_id": "req-7",
        "text": "что в репо?",
        "meeting_id": "meet-1",
        "meeting_title": "Daily",
    })
    assert "что в репо?" in content
    assert "Reply with send_copilot_message" in content
    assert meta == {
        "event_type": "copilot_user_message",
        "request_id": "req-7",
        "meeting_id": "meet-1",
        "meeting_title": "Daily",
    }


async def test_format_channel_event_for_transcript_tick():
    from recog.copilot_channel import _format_channel_event

    content, meta = _format_channel_event({
        "type": "transcript_tick",
        "meeting_id": "meet-2",
        "new_segments": [{"id": 1}, {"id": 2}],
        "recent_context": "[14:20] X: hi",
        "local_user_name": "Слава",
    })
    assert "Transcript tick: 2 new segment(s)" in content
    assert "[14:20] X: hi" in content
    assert meta["event_type"] == "transcript_tick"
    assert meta["meeting_id"] == "meet-2"
    assert meta["local_user_name"] == "Слава"


async def test_format_channel_event_drops_meta_keys_with_invalid_chars():
    from recog.copilot_channel import _format_channel_event

    # Insert a malformed key into a valid event; it must be filtered, not
    # passed through (Claude Code's notification format requires alphanumeric
    # underscores only — non-conforming keys are silently dropped client-side).
    content, meta = _format_channel_event({
        "type": "meeting_started",
        "meeting_id": "abc",
        "meeting-with-hyphen": "should drop",
    })
    assert "abc" not in content  # content for meeting_started is the title hint
    assert "meeting-with-hyphen" not in meta
    assert meta["event_type"] == "meeting_started"
    assert meta["meeting_id"] == "abc"


async def test_broadcast_channel_notification_writes_to_active_transports():
    """
    given: a FastMCP whose session_manager has two fake transports, each
           with a recording write_stream (the same surface real
           `StreamableHTTPServerTransport._write_stream` exposes — an
           anyio MemoryObjectSendStream)
    when:  `_broadcast_channel_notification` is invoked with content + meta
    then:  both transports receive a SessionMessage carrying a
           `notifications/claude/channel` JSON-RPC notification with the
           supplied payload, and the function returns 2 (sessions
           successfully written).
    """
    from recog.copilot_channel import _broadcast_channel_notification

    class RecordingStream:
        def __init__(self):
            self.sent = []

        async def send(self, msg):
            self.sent.append(msg)

    class FakeTransport:
        def __init__(self):
            self._write_stream = RecordingStream()

    class FakeSessionManager:
        def __init__(self):
            self._server_instances = {"a": FakeTransport(), "b": FakeTransport()}

    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    # Override session_manager — FastMCP's real one only exists once
    # streamable_http is running, but the broadcast helper only reads
    # `session_manager._server_instances`, so a stand-in suffices.
    mcp._session_manager = FakeSessionManager()  # type: ignore[attr-defined]

    sent_count = await _broadcast_channel_notification(
        mcp, "test content", {"event_type": "transcript_tick"}
    )
    assert sent_count == 2
    for transport in mcp._session_manager._server_instances.values():
        assert len(transport._write_stream.sent) == 1
        msg = transport._write_stream.sent[0]
        # SessionMessage wraps a JSONRPCMessage; the inner notification's
        # method/params are what Claude Code's listener reads.
        notification = msg.message.root
        assert notification.method == "notifications/claude/channel"
        params = notification.params
        # JSONRPCNotification.params can be a dict or pydantic model
        if hasattr(params, "model_dump"):
            params = params.model_dump()
        assert params["content"] == "test content"
        assert params["meta"] == {"event_type": "transcript_tick"}


async def test_broadcast_channel_no_op_without_active_sessions():
    """
    given: a FastMCP whose session_manager has zero active transports
           (the steady-state on startup before any client has connected)
    when:  `_broadcast_channel_notification` is invoked
    then:  returns 0 without raising — the channel push is best-effort
           and never blocks the long-poll path.
    """
    from recog.copilot_channel import _broadcast_channel_notification

    class FakeSessionManager:
        _server_instances: dict = {}

    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    mcp._session_manager = FakeSessionManager()  # type: ignore[attr-defined]

    sent_count = await _broadcast_channel_notification(mcp, "x", {})
    assert sent_count == 0


async def test_handle_inbound_enqueues_all_four_event_types():
    """
    given: a fresh BridgeLink with an empty queue
    when:  `_handle_inbound` is fed each of the four bridge event types
           (`copilot_user_message`, `transcript_tick`, `meeting_started`,
           `meeting_ended`) plus `get_recent_transcript_response` (which
           must NOT enqueue — it resolves a pending future instead)
    then:  exactly four events are queued, in order; the response frame
           is consumed by the resolve path, not the queue.
    """
    from recog.copilot_channel import _handle_inbound

    link = BridgeLink()

    types = [
        "copilot_user_message",
        "transcript_tick",
        "meeting_started",
        "meeting_ended",
    ]
    for t in types:
        await _handle_inbound(json.dumps({"type": t, "marker": t}), link)

    # Response frame: resolved via pending — never goes to the queue.
    fut = link.register_pending("snap-x")
    await _handle_inbound(
        json.dumps({"type": "get_recent_transcript_response", "request_id": "snap-x", "segments": []}),
        link,
    )
    assert fut.done()

    drained = []
    while not link.event_queue.empty():
        drained.append(link.event_queue.get_nowait())
    assert [e["type"] for e in drained] == types


class _RecordingWS:
    """Minimal stand-in for a websockets connection that records sends.

    `BridgeLink.send` only needs an object with an async `send(str)`; this
    lets a test drive a real BridgeLink (real epoch/connection tracking)
    without a fake bridge server.
    """

    def __init__(self):
        self.sent: list[dict] = []

    async def send(self, data: str) -> None:
        self.sent.append(json.loads(data))


class _FakeSessionManager:
    def __init__(self, instances: dict | None = None):
        self._server_instances = instances if instances is not None else {}


class _FakeSessionManagerNoAttr:
    """A session manager missing `_server_instances` — models an SDK rename."""


async def test_session_count_reporter_emits_on_transitions_not_on_steady():
    """
    given: a channel server connected to a bridge (recording socket) with a
           fake session manager whose live-session count can be mutated
    when:  the reporter polls across 0 → 1 → 1 → 0 count transitions
    then:  a `claude_sessions` frame is sent on the first poll (post-connect
           announce), on 0→1 and on 1→0, but the steady 1→1 poll sends
           nothing.
    """
    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    manager = _FakeSessionManager()
    mcp._session_manager = manager  # type: ignore[attr-defined]
    reporter = SessionCountReporter(mcp, link)

    ws = _RecordingWS()
    link.set_socket(ws)  # epoch → 1, connected

    # First poll after connect announces the current count (0).
    assert await reporter.poll_once() == {"ok": True}
    # 0 → 1
    manager._server_instances["s1"] = object()
    assert await reporter.poll_once() == {"ok": True}
    # steady 1 → 1: no frame
    assert await reporter.poll_once() is None
    # 1 → 0
    manager._server_instances.clear()
    assert await reporter.poll_once() == {"ok": True}
    # steady 0 → 0: no frame
    assert await reporter.poll_once() is None

    assert ws.sent == [
        {"type": "claude_sessions", "count": 0},
        {"type": "claude_sessions", "count": 1},
        {"type": "claude_sessions", "count": 0},
    ]


async def test_session_count_reporter_reannounces_after_reconnect():
    """
    given: a reporter that already announced count=1 on one connection
    when:  the socket drops and a fresh one connects (new epoch) with the
           count unchanged
    then:  the reporter re-sends the current count on the next poll despite
           no count change — the bridge forgets state across restarts, so a
           reconnect must re-announce (`connection_epoch` gate).
    """
    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    manager = _FakeSessionManager({"s1": object()})
    mcp._session_manager = manager  # type: ignore[attr-defined]
    reporter = SessionCountReporter(mcp, link)

    ws1 = _RecordingWS()
    link.set_socket(ws1)  # epoch → 1
    assert await reporter.poll_once() == {"ok": True}
    assert ws1.sent == [{"type": "claude_sessions", "count": 1}]

    # Disconnect: poll is a no-op (nothing to report to).
    link.set_socket(None)
    assert await reporter.poll_once() is None

    # Reconnect on a new socket (epoch → 2); count is still 1.
    ws2 = _RecordingWS()
    link.set_socket(ws2)
    assert await reporter.poll_once() == {"ok": True}
    assert ws2.sent == [{"type": "claude_sessions", "count": 1}]


async def test_session_count_reporter_missing_attr_reports_null_once(capsys):
    """
    given: a session manager missing `_server_instances` (SDK renamed the
           private field)
    when:  the reporter polls twice
    then:  the first poll sends `claude_sessions {count: null}` and logs a
           single loud warning; the second poll (still unknown, unchanged)
           sends nothing and does not re-log — `null` is kept distinct from a
           confident 0 so an SDK refactor cannot force a false offline.
    """
    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    mcp._session_manager = _FakeSessionManagerNoAttr()  # type: ignore[attr-defined]
    reporter = SessionCountReporter(mcp, link)

    ws = _RecordingWS()
    link.set_socket(ws)
    capsys.readouterr()  # drop anything emitted during setup

    assert await reporter.poll_once() == {"ok": True}
    assert await reporter.poll_once() is None  # unchanged unknown → no resend

    assert ws.sent == [{"type": "claude_sessions", "count": None}]
    err = capsys.readouterr().err
    assert err.count("no `_server_instances`") == 1


async def test_session_count_reporter_reports_zero_before_server_serving():
    """
    given: the FastMCP HTTP loop has not started yet, so `session_manager` is
           a property that raises RuntimeError (not AttributeError) on access
    when:  the reporter polls right after the bridge socket connects
    then:  it reports a genuine `claude_sessions {count: 0}` (no sessions yet)
           instead of crashing the poll — `hasattr` does NOT swallow the
           RuntimeError, so the property access must be guarded explicitly.
    """
    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    assert mcp._session_manager is None  # HTTP loop not started → property raises

    reporter = SessionCountReporter(mcp, link)
    ws = _RecordingWS()
    link.set_socket(ws)

    assert await reporter.poll_once() == {"ok": True}
    assert ws.sent == [{"type": "claude_sessions", "count": 0}]


async def test_session_count_reporter_disconnected_is_noop():
    """
    given: a reporter whose link never connected
    when:  it polls
    then:  no frame is sent and no error is raised — there is nothing to
           report to.
    """
    link = BridgeLink()
    mcp = _build_mcp(link, "127.0.0.1", 0)
    mcp._session_manager = _FakeSessionManager()  # type: ignore[attr-defined]
    reporter = SessionCountReporter(mcp, link)

    assert link.is_connected() is False
    assert await reporter.poll_once() is None
