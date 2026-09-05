"""Bidirectional WebSocket link between the KTalk bridge and webapp clients.

The frontend FastAPI server runs as a separate launchd service from the bridge.
This module connects to the bridge as a WS client, relays `FORWARD_TYPES`
frames (`transcript_refine`, `copilot_reply`, `copilot_status`) onto the
webapp's own WS server (port 9080), and carries webapp copilot frames
(`copilot_chat_message` / `copilot_spawn_request` / `copilot_stop_request`)
upstream to the bridge via best-effort `send()`. See plan
`docs/plans/2026-04-24-per-speaker-refine-transport-ui.md` design D10 (the
JSONL tail-reader cannot see refine rewrites, so refined text reaches the
webapp via this side-channel) and `docs/plans/20260724-webapp-copilot-live.md`
design D1 (the browser talks only to the frontend server; copilot traffic is
proxied over this single bridge connection).
"""

from __future__ import annotations

import asyncio
import json
import sys
import time
from typing import Any, Awaitable, Callable, Optional

import websockets


ForwardCallback = Callable[[str], Awaitable[None]]


class BridgeSubscriber:
    """Maintain a WS client connection to the bridge with reconnect+backoff.

    Downstream: filters incoming frames by `type` and forwards members of
    `FORWARD_TYPES` to the supplied `forward_callback`. Other message types
    are dropped — webapp transcripts arrive via the JSONL tail reader, not
    the bridge channel.

    Upstream: `send()` pushes webapp copilot frames over the same connection,
    best-effort — it returns False whenever the bridge link is down (no
    queueing across reconnects; a frame that failed is never redelivered).
    """

    FORWARD_TYPES = frozenset({"transcript_refine", "copilot_reply", "copilot_status"})

    INITIAL_BACKOFF_S = 1.0
    MAX_BACKOFF_S = 30.0
    LOG_THROTTLE_S = 60.0

    def __init__(
        self,
        bridge_url: str,
        forward_callback: ForwardCallback,
        *,
        log_prefix: str = "[bridge-subscriber]",
    ):
        self.bridge_url = bridge_url
        self._forward = forward_callback
        self._log_prefix = log_prefix
        self._task: Optional[asyncio.Task] = None
        self._stop_event: Optional[asyncio.Event] = None
        # Live bridge connection; None during the whole disconnected window
        # (cleared before the backoff sleep starts) so send() fails fast.
        self._ws: Optional[Any] = None
        self._connected_once = False
        self._last_failure_log_ts: float = 0.0
        self._failure_streak = 0

    async def start(self) -> None:
        """Spawn the run loop on the current event loop."""
        if self._task is not None:
            return
        self._stop_event = asyncio.Event()
        self._task = asyncio.create_task(self._run())

    async def stop(self) -> None:
        """Signal the loop to exit and wait for it to finish.

        Bounded by a short timeout so a hung bridge-close handshake does
        not block frontend shutdown.
        """
        if self._stop_event is not None:
            self._stop_event.set()
        if self._task is not None:
            self._task.cancel()
            try:
                await asyncio.wait_for(self._task, timeout=5.0)
            except asyncio.CancelledError:
                pass
            except asyncio.TimeoutError:
                print(
                    f"{self._log_prefix} stop timed out; detaching task",
                    file=sys.stderr,
                )
            except Exception as exc:
                print(
                    f"{self._log_prefix} stop raised: {exc!r}",
                    file=sys.stderr,
                )
            self._task = None
        self._stop_event = None

    async def _run(self) -> None:
        delay = self.INITIAL_BACKOFF_S
        assert self._stop_event is not None
        while not self._stop_event.is_set():
            try:
                async with websockets.connect(self.bridge_url) as ws:
                    try:
                        # Announce ourselves as a subscriber: the bridge
                        # moves this connection into its `_subscriber_clients`
                        # set so it does NOT count toward the extension-client
                        # lifecycle (meeting end on last disconnect) and does
                        # NOT trigger clock_offset/last_seq resets intended for
                        # the Chrome extension. Failure is non-fatal — a bridge
                        # that predates this protocol still delivers refines,
                        # but we'll hold open a phantom "extension" slot.
                        try:
                            await ws.send(json.dumps({"type": "subscriber_hello"}))
                        except Exception as exc:
                            print(
                                f"{self._log_prefix} subscriber_hello send failed: {exc!r}",
                                file=sys.stderr,
                            )
                        # Publish the connection for send() only after the
                        # hello is on the wire: an upstream copilot frame must
                        # never be the connection's first frame, or the bridge
                        # would process it while still classifying this socket
                        # as an extension client.
                        self._ws = ws
                        self._on_connected()
                        delay = self.INITIAL_BACKOFF_S
                        async for raw in ws:
                            await self._handle_message(raw)
                    finally:
                        # Clear before the backoff sleep starts so send()
                        # fails fast for the whole disconnected window.
                        self._ws = None
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                self._on_failure(exc, delay)

            if self._stop_event.is_set():
                return
            try:
                await asyncio.wait_for(self._stop_event.wait(), timeout=delay)
                return
            except asyncio.TimeoutError:
                pass
            delay = min(delay * 2, self.MAX_BACKOFF_S)

    async def send(self, raw: str) -> bool:
        """Best-effort upstream send; True once the frame is written to the transport.

        True does NOT mean the bridge received or processed the frame —
        delivery confirmation only comes via app-level replies. No buffering,
        no retry, no waiting for reconnect: while the bridge link is down
        (never connected, backoff window) this returns False immediately and
        the frame is dropped forever. Failures never raise into the caller
        and never perturb the run loop's reconnect cycle.
        """
        # Local read is conventional style, not race protection: asyncio is
        # single-threaded and there is no await point between this check and
        # the use below. The only real race — the connection dying around or
        # during `await ws.send()` — is handled by the except path.
        ws = self._ws
        if ws is None:
            return False
        try:
            await ws.send(raw)
            return True
        except Exception as exc:
            print(
                f"{self._log_prefix} send failed: {exc!r}",
                file=sys.stderr,
            )
            return False

    def _on_connected(self) -> None:
        self._failure_streak = 0
        if not self._connected_once:
            self._connected_once = True
            print(
                f"{self._log_prefix} connected to {self.bridge_url}",
                file=sys.stderr,
            )
        else:
            print(
                f"{self._log_prefix} reconnected to {self.bridge_url}",
                file=sys.stderr,
            )

    def _on_failure(self, exc: BaseException, delay: float) -> None:
        self._failure_streak += 1
        now = time.monotonic()
        first_after_success = self._failure_streak == 1
        log_due_to_throttle = (now - self._last_failure_log_ts) >= self.LOG_THROTTLE_S
        if first_after_success or log_due_to_throttle:
            print(
                f"{self._log_prefix} bridge unavailable ({type(exc).__name__}: {exc}); "
                f"retry in {delay:.1f}s",
                file=sys.stderr,
            )
            self._last_failure_log_ts = now

    async def _handle_message(self, raw) -> None:
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
        # isinstance guard keeps this total: an unhashable `type` value (e.g.
        # a list) would raise TypeError on frozenset membership and kill the
        # live connection.
        if not isinstance(msg_type, str) or msg_type not in self.FORWARD_TYPES:
            return
        try:
            await self._forward(raw)
        except Exception as exc:
            print(
                f"{self._log_prefix} forward callback raised: {exc!r}",
                file=sys.stderr,
            )
