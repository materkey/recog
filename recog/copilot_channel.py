"""KTalk Copilot channel server: MCP Streamable HTTP <-> bridge WS bridge.

This module is the local end of the Copilot channel described in the openspec
change `ktalk-claude-copilot-chat` (transport choice D4). It runs as a
separate process from the bridge and serves two roles simultaneously:

1. An MCP Streamable HTTP server (FastMCP) bound to `127.0.0.1:8767/mcp`.
   A locally-running Claude Code session adds it via
   `claude mcp add --transport http copilot http://127.0.0.1:8767/mcp` and
   gains two tools: `send_copilot_message` and `set_copilot_status`. The
   tool implementations push frames back through this process's WS client.

2. A WebSocket client of the KTalk bridge (`ws://127.0.0.1:8766` by default).
   On connect it sends `{"type": "copilot_channel_hello"}` so the bridge
   moves the socket out of the extension client set (see `_handle_client`
   disconnect path) and into `_copilot_channel_clients`. From there it
   reads frames in a loop. The tracer-bullet scope of this plan
   (`docs/plans/2026-04-26-ktalk-copilot-channel-scaffold.md`) does not yet
   consume `copilot_user_message`s — that arrives in the follow-up plan
   alongside the extension-side `Copilot` tab.

Reconnect uses simple exponential backoff (1s -> 2s -> 4s -> ... -> max 30s),
modelled on `frontend/bridge_subscriber.py` which serves the analogous role
for refine events (see ADR-0019 / D10).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
import uuid
from typing import Any, Optional

import websockets
from mcp.server.fastmcp import FastMCP


DEFAULT_BRIDGE_URL = "ws://127.0.0.1:8766"
DEFAULT_MCP_HOST = "127.0.0.1"
DEFAULT_MCP_PORT = 8767

INITIAL_BACKOFF_S = 1.0
MAX_BACKOFF_S = 30.0
LOG_THROTTLE_S = 60.0
LOG_PREFIX = "[copilot-channel]"


class BridgeLink:
    """Hold the live WS connection to the bridge and expose `send` for tools.

    The MCP tool callbacks run inside the FastMCP/uvicorn task; the WS
    reconnect loop runs as a separate asyncio task. Both share this object
    via the FastMCP instance — see `_attach_tools`. When the WS is down
    `send` returns an error dict instead of raising so the MCP tool reports
    a structured failure to the caller.

    `_pending` holds request_id -> Future entries awaiting an inbound
    response (e.g. `get_recent_transcript_response`). The inbound dispatcher
    in `_handle_inbound` resolves the matching future; the tool unregisters
    on timeout to avoid leaking entries.
    """

    def __init__(self) -> None:
        self._ws: Optional[Any] = None
        self._lock = asyncio.Lock()
        self._pending: dict[str, "asyncio.Future[dict]"] = {}

    def set_socket(self, ws: Optional[Any]) -> None:
        self._ws = ws
        # Drop any in-flight futures — the underlying socket has been
        # replaced, so the responses they were waiting for can never arrive.
        if ws is None and self._pending:
            for future in list(self._pending.values()):
                if not future.done():
                    future.set_exception(
                        ConnectionError("bridge connection dropped")
                    )
            self._pending.clear()

    def is_connected(self) -> bool:
        return self._ws is not None

    async def send(self, payload: dict) -> dict:
        ws = self._ws
        if ws is None:
            return {"ok": False, "error": "bridge not connected"}
        encoded = json.dumps(payload, ensure_ascii=False)
        async with self._lock:
            try:
                await ws.send(encoded)
            except Exception as exc:
                return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        return {"ok": True}

    def register_pending(self, request_id: str) -> "asyncio.Future[dict]":
        future: asyncio.Future[dict] = asyncio.get_running_loop().create_future()
        self._pending[request_id] = future
        return future

    def unregister_pending(self, request_id: str) -> None:
        self._pending.pop(request_id, None)

    def resolve_pending(self, request_id: str, result: dict) -> bool:
        future = self._pending.pop(request_id, None)
        if future is None or future.done():
            return False
        future.set_result(result)
        return True


def _build_mcp(link: BridgeLink, host: str, port: int) -> FastMCP:
    """Construct FastMCP and register the two Copilot tools.

    FastMCP exposes `host`, `port`, and `streamable_http_path` as
    constructor kwargs. With `streamable_http_path="/mcp"` (the default)
    the endpoint becomes `http://<host>:<port>/mcp`.
    """
    mcp = FastMCP(
        name="recog-copilot-channel",
        instructions=(
            "Send chat messages back to the KTalk extension Copilot tab "
            "for the active meeting. Use `send_copilot_message` for "
            "answers, transcripts, or proactive notes; use "
            "`set_copilot_status` to surface progress for a specific "
            "user request."
        ),
        host=host,
        port=port,
    )

    @mcp.tool(
        name="send_copilot_message",
        description=(
            "Send a Copilot message into the active KTalk meeting Copilot "
            "tab. Use kind='reply' for answers to a user request, "
            "kind='live' for proactive notes, or kind='manual' for "
            "ad-hoc messages."
        ),
    )
    async def send_copilot_message(
        kind: str,
        text: str,
        request_id: Optional[str] = None,
        meeting_id: Optional[str] = None,
    ) -> dict:
        if not isinstance(kind, str) or not kind:
            return {"ok": False, "error": "kind must be a non-empty string"}
        if not isinstance(text, str) or not text:
            return {"ok": False, "error": "text must be a non-empty string"}
        # Per design D7 `request_id` is optional on the tool surface
        # (proactive/manual sends have no user request to correlate).
        # The on-wire `copilot_channel_reply` frame still carries a
        # non-empty value because the bridge requires one — generate a
        # synthetic id when the caller omits it so the tool contract
        # does not trip the bridge validation.
        effective_request_id = (
            request_id if isinstance(request_id, str) and request_id
            else f"proactive_{uuid.uuid4().hex[:12]}"
        )
        return await link.send(
            {
                "type": "copilot_channel_reply",
                "request_id": effective_request_id,
                "kind": kind,
                "text": text,
                "meeting_id": meeting_id,
                "created_at": time.time() * 1000,
            }
        )

    @mcp.tool(
        name="set_copilot_status",
        description=(
            "Update the status indicator for a previously-received user "
            "request. Common statuses: 'queued', 'working', 'done', 'error'."
        ),
    )
    async def set_copilot_status(
        request_id: str,
        status: str,
        message: str = "",
    ) -> dict:
        if not isinstance(request_id, str) or not request_id:
            return {"ok": False, "error": "request_id must be a non-empty string"}
        if not isinstance(status, str) or not status:
            return {"ok": False, "error": "status must be a non-empty string"}
        return await link.send(
            {
                "type": "copilot_channel_status",
                "request_id": request_id,
                "status": status,
                "message": message,
            }
        )

    @mcp.tool(
        name="get_recent_transcript",
        description=(
            "Snapshot the recent transcript buffer for the active KTalk "
            "meeting. Returns segments newer than `window_seconds` (default "
            "180) and, if `since_segment_id` is provided, only segments "
            "with a strictly larger id. Set `include_refined=False` to skip "
            "tier-2 refined segments. The bridge filters by the current "
            "meeting_id, so segments from a meeting that already ended are "
            "never returned."
        ),
    )
    async def get_recent_transcript(
        window_seconds: int = 180,
        since_segment_id: Optional[int] = None,
        include_refined: bool = True,
    ) -> dict:
        if not link.is_connected():
            return {"ok": False, "error": "bridge not connected"}
        request_id = f"snapshot_{uuid.uuid4().hex[:12]}"
        future = link.register_pending(request_id)
        sent = await link.send(
            {
                "type": "get_recent_transcript_request",
                "request_id": request_id,
                "window_seconds": window_seconds,
                "since_segment_id": since_segment_id,
                "include_refined": include_refined,
            }
        )
        if not sent.get("ok"):
            link.unregister_pending(request_id)
            return sent
        try:
            response = await asyncio.wait_for(future, timeout=5.0)
        except asyncio.TimeoutError:
            link.unregister_pending(request_id)
            return {"ok": False, "error": "timeout waiting for transcript snapshot"}
        except ConnectionError as exc:
            return {"ok": False, "error": str(exc)}
        return {"ok": True, "segments": response.get("segments", [])}

    return mcp


async def _ws_loop(bridge_url: str, link: BridgeLink, stop_event: asyncio.Event) -> None:
    """Maintain the bridge WS connection with reconnect+backoff.

    Mirrors `frontend/bridge_subscriber.py::BridgeSubscriber._run`. On every
    successful connect we send `copilot_channel_hello` so the bridge tags
    this socket as a channel client; failure to send is non-fatal but the
    bridge will treat us as a regular extension client.
    """
    delay = INITIAL_BACKOFF_S
    failure_streak = 0
    last_failure_log_ts = 0.0
    connected_once = False

    while not stop_event.is_set():
        try:
            async with websockets.connect(bridge_url) as ws:
                try:
                    await ws.send(json.dumps({"type": "copilot_channel_hello"}))
                except Exception as exc:
                    print(
                        f"{LOG_PREFIX} copilot_channel_hello send failed: {exc!r}",
                        file=sys.stderr,
                    )
                link.set_socket(ws)
                failure_streak = 0
                if not connected_once:
                    connected_once = True
                    print(
                        f"{LOG_PREFIX} connected to {bridge_url}",
                        file=sys.stderr,
                    )
                else:
                    print(
                        f"{LOG_PREFIX} reconnected to {bridge_url}",
                        file=sys.stderr,
                    )
                delay = INITIAL_BACKOFF_S
                try:
                    async for raw in ws:
                        await _handle_inbound(raw, link)
                finally:
                    link.set_socket(None)
        except asyncio.CancelledError:
            link.set_socket(None)
            raise
        except Exception as exc:
            failure_streak += 1
            now = time.monotonic()
            if failure_streak == 1 or (now - last_failure_log_ts) >= LOG_THROTTLE_S:
                print(
                    f"{LOG_PREFIX} bridge unavailable "
                    f"({type(exc).__name__}: {exc}); retry in {delay:.1f}s",
                    file=sys.stderr,
                )
                last_failure_log_ts = now

        if stop_event.is_set():
            return
        try:
            await asyncio.wait_for(stop_event.wait(), timeout=delay)
            return
        except asyncio.TimeoutError:
            pass
        delay = min(delay * 2, MAX_BACKOFF_S)


async def _handle_inbound(raw: Any, link: BridgeLink) -> None:
    if isinstance(raw, (bytes, bytearray)):
        try:
            raw = raw.decode("utf-8")
        except UnicodeDecodeError:
            return
    if not isinstance(raw, str):
        return
    try:
        data = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return
    if not isinstance(data, dict):
        return
    msg_type = data.get("type")
    if msg_type == "copilot_user_message":
        request_id = data.get("request_id")
        text = data.get("text")
        print(
            f"{LOG_PREFIX} received copilot_user_message "
            f"request_id={request_id!r} text={text!r:.80}",
            file=sys.stderr,
        )
    elif msg_type == "get_recent_transcript_response":
        request_id = data.get("request_id")
        if isinstance(request_id, str) and request_id:
            link.resolve_pending(request_id, data)


async def _serve(bridge_url: str, mcp_host: str, mcp_port: int) -> None:
    link = BridgeLink()
    mcp = _build_mcp(link, mcp_host, mcp_port)
    stop_event = asyncio.Event()

    print(
        f"{LOG_PREFIX} MCP Streamable HTTP listening on "
        f"http://{mcp_host}:{mcp_port}/mcp",
        file=sys.stderr,
    )

    ws_task = asyncio.create_task(_ws_loop(bridge_url, link, stop_event))
    mcp_task = asyncio.create_task(mcp.run_streamable_http_async())
    try:
        done, _pending = await asyncio.wait(
            {ws_task, mcp_task}, return_when=asyncio.FIRST_COMPLETED
        )
        for task in done:
            exc = task.exception()
            if exc is not None:
                print(f"{LOG_PREFIX} task crashed: {exc!r}", file=sys.stderr)
    finally:
        stop_event.set()
        for task in (ws_task, mcp_task):
            if not task.done():
                task.cancel()
        for task in (ws_task, mcp_task):
            try:
                await task
            except asyncio.CancelledError:
                pass
            except Exception as exc:
                print(
                    f"{LOG_PREFIX} cleanup task failed: {exc!r}",
                    file=sys.stderr,
                )


def _parse_args(argv: Optional[list[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="KTalk Copilot channel server (MCP Streamable HTTP <-> bridge WS).",
    )
    parser.add_argument(
        "--bridge-url",
        default=DEFAULT_BRIDGE_URL,
        help=f"WebSocket URL of the KTalk bridge (default: {DEFAULT_BRIDGE_URL})",
    )
    parser.add_argument(
        "--mcp-host",
        default=DEFAULT_MCP_HOST,
        help=f"Host to bind the MCP HTTP server to (default: {DEFAULT_MCP_HOST})",
    )
    parser.add_argument(
        "--mcp-port",
        type=int,
        default=DEFAULT_MCP_PORT,
        help=f"Port to bind the MCP HTTP server to (default: {DEFAULT_MCP_PORT})",
    )
    return parser.parse_args(argv)


def main(argv: Optional[list[str]] = None) -> None:
    args = _parse_args(argv)
    try:
        asyncio.run(_serve(args.bridge_url, args.mcp_host, args.mcp_port))
    except KeyboardInterrupt:
        print(f"{LOG_PREFIX} interrupted, exiting", file=sys.stderr)


if __name__ == "__main__":
    main()
