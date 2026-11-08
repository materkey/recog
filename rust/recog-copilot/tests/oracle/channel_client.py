"""An MCP client on the official Python SDK's Streamable HTTP transport, for
rust/recog-copilot/tests/channel_differential.rs: the same conversation with the Python
channel and with the Rust one, the messages each answered with printed as JSON lines.

Usage: python channel_client.py URL. After `initialize` and `notifications/initialized`
(which makes the SDK open its standing GET stream) it prints {"ready": true} and waits
for a line on stdin; the test then has the fake bridge push an event, which arrives
both as a `notifications/claude/channel` push and as the answer of the pending
`await_copilot_event` long-poll. The SDK's `ClientSession` would drop the channel
notification (it is not in its notification union), so the client drives the
transport's message streams directly.
"""

import asyncio
import json
import sys

import anyio
from mcp.client.streamable_http import streamable_http_client
from mcp.shared.message import SessionMessage
from mcp.types import JSONRPCMessage, JSONRPCNotification, JSONRPCRequest


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


async def main(url):
    async with streamable_http_client(url) as (read_stream, write_stream, _session_id):

        async def send(message):
            await write_stream.send(SessionMessage(JSONRPCMessage(message)))

        async def request(request_id, method, params=None):
            await send(JSONRPCRequest(jsonrpc="2.0", id=request_id, method=method, params=params))

        async def receive_until(request_id):
            while True:
                message = await read_stream.receive()
                if isinstance(message, Exception):
                    emit({"exception": type(message).__name__})
                    continue
                root = message.message.root
                emit(root.model_dump(by_alias=True, mode="json", exclude_none=True))
                if getattr(root, "id", None) == request_id:
                    return

        await request(
            1,
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "parity-client", "version": "1"},
            },
        )
        await receive_until(1)
        await send(JSONRPCNotification(jsonrpc="2.0", method="notifications/initialized"))
        await request(2, "tools/list")
        await receive_until(2)
        await request(
            3,
            "tools/call",
            {
                "name": "set_copilot_status",
                "arguments": {"request_id": "req-1", "status": "working"},
            },
        )
        await receive_until(3)
        await anyio.sleep(1.0)  # let the SDK's GET stream settle
        emit({"ready": True})
        await request(
            4, "tools/call", {"name": "await_copilot_event", "arguments": {"timeout_seconds": 20}}
        )
        await asyncio.get_running_loop().run_in_executor(None, sys.stdin.readline)
        await receive_until(4)
        # The push travels the GET stream: wait for it if it has not arrived yet.
        with anyio.move_on_after(5):
            message = await read_stream.receive()
            emit(message.message.root.model_dump(by_alias=True, mode="json", exclude_none=True))
        await request(
            5,
            "tools/call",
            {
                "name": "send_copilot_message",
                "arguments": {"kind": "reply", "text": "Готово", "request_id": "req-1"},
            },
        )
        await receive_until(5)


if __name__ == "__main__":
    anyio.run(main, sys.argv[1])
