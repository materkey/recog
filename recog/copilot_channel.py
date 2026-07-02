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

## System prompt for Claude

The MCP server description (`FastMCP.instructions` below) carries the
behaviour contract for Claude when it receives `transcript_tick` events or
direct `copilot_user_message`s. Defaults are conservative:

- **Default to silence.** Most ticks should produce no `send_copilot_message`
  call. Proactive output is rate-limited by the extension UI cooldown (60 s)
  and a global `mute proactive 10m` toggle, so a high-volume style would
  simply be discarded — but discarded messages still cost tokens.
- **Reply only on direct user requests.** Manual chat from the local user
  (`copilot_user_message`) always deserves an answer. Echo unchanged
  questions back to the user with a one-line acknowledgement only if a
  longer reply is still pending.
- **Proactive triggers.** Speak up when the transcript shows: a question
  addressed to the local user (`MIC_SPEAKER_NAME`); a missing owner or
  deadline on a stated action item; a technical risk that the participants
  have not flagged; a contradiction with earlier transcript context.
- **Never speak up for.** Filler, agreement, off-topic chat, or anything
  that would be obvious to the participants. Do not summarise the meeting
  unless asked.
- **Tool ordering.** When unsure whether the buffered context is fresh,
  call `get_recent_transcript` first; do not invent context from memory.
- **kind selection.** `kind="proactive"` for anything not tied to a
  `request_id`; `kind="reply"` when answering a `copilot_user_message`.
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
from mcp.shared.message import SessionMessage
from mcp.types import JSONRPCMessage, JSONRPCNotification


DEFAULT_BRIDGE_URL = "ws://127.0.0.1:8766"
DEFAULT_MCP_HOST = "127.0.0.1"
DEFAULT_MCP_PORT = 8767

# Claude Code Channels (research preview): an MCP server declaring
# `experimental['claude/channel']: {}` may emit `notifications/claude/channel`
# notifications that wake the running session. The handler is gated on the
# server name being listed in `--channels` (or `--dangerously-load-development-channels
# server:<name>` for local dev). Transport is intentionally NOT gated on the
# Claude Code side (`channelNotification.ts:gateChannelServer`) — the Streamable
# HTTP transport is therefore valid for channel push, despite the bundled plugins
# all using stdio. See `services/mcp/useManageMCPConnections.ts` for the
# `setNotificationHandler` registration site.
CHANNEL_NOTIFICATION_METHOD = "notifications/claude/channel"
CHANNEL_CAPABILITY_KEY = "claude/channel"

INITIAL_BACKOFF_S = 1.0
MAX_BACKOFF_S = 30.0
LOG_THROTTLE_S = 60.0
LOG_PREFIX = "[copilot-channel]"

# Cadence of the MCP-session-count poll that feeds the bridge status merge
# (D4). 5 s matches the debounce window the bridge applies before letting a
# steady `claude_sessions == 0` flip the chip to offline (see the
# `claude_sessions` merge comment in ktalk_bridge.py).
SESSION_COUNT_POLL_INTERVAL_S = 5.0


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
        # Monotonic counter bumped on every successful (re)connect. The
        # session-count reporter watches it to re-announce the current count
        # right after a reconnect (the bridge forgets state across restarts),
        # even when the count itself has not changed.
        self.connection_epoch = 0
        # Bounded backlog for bridge → Claude events (`copilot_user_message`,
        # `transcript_tick`, `meeting_started`, `meeting_ended`). Standard
        # MCP has no server-pushed event channel that would wake Claude
        # between tool calls; instead Claude drains this queue via the
        # `await_copilot_event` long-poll tool while in copilot loop mode.
        # maxsize=50 ≈ 12 min of transcript_ticks at the 15 s cadence —
        # comfortably more than a polling Claude could miss before catching
        # up. On overflow we drop the oldest event so the freshest meeting
        # state is preserved.
        self.event_queue: asyncio.Queue[dict] = asyncio.Queue(maxsize=50)

    def set_socket(self, ws: Optional[Any]) -> None:
        self._ws = ws
        if ws is not None:
            self.connection_epoch += 1
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


class SessionCountReporter:
    """Report the live MCP-session count to the bridge as `claude_sessions`.

    The bridge status merge (D4) treats `claude_sessions == 0` during an
    active meeting as "TUI up but the copilot MCP session never connected" →
    offline chip. `count == null` means *unknown* (the SDK renamed the private
    `_server_instances` attribute this reads) and is deliberately kept distinct
    from a confident 0 so an SDK refactor cannot force a false offline.

    A frame is sent only when the count changes, plus once right after every
    (re)connect — tracked via `BridgeLink.connection_epoch` — so the bridge
    learns the current state before the first 0↔n transition; otherwise a
    bridge restart mid-session would leave it blind until the next change.
    """

    def __init__(self, mcp: FastMCP, link: BridgeLink) -> None:
        self._mcp = mcp
        self._link = link
        self._last_count: Optional[int] = None
        self._reported_epoch = 0
        self._warned_missing_attr = False

    def _current_count(self) -> Optional[int]:
        # `session_manager` is a FastMCP property that raises RuntimeError until
        # the Streamable HTTP loop is up. `hasattr` does NOT collapse that into
        # False — in Python 3 it only swallows AttributeError, so a RuntimeError
        # would propagate. Guard the access explicitly and treat "not serving
        # yet" as a genuine 0 (no sessions yet).
        try:
            manager = self._mcp.session_manager
        except (RuntimeError, AttributeError):
            return 0
        if manager is None:
            return 0
        sentinel = object()
        instances = getattr(manager, "_server_instances", sentinel)
        if instances is sentinel:
            # SDK renamed the private field: report unknown (null), not a
            # confident 0 that the bridge would trust as a real offline.
            if not self._warned_missing_attr:
                print(
                    f"{LOG_PREFIX} session_manager has no `_server_instances` "
                    f"attribute — reporting claude_sessions count=null "
                    f"(unknown); the MCP SDK likely renamed the private field.",
                    file=sys.stderr,
                )
                self._warned_missing_attr = True
            return None
        return len(instances or {})

    async def poll_once(self) -> Optional[dict]:
        """One poll iteration.

        Sends a `claude_sessions` frame when the count changed since the last
        send or a (re)connect happened since then. Returns the `link.send`
        result dict when a frame was sent, else None (disconnected, or no
        change on the same connection).
        """
        if not self._link.is_connected():
            return None
        epoch = self._link.connection_epoch
        count = self._current_count()
        reconnected = epoch != self._reported_epoch
        if not reconnected and count == self._last_count:
            return None
        result = await self._link.send(
            {"type": "claude_sessions", "count": count}
        )
        self._last_count = count
        self._reported_epoch = epoch
        return result


def _attach_channel_capability(mcp: FastMCP) -> None:
    """Inject `experimental['claude/channel']: {}` into initialization.

    FastMCP doesn't expose `experimental_capabilities` as a constructor kwarg,
    so we monkey-patch the underlying lowlevel Server's
    `create_initialization_options` to merge our capability key into whatever
    the rest of the SDK passes in. This is the only knob Claude Code's
    `gateChannelServer` actually inspects, so without it the server is invisible
    to `--channels`.
    """
    server = mcp._mcp_server
    original = server.create_initialization_options

    def patched(notification_options=None, experimental_capabilities=None):
        caps = dict(experimental_capabilities or {})
        caps.setdefault(CHANNEL_CAPABILITY_KEY, {})
        return original(notification_options, caps)

    server.create_initialization_options = patched  # type: ignore[assignment]


async def _broadcast_channel_notification(
    mcp: FastMCP, content: str, meta: dict[str, str]
) -> int:
    """Push a `notifications/claude/channel` to every active HTTP session.

    The Streamable HTTP session manager owns a registry of active transports
    keyed by mcp_session_id. Each transport's `_write_stream` feeds its
    outbound SSE channel — pushing a SessionMessage there is the same path
    `ServerSession.send_notification` uses internally, just bypassed because
    we're emitting from outside the session task (no `request_context`).

    Returns the number of sessions the notification was successfully written
    to. Zero is normal at startup before any client has initialized.
    """
    # `session_manager` raises RuntimeError before the HTTP loop is up;
    # `hasattr` does not swallow that (only AttributeError) — guard explicitly.
    try:
        manager = mcp.session_manager
    except (RuntimeError, AttributeError):
        return 0
    if manager is None:
        return 0
    transports = getattr(manager, "_server_instances", None) or {}
    if not transports:
        return 0

    notification = JSONRPCNotification(
        jsonrpc="2.0",
        method=CHANNEL_NOTIFICATION_METHOD,
        params={"content": content, "meta": meta},
    )
    session_message = SessionMessage(message=JSONRPCMessage(notification))

    sent = 0
    for transport in list(transports.values()):
        write_stream = getattr(transport, "_write_stream", None)
        if write_stream is None:
            continue
        try:
            await write_stream.send(session_message)
            sent += 1
        except Exception as exc:
            print(
                f"{LOG_PREFIX} channel notification push failed: "
                f"{type(exc).__name__}: {exc}",
                file=sys.stderr,
            )
    return sent


def _format_channel_event(event: dict) -> tuple[str, dict[str, str]]:
    """Turn a bridge event dict into `(content, meta)` for the channel push.

    The event payload becomes the body of the `<channel>` tag Claude sees;
    meta keys become tag attributes. Meta keys must be identifiers (letters,
    digits, underscores) per the Claude Code reference — non-string values
    are coerced to str, and keys are filtered.
    """
    msg_type = str(event.get("type") or "unknown")
    if msg_type == "copilot_user_message":
        content = (
            f"User typed in the Copilot tab. Reply with send_copilot_message "
            f"kind='reply' carrying the original request_id.\n\n"
            f"Text: {event.get('text', '')}"
        )
    elif msg_type == "transcript_tick":
        new_segments = event.get("new_segments") or []
        recent = event.get("recent_context") or ""
        content = (
            f"Transcript tick: {len(new_segments)} new segment(s). Apply the "
            f"proactive-triggers contract; default to silence.\n\n"
            f"Recent context:\n{recent}"
        )
    elif msg_type == "meeting_started":
        content = (
            f"Meeting started: {event.get('meeting_title') or '(untitled)'}. "
            f"Reset internal proactive state."
        )
    elif msg_type == "meeting_ended":
        content = (
            "Meeting ended. Stop polling and exit the copilot loop."
        )
    else:
        content = json.dumps(event, ensure_ascii=False)

    meta_in: dict[str, Any] = {"event_type": msg_type}
    for key in ("request_id", "meeting_id", "meeting_title", "local_user_name"):
        value = event.get(key)
        if value is None:
            continue
        meta_in[key] = value

    meta_out: dict[str, str] = {}
    for k, v in meta_in.items():
        # Per channels-reference: meta keys must be [A-Za-z0-9_]+; others
        # are silently dropped by Claude Code. Coerce values to string —
        # the spec is `Record<string, string>`.
        if not isinstance(k, str) or not k.replace("_", "").isalnum():
            continue
        meta_out[k] = v if isinstance(v, str) else json.dumps(v, ensure_ascii=False)
    return content, meta_out


def _build_mcp(link: BridgeLink, host: str, port: int) -> FastMCP:
    """Construct FastMCP and register the two Copilot tools.

    FastMCP exposes `host`, `port`, and `streamable_http_path` as
    constructor kwargs. With `streamable_http_path="/mcp"` (the default)
    the endpoint becomes `http://<host>:<port>/mcp`.
    """
    mcp = FastMCP(
        name="recog-copilot-channel",
        instructions=(
            "KTalk Copilot channel: send messages into the active KTalk "
            "meeting Copilot tab, snapshot the recent transcript, and "
            "drain bridge events while a meeting is active.\n"
            "\n"
            "Tools:\n"
            "- send_copilot_message(kind, text, request_id?, meeting_id?): "
            "post a message. Use kind='reply' for answers to a user "
            "request, kind='proactive' for unsolicited notes, or "
            "kind='manual' for ad-hoc messages.\n"
            "- set_copilot_status(request_id, status, message?): update "
            "the per-request status indicator (queued/working/done/error).\n"
            "- get_recent_transcript(window_seconds=180, since_segment_id?, "
            "include_refined=True): snapshot recent segments for the active "
            "meeting only. Call this before answering when the buffered "
            "context might be stale; do not invent context from memory.\n"
            "- await_copilot_event(timeout_seconds=60): block until the "
            "next bridge event (`copilot_user_message`, `transcript_tick`, "
            "`meeting_started`, `meeting_ended`) or the timeout. Returns "
            "{'event': null} on timeout. Standard MCP has no server-pushed "
            "wake-up channel, so call this in a loop while a meeting is "
            "active to consume events.\n"
            "\n"
            "Loop pattern (when the user asks you to enter copilot mode):\n"
            "1. Call await_copilot_event(timeout_seconds=60).\n"
            "2. If event is null, call again (or stop after a sustained "
            "idle period — three consecutive null returns ≈ no meeting).\n"
            "3. On copilot_user_message: read its text, decide whether the "
            "buffered context is fresh (call get_recent_transcript if not), "
            "answer with send_copilot_message(kind='reply', "
            "request_id=event['request_id'], text=...).\n"
            "4. On transcript_tick: apply the proactive-triggers contract "
            "below; in most cases stay silent; if you do speak up, use "
            "send_copilot_message(kind='proactive', text=...).\n"
            "5. On meeting_started / meeting_ended: reset internal "
            "proactive state. After meeting_ended, exit the loop.\n"
            "6. After each handled event return to step 1.\n"
            "\n"
            "Behaviour contract:\n"
            "- Default to silence. Most transcript_tick events should "
            "produce no send_copilot_message call. Proactive output is "
            "rate-limited by the extension UI cooldown (60 s) and a "
            "global mute toggle, so noisy output is dropped — but "
            "discarded messages still cost tokens.\n"
            "- Reply to direct user requests (copilot_user_message) with "
            "kind='reply' carrying the original request_id.\n"
            "- Proactive triggers: a question addressed to the local user, "
            "a missing owner or deadline on a stated action item, a "
            "technical risk participants have not flagged, a contradiction "
            "with earlier transcript context.\n"
            "- Never speak up for filler, agreement, off-topic chat, or "
            "anything obvious to participants. Do not summarise the "
            "meeting unless asked."
        ),
        host=host,
        port=port,
    )

    @mcp.tool(
        name="send_copilot_message",
        description=(
            "Send a Copilot message into the active KTalk meeting Copilot "
            "tab. Use kind='reply' for answers to a user request, "
            "kind='proactive' for unsolicited notes (the extension UI "
            "filters proactive messages through a 60 s cooldown and the "
            "user's mute toggle — only kind='proactive' triggers that "
            "path), or kind='manual' for ad-hoc messages."
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
        # Wrap the entire body in try/finally so a CancelledError raised
        # from `link.send` (e.g. MCP HTTP connection closed mid-await)
        # cannot leave the future entry pinned in `_pending` until the
        # next reconnect drops the socket — without this, repeated tool
        # cancellations leak futures unboundedly while the WS stays up.
        try:
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
                return sent
            try:
                response = await asyncio.wait_for(future, timeout=5.0)
            except asyncio.TimeoutError:
                return {"ok": False, "error": "timeout waiting for transcript snapshot"}
            except ConnectionError as exc:
                return {"ok": False, "error": str(exc)}
            return {"ok": True, "segments": response.get("segments", [])}
        finally:
            link.unregister_pending(request_id)

    _attach_channel_capability(mcp)

    @mcp.tool(
        name="await_copilot_event",
        description=(
            "Block until the next bridge event arrives or the timeout "
            "elapses. Returns {'ok': True, 'event': <dict>} on success or "
            "{'ok': True, 'event': null} on timeout. Event types: "
            "'copilot_user_message' (the user typed in the Copilot tab — "
            "reply with send_copilot_message kind='reply' carrying the "
            "original request_id), 'transcript_tick' (new transcript "
            "segments — apply the proactive-triggers contract), "
            "'meeting_started' / 'meeting_ended' (lifecycle markers; "
            "reset internal proactive state and exit the loop after "
            "meeting_ended). Call this in a loop while a meeting is "
            "active. The queue is bounded at 50 events; oldest events "
            "are dropped on overflow."
        ),
    )
    async def await_copilot_event(timeout_seconds: int = 60) -> dict:
        if not isinstance(timeout_seconds, int) or timeout_seconds <= 0:
            return {"ok": False, "error": "timeout_seconds must be a positive int"}
        # Cap timeouts to keep individual MCP requests bounded — a single
        # tool call should not hold the HTTP connection open for hours.
        timeout_seconds = min(timeout_seconds, 300)
        try:
            event = await asyncio.wait_for(
                link.event_queue.get(),
                timeout=timeout_seconds,
            )
            return {"ok": True, "event": event}
        except asyncio.TimeoutError:
            return {"ok": True, "event": None}

    return mcp


async def _ws_loop(
    bridge_url: str,
    link: BridgeLink,
    stop_event: asyncio.Event,
    mcp: Optional[FastMCP] = None,
) -> None:
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
                        await _handle_inbound(raw, link, mcp)
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


async def _handle_inbound(
    raw: Any, link: BridgeLink, mcp: Optional[FastMCP] = None
) -> None:
    """Dispatch inbound bridge frames.

    Two paths today:

    - `get_recent_transcript_response` resolves the pending tool future
      registered by the matching `get_recent_transcript` call.
    - `copilot_user_message`, `transcript_tick`, and
      `meeting_started`/`meeting_ended` are enqueued onto
      `link.event_queue` so the `await_copilot_event` long-poll tool can
      hand them to Claude. Standard MCP has no generic server-pushed
      event channel that would wake Claude between tool calls, so the
      `tasks.md` 1.1 spike concluded with a long-poll pattern instead:
      Claude is told (via the MCP server `instructions`) to call
      `await_copilot_event` in a loop while a meeting is active. On
      queue overflow we drop the oldest event to keep the freshest
      meeting state.
    """
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
    if msg_type == "get_recent_transcript_response":
        request_id = data.get("request_id")
        if isinstance(request_id, str) and request_id:
            link.resolve_pending(request_id, data)
        return
    if msg_type in {
        "copilot_user_message",
        "transcript_tick",
        "meeting_started",
        "meeting_ended",
    }:
        queue = link.event_queue
        if queue.full():
            try:
                dropped = queue.get_nowait()
                print(
                    f"{LOG_PREFIX} event_queue full — dropped oldest "
                    f"{dropped.get('type')!r}",
                    file=sys.stderr,
                )
            except asyncio.QueueEmpty:
                pass
        try:
            queue.put_nowait(data)
        except asyncio.QueueFull:
            # Race with another consumer between full() and put_nowait();
            # discard this event rather than block the inbound loop.
            print(
                f"{LOG_PREFIX} event_queue still full after drain — "
                f"discarded {msg_type!r}",
                file=sys.stderr,
            )
        # Channels push (research preview): if an MCP HTTP session is open
        # and the client has --channels enabled for us, this wakes Claude
        # immediately. If no session is open the broadcast is a no-op and
        # Claude can still drain the queue via `await_copilot_event`.
        if mcp is not None:
            content, meta = _format_channel_event(data)
            try:
                sent = await _broadcast_channel_notification(mcp, content, meta)
                print(
                    f"{LOG_PREFIX} channel push {msg_type!r} → {sent} session(s)",
                    file=sys.stderr,
                )
            except Exception as exc:
                print(
                    f"{LOG_PREFIX} channel push failed for {msg_type!r}: "
                    f"{type(exc).__name__}: {exc}",
                    file=sys.stderr,
                )


async def _session_count_poll_loop(
    reporter: SessionCountReporter, stop_event: asyncio.Event
) -> None:
    """Poll the MCP-session count every `SESSION_COUNT_POLL_INTERVAL_S`.

    `reporter.poll_once` only emits a frame on a change or a reconnect, so a
    steady session count produces no bridge traffic. Exceptions inside a poll
    are logged and swallowed — a transient reporting failure must not tear
    down the channel server.
    """
    while not stop_event.is_set():
        try:
            await reporter.poll_once()
        except Exception as exc:
            print(
                f"{LOG_PREFIX} session-count poll failed: "
                f"{type(exc).__name__}: {exc}",
                file=sys.stderr,
            )
        try:
            await asyncio.wait_for(
                stop_event.wait(), timeout=SESSION_COUNT_POLL_INTERVAL_S
            )
            return
        except asyncio.TimeoutError:
            pass


async def _serve(bridge_url: str, mcp_host: str, mcp_port: int) -> None:
    link = BridgeLink()
    mcp = _build_mcp(link, mcp_host, mcp_port)
    stop_event = asyncio.Event()
    reporter = SessionCountReporter(mcp, link)

    print(
        f"{LOG_PREFIX} MCP Streamable HTTP listening on "
        f"http://{mcp_host}:{mcp_port}/mcp",
        file=sys.stderr,
    )

    ws_task = asyncio.create_task(_ws_loop(bridge_url, link, stop_event, mcp))
    mcp_task = asyncio.create_task(mcp.run_streamable_http_async())
    poll_task = asyncio.create_task(
        _session_count_poll_loop(reporter, stop_event)
    )
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
        for task in (ws_task, mcp_task, poll_task):
            if not task.done():
                task.cancel()
        for task in (ws_task, mcp_task, poll_task):
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
