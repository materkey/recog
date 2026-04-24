"""WebSocket subscriber that forwards refine events from KTalk bridge to webapp clients.

The frontend FastAPI server runs as a separate launchd service from the bridge.
This module connects to the bridge as a WS client and relays `transcript_refine`
messages onto the webapp's own WS server (port 9080). See plan
`docs/plans/2026-04-24-per-speaker-refine-transport-ui.md` and design D10:
the JSONL tail-reader cannot see refine rewrites, so refined text reaches the
webapp via this side-channel.
"""

from __future__ import annotations

import asyncio
import json
import sys
import time
from typing import Awaitable, Callable, Optional

import websockets


ForwardCallback = Callable[[str], Awaitable[None]]


class BridgeSubscriber:
    """Maintain a WS client connection to the bridge with reconnect+backoff.

    Filters incoming frames by `type` and forwards `transcript_refine` to the
    supplied `forward_callback`. Other message types are dropped — webapp
    transcripts arrive via the JSONL tail reader, not the bridge channel.
    """

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
                    self._on_connected()
                    delay = self.INITIAL_BACKOFF_S
                    async for raw in ws:
                        await self._handle_message(raw)
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
        if data.get("type") != "transcript_refine":
            return
        try:
            await self._forward(raw)
        except Exception as exc:
            print(
                f"{self._log_prefix} forward callback raised: {exc!r}",
                file=sys.stderr,
            )
