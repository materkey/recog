"""Supervisor daemon that auto-spawns the Copilot Claude session per KTalk meeting.

Connects to ktalk_bridge as a WS client (`supervisor_hello`, carrying the id of
the session it currently intends to keep alive), listens for `meeting_started` /
`meeting_ended` lifecycle frames and drives a detached tmux session with the
Claude Code copilot (spawn on meeting start, kill after a grace period on
meeting end). See `docs/plans/2026-07-02-auto-copilot-session.md`.

The webapp can also drive the same machine by hand: `copilot_spawn_request` /
`copilot_stop_request` (relayed by the bridge) map onto the synthetic meeting id
`MANUAL_MEETING_ID`, so a repeated spawn is the existing same-id flap and a real
conference preempts the manual session as any back-to-back meeting would. See
`docs/plans/20260724-webapp-copilot-live.md`.

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
import inspect
import json
import os
import shlex
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import websockets


DEFAULT_BRIDGE_URL = "ws://127.0.0.1:8766"
DEFAULT_GRACE_S = 90.0
DEFAULT_MCP_URL = "http://127.0.0.1:8767/mcp"
TMUX_SOCKET = "recog-copilot"
TMUX_SESSION = "copilot"
REPO_ROOT = Path(__file__).resolve().parent.parent
# Synthetic lifecycle id for a webapp-driven (manual) copilot session. Twin of
# `ktalk_bridge.MANUAL_MEETING_ID` — the value must not drift, the bridge keys
# its manual mirror and `meeting_state` reply off the very same string. Not
# imported from there on purpose: ktalk_bridge is a heavy top-level module and
# importing it here would invert the layering.
MANUAL_MEETING_ID = "manual"


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


# --- tmux driver ---


async def _run_subprocess(args: list) -> tuple:
    """Default subprocess runner: (returncode, combined stdout+stderr text)."""
    proc = await asyncio.create_subprocess_exec(
        *args,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )
    out, _ = await proc.communicate()
    return proc.returncode, out.decode("utf-8", errors="replace")


class TmuxRunner:
    """Drives the copilot Claude session in detached tmux on a dedicated
    socket (`tmux -L recog-copilot`), isolated from the user's personal tmux.

    The subprocess runner is injectable for tests: an async callable
    `run(args) -> (returncode, output)`.
    """

    KILL_RETRIES = 3
    KILL_RETRY_DELAY_S = 0.5

    def __init__(
        self,
        *,
        repo_root: Optional[Path] = None,
        mcp_url: Optional[str] = None,
        run: Optional[Callable] = None,
        socket_name: str = TMUX_SOCKET,
        session_name: str = TMUX_SESSION,
    ) -> None:
        self.repo_root = Path(repo_root) if repo_root is not None else REPO_ROOT
        self.mcp_url = mcp_url or os.environ.get("RECOG_COPILOT_MCP_URL", DEFAULT_MCP_URL)
        self._run = run or _run_subprocess
        self.socket_name = socket_name
        self.session_name = session_name

    def _tmux(self, *args: str) -> list:
        return ["tmux", "-L", self.socket_name, *args]

    def build_claude_command(self) -> str:
        """Claude invocation for `tmux new-session` (single shell string).

        `--strict-mcp-config --mcp-config` limits the session to built-ins +
        the copilot daemon (no user MCP servers); the dev-channels flag makes
        the daemon's channel pushes reach the TUI (see plan Context).
        """
        mcp_config = json.dumps(
            {"mcpServers": {"copilot": {"type": "http", "url": self.mcp_url}}}
        )
        return shlex.join(
            [
                "claude",
                "--agent",
                "copilot-session",
                "--strict-mcp-config",
                "--mcp-config",
                mcp_config,
                "--dangerously-load-development-channels",
                "server:copilot",
            ]
        )

    async def spawn(self, meeting_id: Optional[str] = None) -> bool:
        """Start the copilot session; adopt a live one instead of erroring.

        tmux treats a duplicate session name as an error, which would burn a
        respawn-backoff attempt for nothing — spawn is gated on `alive()`.
        Returns True if a new session was created, False if adopted.
        """
        if await self.alive():
            return False
        rc, out = await self._run(
            self._tmux(
                "new-session",
                "-d",
                "-s",
                self.session_name,
                "-c",
                str(self.repo_root),
                self.build_claude_command(),
            )
        )
        if rc != 0:
            raise RuntimeError(f"tmux new-session failed (rc={rc}): {out.strip()}")
        return True

    async def kill(self) -> None:
        """Kill the copilot session; a non-zero rc is verified against
        `has-session` before retrying or raising (DW-12). A zero rc is
        trusted as-is.

        A non-zero rc with the session already gone stays a success — kill is
        idempotent (D5: no graceful stop). A non-zero rc while `has-session`
        still succeeds is retried up to KILL_RETRIES times and finally raised
        as RuntimeError, so a failed kill surfaces in `_execute`'s per-command
        error log instead of silently orphaning a live Copilot session.
        """
        rc, out = 0, ""
        for attempt in range(self.KILL_RETRIES):
            if attempt:
                await asyncio.sleep(self.KILL_RETRY_DELAY_S)
            rc, out = await self._run(
                self._tmux("kill-session", "-t", self.session_name)
            )
            if rc == 0:
                return
            if not await self.alive():
                return
        raise RuntimeError(f"tmux kill-session failed (rc={rc}): {out.strip()}")

    async def alive(self) -> bool:
        rc, _ = await self._run(self._tmux("has-session", "-t", self.session_name))
        return rc == 0

    async def send_keys(self, *keys: str) -> None:
        await self._run(self._tmux("send-keys", "-t", self.session_name, *keys))

    async def capture(self) -> str:
        rc, out = await self._run(
            self._tmux("capture-pane", "-p", "-t", self.session_name)
        )
        return out if rc == 0 else ""


# --- screen classification (watchdog) ---


class AgentState(enum.Enum):
    SPAWNING = "spawning"
    ONLINE = "online"
    WORKING = "working"
    BLOCKED = "blocked"
    OFFLINE = "offline"


# Markers frozen on claude 2.1.197 rendered screens (spike fixtures); after a
# claude update re-capture the fixtures and re-run the classify tests.
BLOCKED_MARKERS = ("enter to confirm", "esc to cancel", "❯ 1.")
WORKING_MARKER = "esc to interrupt"
PROMPT_CHAR = "❯"
DEV_CHANNELS_MARKER = "local development"
PROMPT_TAIL_LINES = 15


def classify_screen(text: Optional[str]) -> AgentState:
    """Map a captured tmux pane to an agent state.

    Order is fixed — first match wins (see plan Technical Details):
    no session → dialog footer → spinner → prompt → transitional.
    `None` means the tmux session itself is gone; an *empty* pane is a
    transitional startup screen, not offline.
    """
    if text is None:
        return AgentState.OFFLINE
    lowered = text.lower()
    if any(marker in lowered for marker in BLOCKED_MARKERS):
        return AgentState.BLOCKED
    if WORKING_MARKER in lowered:
        return AgentState.WORKING
    tail = text.rstrip().splitlines()[-PROMPT_TAIL_LINES:]
    if any(PROMPT_CHAR in line for line in tail):
        return AgentState.ONLINE
    return AgentState.SPAWNING


class Watchdog:
    """Polls the copilot tmux screen while a session is expected to live.

    Each tick: classify the pane (`offline` when the session is gone),
    auto-dismiss the known dev-channels dialog (`send-keys 1`, once per
    dialog occurrence, reported as `spawning` — not `blocked`), report
    `copilot_agent_status` to the bridge on every state change plus an
    idempotent heartbeat every ~HEARTBEAT_S (the bridge merge measures
    freshness by it), and feed `session_ready` / `session_died` back into
    the lifecycle. A screen stuck in `spawning` past SPAWN_DEADLINE_S is
    reported as `blocked` (fail-visible; respawn would not help).
    """

    POLL_INTERVAL_S = 2.0
    HEARTBEAT_S = 5.0
    SPAWN_DEADLINE_S = 60.0

    def __init__(
        self,
        *,
        runner,
        lifecycle: SessionLifecycle,
        send_status: Callable,
        on_session_died: Callable,
        clock: Callable[[], float] = time.monotonic,
        spawn_in_flight: Callable[[], bool] = lambda: False,
    ) -> None:
        self.runner = runner
        self.lifecycle = lifecycle
        self._send_status = send_status
        self._on_session_died = on_session_died
        self._clock = clock
        self._spawn_in_flight = spawn_in_flight
        self._last_reported: Optional[AgentState] = None
        self._last_report_ts: float = 0.0
        self._dialog_dismissed = False
        self._spawning_since: Optional[float] = None

    async def run(self) -> None:
        while True:
            try:
                await self.tick()
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                print(
                    f"[copilot-supervisor] watchdog tick failed: {exc!r}",
                    file=sys.stderr,
                )
            await asyncio.sleep(self.POLL_INTERVAL_S)

    async def tick(self) -> None:
        now = self._clock()
        if self.lifecycle.state is State.IDLE:
            # No session should exist: no probing, no heartbeats (the bridge
            # merge treats silence as offline) — just close out the chip once.
            self._dialog_dismissed = False
            self._spawning_since = None
            if self._last_reported not in (None, AgentState.OFFLINE):
                await self._report(AgentState.OFFLINE, now)
            return

        observed = await self._observe()
        if self._spawn_in_flight():
            # A spawn command is queued or executing: any visible pane still
            # belongs to the previous session (or to an incompletely-created
            # replacement), so it cannot mark the replacement ready yet.
            # Treat every observation as SPAWNING until the command settles;
            # a genuine post-spawn state is observed once the flag clears.
            observed = AgentState.SPAWNING
        if observed in (AgentState.ONLINE, AgentState.WORKING):
            self.lifecycle.session_ready()

        effective = observed
        if observed is AgentState.SPAWNING:
            if self._spawning_since is None:
                self._spawning_since = now
            if now - self._spawning_since >= self.SPAWN_DEADLINE_S:
                effective = AgentState.BLOCKED
        else:
            self._spawning_since = None

        if effective is not self._last_reported or (
            now - self._last_report_ts >= self.HEARTBEAT_S
        ):
            await self._report(effective, now)

        if observed is AgentState.OFFLINE:
            await self._on_session_died()

    async def _observe(self) -> AgentState:
        if not await self.runner.alive():
            self._dialog_dismissed = False
            return AgentState.OFFLINE
        text = await self.runner.capture()
        state = classify_screen(text)
        if state is AgentState.BLOCKED and DEV_CHANNELS_MARKER in text.lower():
            if not self._dialog_dismissed:
                self._dialog_dismissed = True
                await self.runner.send_keys("1")
            return AgentState.SPAWNING
        self._dialog_dismissed = False
        return state

    async def _report(self, state: AgentState, now: float) -> None:
        self._last_reported = state
        self._last_report_ts = now
        await self._send_status(state)


class Supervisor:
    """WS glue around SessionLifecycle: bridge connection, grace timer, runner.

    `runner` (spawn/kill target, TmuxRunner in production) must expose
    `async spawn(meeting_id)` and `async kill()`. `session_alive_probe`
    reports whether a copilot tmux session already exists — used only for
    reconciliation after a supervisor restart; defaults to `runner.alive`
    when the runner provides one.
    """

    INITIAL_BACKOFF_S = 1.0
    MAX_BACKOFF_S = 30.0
    LOG_THROTTLE_S = 60.0
    RESPAWN_BACKOFF_S = (5.0, 15.0, 30.0)  # ≤3 respawn attempts per meeting

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
        if session_alive_probe is not None:
            self._session_alive_probe = session_alive_probe
        elif runner is not None and hasattr(runner, "alive"):
            self._session_alive_probe = runner.alive
        else:
            self._session_alive_probe = lambda: False
        self._log_prefix = log_prefix
        self._respawn_attempts = 0
        self._respawn_exhausted_logged = False
        self._respawn_task: Optional[asyncio.Task] = None
        self._grace_task: Optional[asyncio.Task] = None
        self._task: Optional[asyncio.Task] = None
        self._watchdog_task: Optional[asyncio.Task] = None
        self._stop_event: Optional[asyncio.Event] = None
        self._connected_once = False
        self._last_failure_log_ts: float = 0.0
        self._failure_streak = 0
        self._ws = None
        # True while `_execute` is running a spawn (and any preceding kill):
        # during that window the tmux session is transiently absent, so the
        # watchdog must not read OFFLINE as a death (see Watchdog.tick).
        self._spawn_in_flight = False
        self._spawn_in_flight_count = 0
        # Serialises command execution. `_grace_expire` holds it across its
        # stale-Kill re-check + kill, so whichever of {grace-expiry Kill,
        # frame-driven Spawn} acquires first fully settles before the other
        # observes the world (DW-10). Loop-agnostic on Python 3.12.
        self._execute_lock = asyncio.Lock()
        if runner is not None and all(
            hasattr(runner, name) for name in ("alive", "capture", "send_keys")
        ):
            self.watchdog: Optional[Watchdog] = Watchdog(
                runner=runner,
                lifecycle=self.lifecycle,
                send_status=self._send_agent_status,
                on_session_died=self.on_session_died,
                spawn_in_flight=lambda: self._spawn_in_flight,
            )
        else:
            self.watchdog = None

    # --- lifecycle event plumbing ---

    async def handle_frame(self, data: dict) -> None:
        """Drive the lifecycle from one bridge frame.

        `autospawn` (RECOG_COPILOT_AUTOSPAWN) gates only the three
        meeting-driven branches (D3). The manual frames are an explicit user
        click relayed by the bridge, so they run regardless of the flag; the
        bridge already rejected a spawn during a live conference, and
        `request_id` is its business — the supervisor answers only through
        `copilot_agent_status`.
        """
        if not isinstance(data, dict):
            return
        ftype = data.get("type")
        if ftype == "meeting_state":
            if not self.autospawn:
                return
            session_alive = await self._probe_session_alive()
            await self._apply(
                self._reconcile(
                    bool(data.get("active")), data.get("meeting_id"), session_alive
                )
            )
        elif ftype == "meeting_started":
            if not self.autospawn:
                return
            meeting_id = data.get("meeting_id")
            if meeting_id:
                await self._apply(self.lifecycle.meeting_started(str(meeting_id)))
        elif ftype == "meeting_ended":
            if not self.autospawn:
                return
            await self._apply(self.lifecycle.meeting_ended(data.get("meeting_id")))
        elif ftype == "copilot_spawn_request":
            # An explicit click is fresh user intent, not a flap: reset the
            # respawn budget here. A session that burned all its attempts parks
            # in SPAWNING("manual"), where the same-id flap emits no `Spawn` —
            # so `_apply` would never reset it and Start would stay inert
            # forever. Preserve an already-pending retry, though: cancelling it
            # would make a repeated click postpone recovery.
            if self._respawn_task is None or self._respawn_task.done():
                self._reset_respawn_backoff()
            await self._apply(self.lifecycle.meeting_started(MANUAL_MEETING_ID))
        elif ftype == "copilot_stop_request":
            await self._apply(self.lifecycle.meeting_ended(MANUAL_MEETING_ID))

    async def _probe_session_alive(self) -> bool:
        try:
            result = self._session_alive_probe()
            if inspect.isawaitable(result):
                result = await result
            return bool(result)
        except Exception as exc:
            print(f"{self._log_prefix} session probe failed: {exc!r}", file=sys.stderr)
            return False

    def _reconcile(
        self, active: bool, meeting_id: Optional[str], session_alive: bool
    ) -> tuple:
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
        session_exists = machine.state is not State.IDLE or session_alive

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
        if any(isinstance(command, Spawn) for command in commands):
            # A meeting-driven spawn starts a fresh session: reset the respawn
            # budget and drop any pending respawn left over from the old one.
            self._reset_respawn_backoff()
        self._sync_grace_timer()
        await self._execute(commands)

    async def _execute(self, commands: tuple) -> None:
        if not commands:
            return
        # Mark queued spawns before waiting for the lock: the lifecycle has
        # already entered SPAWNING, so a watchdog tick must treat an OFFLINE
        # session as the expected replacement gap even while another command
        # still owns the execution lock. A counter keeps overlapping callers
        # from clearing the shared flag out from under each other.
        spawning = any(isinstance(command, Spawn) for command in commands)
        if spawning:
            self._spawn_in_flight_count += 1
            self._spawn_in_flight = True
        try:
            async with self._execute_lock:
                await self._execute_locked(commands)
        finally:
            if spawning:
                self._spawn_in_flight_count -= 1
                self._spawn_in_flight = self._spawn_in_flight_count > 0

    async def _execute_locked(self, commands: tuple) -> None:
        # Callers must hold `_execute_lock`.
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

    def _live_meeting_id(self) -> Optional[str]:
        """`supervisor_hello.current_meeting_id` (D5): the id of a session the
        machine intends to keep alive. GRACE/IDLE report None — a session
        already scheduled to die must not be re-armed by the bridge's
        reconcile reply (that would let a reconnect inside the grace window
        defeat the user's Stop).
        """
        if self.lifecycle.state in (State.SPAWNING, State.RUNNING):
            return self.lifecycle.meeting_id
        return None

    async def _send_agent_status(self, status: AgentState) -> None:
        """Report `copilot_agent_status` to the bridge (watchdog callback).

        Dropped silently while disconnected — the bridge merge treats
        supervisor silence as offline, which is the truthful state anyway.
        """
        ws = self._ws
        if ws is None:
            return
        frame = {
            "type": "copilot_agent_status",
            "status": status.value,
            "meeting_id": self.lifecycle.meeting_id,
        }
        try:
            await ws.send(json.dumps(frame))
        except Exception as exc:
            print(f"{self._log_prefix} status send failed: {exc!r}", file=sys.stderr)

    # --- respawn with backoff (session_died input from the watchdog) ---

    async def on_session_died(self) -> None:
        """The copilot tmux session vanished while it should be alive.

        Respawns with per-meeting backoff (`RESPAWN_BACKOFF_S`, ≤3 attempts).
        A pending respawn absorbs repeated reports; the budget resets on
        every fresh meeting-driven spawn (see `_apply`).

        Not gated on `autospawn` (D3): this path only fires for a session the
        machine already decided should be alive — including a manual one
        started with the flag off. The IDLE machine, not the flag, is what
        keeps a dead session dead (`session_died` yields no `Spawn` there).
        """
        if self._respawn_task is not None and not self._respawn_task.done():
            return
        commands = self.lifecycle.session_died()
        self._sync_grace_timer()
        if not any(isinstance(command, Spawn) for command in commands):
            return
        if self._respawn_attempts >= len(self.RESPAWN_BACKOFF_S):
            if not self._respawn_exhausted_logged:
                self._respawn_exhausted_logged = True
                print(
                    f"{self._log_prefix} respawn attempts exhausted for meeting "
                    f"{self.lifecycle.meeting_id!r} — leaving session down",
                    file=sys.stderr,
                )
            return
        delay = self.RESPAWN_BACKOFF_S[self._respawn_attempts]
        self._respawn_attempts += 1
        self._respawn_task = asyncio.create_task(
            self._respawn_after(delay, self.lifecycle.meeting_id, commands)
        )

    async def _respawn_after(
        self, delay: float, meeting_id: Optional[str], commands: tuple
    ) -> None:
        await asyncio.sleep(delay)
        self._respawn_task = None
        # The meeting may have ended or changed during the backoff sleep —
        # only respawn if the machine still waits for this very session.
        if (
            self.lifecycle.state is State.SPAWNING
            and self.lifecycle.meeting_id == meeting_id
        ):
            await self._execute(commands)

    def _reset_respawn_backoff(self) -> None:
        self._respawn_attempts = 0
        self._respawn_exhausted_logged = False
        if self._respawn_task is not None:
            self._respawn_task.cancel()
            self._respawn_task = None

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
        if not commands:
            return
        async with self._execute_lock:
            # A frame-driven spawn/adopt may have claimed the still-live
            # session while this task waited for the lock (state left IDLE);
            # executing the queued Kill then would destroy it (DW-10).
            if self.lifecycle.state is not State.IDLE:
                return
            await self._execute_locked(commands)

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
        if self._respawn_task is not None:
            self._respawn_task.cancel()
            self._respawn_task = None
        self._stop_watchdog()
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
        # The watchdog polls tmux for the supervisor's whole lifetime; it
        # sends status only while `self._ws` is connected and drops silently
        # otherwise (bridge merge reads silence as offline).
        self._start_watchdog()
        try:
            while not self._stop_event.is_set():
                try:
                    async with websockets.connect(self.bridge_url) as ws:
                        # Announce ourselves so the bridge moves this socket
                        # into its supervisor set (does not count as an
                        # extension client) and replies with `meeting_state`
                        # for reconciliation. A bridge that predates the
                        # protocol simply never replies — lifecycle frames
                        # still arrive once Task 4 lands.
                        # `current_meeting_id` lets a restarted bridge re-arm
                        # its manual mirror from the only runtime owner of that
                        # truth — us (D5) — so its reconcile reply adopts a live
                        # manual session instead of GRACE-killing it.
                        self._ws = ws
                        await ws.send(
                            json.dumps(
                                {
                                    "type": "supervisor_hello",
                                    "current_meeting_id": self._live_meeting_id(),
                                }
                            )
                        )
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
                finally:
                    self._ws = None

                if self._stop_event.is_set():
                    return
                try:
                    await asyncio.wait_for(self._stop_event.wait(), timeout=delay)
                    return
                except asyncio.TimeoutError:
                    pass
                delay = min(delay * 2, self.MAX_BACKOFF_S)
        finally:
            self._stop_watchdog()

    def _start_watchdog(self) -> None:
        if self.watchdog is None:
            return
        if self._watchdog_task is None or self._watchdog_task.done():
            self._watchdog_task = asyncio.create_task(self.watchdog.run())

    def _stop_watchdog(self) -> None:
        if self._watchdog_task is not None:
            self._watchdog_task.cancel()
            self._watchdog_task = None

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
    supervisor = Supervisor(args.bridge_url, runner=TmuxRunner())
    if not supervisor.autospawn:
        print(
            "[copilot-supervisor] RECOG_COPILOT_AUTOSPAWN is off — "
            "meeting-driven auto-spawn disabled; manual webapp spawn/stop "
            "and crash-respawn still work",
            file=sys.stderr,
        )
    try:
        asyncio.run(supervisor.run())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
