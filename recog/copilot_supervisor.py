"""Supervisor daemon that auto-spawns the Copilot Claude session per KTalk meeting.

Connects to ktalk_bridge as a WS client (`supervisor_hello`), listens for
`meeting_started` / `meeting_ended` lifecycle frames and drives a detached
tmux session with the Claude Code copilot (spawn on meeting start, kill after
a grace period on meeting end). See
`docs/plans/2026-07-02-auto-copilot-session.md`.

State machine (SessionLifecycle, pure — no I/O):

    IDLE ──meeting_started(A)──► SPAWNING ──session_ready──► RUNNING(A)
    RUNNING(A) ──meeting_ended(A)──► GRACE(A, timer)
    GRACE(A) ──meeting_started(A)──► RUNNING(A)      (flap: same id — keep session)
    GRACE(A) ──meeting_started(B)──► kill + SPAWNING(B)
    GRACE(A) ──timer expired──► kill + IDLE
    RUNNING(A) ──meeting_started(B)──► kill + SPAWNING(B)
    RUNNING(A) ──session_died──► SPAWNING(A)          (respawn; backoff in runner glue)
"""

from __future__ import annotations

import argparse
import asyncio
import enum
import json
import os
import sys
import time
from dataclasses import dataclass
from typing import Callable, Optional

import websockets


DEFAULT_BRIDGE_URL = "ws://127.0.0.1:8766"
DEFAULT_GRACE_S = 90.0


def _env_flag(name: str, default: bool) -> bool:
    raw = os.environ.get(name)
    if raw is None or raw.strip() == "":
        return default
    return raw.strip().lower() not in ("0", "false", "no", "off")


# --- Commands: outputs of the state machine, executed by the supervisor glue ---


@dataclass(frozen=True)
class Spawn:
    meeting_id: Optional[str]


@dataclass(frozen=True)
class Kill:
    pass


class State(enum.Enum):
    IDLE = "idle"
    SPAWNING = "spawning"
    RUNNING = "running"
    GRACE = "grace"


class SessionLifecycle:
    """Pure lifecycle state machine — no I/O, no timers.

    Inputs are method calls (`meeting_started` / `meeting_ended` /
    `timer_expired` / `session_died` / `session_ready` / `adopt`); each
    returns a tuple of commands (`Spawn` / `Kill`) for the caller to execute.
    The grace timer itself lives in the supervisor glue: it calls
    `timer_expired()` when the timer fires and keeps the timer in sync with
    `state is State.GRACE`.

    `meeting_id is None` while not IDLE means "adopted session, meeting
    unknown" (supervisor restart reconciliation) — it matches any incoming
    meeting id so a rejoin flap keeps the session alive.
    """

    def __init__(self) -> None:
        self.state = State.IDLE
        self.meeting_id: Optional[str] = None

    def _same_meeting(self, meeting_id: Optional[str]) -> bool:
        return self.meeting_id is None or self.meeting_id == meeting_id

    def meeting_started(self, meeting_id: str) -> tuple:
        if self.state is State.IDLE:
            self.state = State.SPAWNING
            self.meeting_id = meeting_id
            return (Spawn(meeting_id),)
        if self._same_meeting(meeting_id):
            self.meeting_id = meeting_id  # resolve unknown id after adopt
            if self.state is State.GRACE:
                self.state = State.RUNNING
            return ()
        # Different meeting while a session exists (SPAWNING/RUNNING/GRACE):
        # back-to-back meetings — replace the session.
        self.state = State.SPAWNING
        self.meeting_id = meeting_id
        return (Kill(), Spawn(meeting_id))

    def meeting_ended(self, meeting_id: Optional[str]) -> tuple:
        if self.state in (State.RUNNING, State.SPAWNING) and self._same_meeting(meeting_id):
            self.state = State.GRACE
        return ()

    def timer_expired(self) -> tuple:
        if self.state is not State.GRACE:
            return ()
        self.state = State.IDLE
        self.meeting_id = None
        return (Kill(),)

    def session_died(self) -> tuple:
        if self.state in (State.RUNNING, State.SPAWNING):
            self.state = State.SPAWNING
            return (Spawn(self.meeting_id),)
        if self.state is State.GRACE:
            # Nothing left to keep alive through grace — no kill needed either.
            self.state = State.IDLE
            self.meeting_id = None
        return ()

    def session_ready(self) -> tuple:
        if self.state is State.SPAWNING:
            self.state = State.RUNNING
        return ()

    def adopt(self, meeting_id: Optional[str]) -> tuple:
        """Attach to an already-live tmux session without spawning.

        Used by reconciliation after a supervisor restart: the session
        exists but the machine is IDLE. `meeting_id=None` = unknown.
        """
        self.state = State.RUNNING
        self.meeting_id = meeting_id
        return ()


class Supervisor:
    """WS glue around SessionLifecycle: bridge connection, grace timer, runner.

    `runner` (spawn/kill target, wired to TmuxRunner in a later task) must
    expose `async spawn(meeting_id)` and `async kill()`. `session_alive_probe`
    reports whether a copilot tmux session already exists — used only for
    reconciliation after a supervisor restart.
    """

    INITIAL_BACKOFF_S = 1.0
    MAX_BACKOFF_S = 30.0
    LOG_THROTTLE_S = 60.0

    def __init__(
        self,
        bridge_url: str = DEFAULT_BRIDGE_URL,
        *,
        runner=None,
        grace_s: Optional[float] = None,
        autospawn: Optional[bool] = None,
        session_alive_probe: Optional[Callable[[], bool]] = None,
        log_prefix: str = "[copilot-supervisor]",
    ) -> None:
        self.bridge_url = bridge_url
        self.runner = runner
        self.lifecycle = SessionLifecycle()
        self.grace_s = (
            grace_s
            if grace_s is not None
            else float(os.environ.get("RECOG_COPILOT_GRACE_S", str(DEFAULT_GRACE_S)))
        )
        self.autospawn = (
            autospawn
            if autospawn is not None
            else _env_flag("RECOG_COPILOT_AUTOSPAWN", True)
        )
        self._session_alive_probe = session_alive_probe or (lambda: False)
        self._log_prefix = log_prefix
        self._grace_task: Optional[asyncio.Task] = None
        self._task: Optional[asyncio.Task] = None
        self._stop_event: Optional[asyncio.Event] = None
        self._connected_once = False
        self._last_failure_log_ts: float = 0.0
        self._failure_streak = 0

    # --- lifecycle event plumbing ---

    async def handle_frame(self, data: dict) -> None:
        if not isinstance(data, dict):
            return
        if not self.autospawn:
            return
        ftype = data.get("type")
        if ftype == "meeting_state":
            await self._apply(
                self._reconcile(bool(data.get("active")), data.get("meeting_id"))
            )
        elif ftype == "meeting_started":
            meeting_id = data.get("meeting_id")
            if meeting_id:
                await self._apply(self.lifecycle.meeting_started(str(meeting_id)))
        elif ftype == "meeting_ended":
            await self._apply(self.lifecycle.meeting_ended(data.get("meeting_id")))

    def _reconcile(self, active: bool, meeting_id: Optional[str]) -> tuple:
        """React to the bridge's `meeting_state` reply after (re)connect.

        Three branches (see plan, Task 1):
        - session exists + no active meeting → synthetic `meeting_ended`:
          RUNNING→GRACE, NOT an instant kill. After a bridge restart
          `active=false` is transient (the extension has not re-sent
          `meeting_join` yet); grace gives it time to bring the same
          meeting back, and the same-id flap keeps the session.
        - no session + meeting active → spawn.
        - session alive + meeting active → adopt (no respawn): a supervisor
          restart mid-meeting must not kill a live copilot.
        """
        machine = self.lifecycle
        session_exists = machine.state is not State.IDLE or self._session_alive_probe()

        if active and meeting_id:
            if not session_exists:
                return machine.meeting_started(str(meeting_id))
            if machine.state is State.IDLE:
                return machine.adopt(str(meeting_id))
            # Machine already tracks a session: same id is a no-op / GRACE
            # revival, different id is a kill+spawn — exactly meeting_started.
            return machine.meeting_started(str(meeting_id))

        if session_exists:
            if machine.state is State.IDLE:
                machine.adopt(None)
            return machine.meeting_ended(machine.meeting_id)
        return ()

    async def _apply(self, commands: tuple) -> None:
        self._sync_grace_timer()
        await self._execute(commands)

    async def _execute(self, commands: tuple) -> None:
        for command in commands:
            try:
                if isinstance(command, Kill):
                    if self.runner is not None:
                        await self.runner.kill()
                elif isinstance(command, Spawn):
                    if self.runner is not None:
                        await self.runner.spawn(command.meeting_id)
            except Exception as exc:
                print(
                    f"{self._log_prefix} command {command!r} failed: {exc!r}",
                    file=sys.stderr,
                )

    # --- grace timer ---

    def _sync_grace_timer(self) -> None:
        if self.lifecycle.state is State.GRACE:
            if self._grace_task is None or self._grace_task.done():
                self._grace_task = asyncio.create_task(self._grace_expire())
        elif self._grace_task is not None:
            self._grace_task.cancel()
            self._grace_task = None

    async def _grace_expire(self) -> None:
        await asyncio.sleep(self.grace_s)
        self._grace_task = None
        commands = self.lifecycle.timer_expired()
        await self._execute(commands)

    # --- bridge connection (reconnect loop modelled on BridgeSubscriber) ---

    async def start(self) -> None:
        """Spawn the run loop on the current event loop."""
        if self._task is not None:
            return
        self._stop_event = asyncio.Event()
        self._task = asyncio.create_task(self.run())

    async def stop(self) -> None:
        if self._stop_event is not None:
            self._stop_event.set()
        if self._grace_task is not None:
            self._grace_task.cancel()
            self._grace_task = None
        if self._task is not None:
            self._task.cancel()
            try:
                await asyncio.wait_for(self._task, timeout=5.0)
            except (asyncio.CancelledError, asyncio.TimeoutError):
                pass
            except Exception as exc:
                print(f"{self._log_prefix} stop raised: {exc!r}", file=sys.stderr)
            self._task = None
        self._stop_event = None

    async def run(self) -> None:
        delay = self.INITIAL_BACKOFF_S
        if self._stop_event is None:
            self._stop_event = asyncio.Event()
        while not self._stop_event.is_set():
            try:
                async with websockets.connect(self.bridge_url) as ws:
                    # Announce ourselves so the bridge moves this socket into
                    # its supervisor set (does not count as an extension
                    # client) and replies with `meeting_state` for
                    # reconciliation. A bridge that predates the protocol
                    # simply never replies — lifecycle frames still arrive
                    # once Task 4 lands.
                    await ws.send(json.dumps({"type": "supervisor_hello"}))
                    self._on_connected()
                    delay = self.INITIAL_BACKOFF_S
                    async for raw in ws:
                        data = self._decode(raw)
                        if data is not None:
                            await self.handle_frame(data)
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

    @staticmethod
    def _decode(raw) -> Optional[dict]:
        if isinstance(raw, (bytes, bytearray)):
            try:
                raw = raw.decode("utf-8")
            except UnicodeDecodeError:
                return None
        if not isinstance(raw, str):
            return None
        try:
            data = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return None
        return data if isinstance(data, dict) else None

    def _on_connected(self) -> None:
        self._failure_streak = 0
        verb = "reconnected" if self._connected_once else "connected"
        self._connected_once = True
        print(f"{self._log_prefix} {verb} to {self.bridge_url}", file=sys.stderr)

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


def main(argv: Optional[list] = None) -> None:
    parser = argparse.ArgumentParser(
        description="Auto-spawn the Copilot Claude session per KTalk meeting"
    )
    parser.add_argument(
        "--bridge-url",
        default=DEFAULT_BRIDGE_URL,
        help=f"ktalk_bridge WebSocket URL (default: {DEFAULT_BRIDGE_URL})",
    )
    args = parser.parse_args(argv)
    supervisor = Supervisor(args.bridge_url)
    if not supervisor.autospawn:
        print(
            "[copilot-supervisor] RECOG_COPILOT_AUTOSPAWN is off — "
            "connected but not spawning sessions",
            file=sys.stderr,
        )
    try:
        asyncio.run(supervisor.run())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
