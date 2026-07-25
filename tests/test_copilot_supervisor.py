"""Tests for recog/copilot_supervisor.py — lifecycle state machine + bridge glue."""
import asyncio
import json
import shlex
import sys
from pathlib import Path

import pytest
import websockets

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.copilot_supervisor import (
    DEFAULT_MCP_URL,
    MANUAL_MEETING_ID,
    AgentState,
    Kill,
    SessionLifecycle,
    Spawn,
    State,
    Supervisor,
    TmuxRunner,
    Watchdog,
    classify_screen,
)


class FakeRunner:
    """Records spawn/kill calls issued by the supervisor glue."""

    def __init__(self):
        self.spawned = []
        self.kills = 0

    async def spawn(self, meeting_id):
        self.spawned.append(meeting_id)

    async def kill(self):
        self.kills += 1


class FakeTmux:
    """Injectable subprocess runner recording tmux invocations."""

    def __init__(self, has_session=False):
        self.calls = []
        self.has_session = has_session
        self.responses = {}  # tmux subcommand -> (rc, stdout)

    async def __call__(self, args):
        self.calls.append(list(args))
        sub = args[3]
        if sub in self.responses:
            return self.responses[sub]
        if sub == "has-session":
            return (0, "") if self.has_session else (1, "can't find session: copilot")
        return (0, "")

    def subcommands(self):
        return [call[3] for call in self.calls]


# ---------------------------------------------------------------------------
# SessionLifecycle: pure state machine, table-driven
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "start_state,start_mid,event,args,end_state,end_mid,commands",
    [
        # IDLE + meeting_started → SPAWNING + spawn
        (State.IDLE, None, "meeting_started", ("A",), State.SPAWNING, "A", (Spawn("A"),)),
        # flap same id: GRACE → RUNNING, NO kill (session survives)
        (State.GRACE, "A", "meeting_started", ("A",), State.RUNNING, "A", ()),
        # meeting change from GRACE: kill + spawn new
        (State.GRACE, "A", "meeting_started", ("B",), State.SPAWNING, "B", (Kill(), Spawn("B"))),
        # meeting change from RUNNING (back-to-back meetings): kill + spawn
        (State.RUNNING, "A", "meeting_started", ("B",), State.SPAWNING, "B", (Kill(), Spawn("B"))),
        # duplicate meeting_started while RUNNING same id: no-op
        (State.RUNNING, "A", "meeting_started", ("A",), State.RUNNING, "A", ()),
        # duplicate meeting_started while SPAWNING same id: no-op
        (State.SPAWNING, "A", "meeting_started", ("A",), State.SPAWNING, "A", ()),
        # meeting change while still SPAWNING: kill + spawn new
        (State.SPAWNING, "A", "meeting_started", ("B",), State.SPAWNING, "B", (Kill(), Spawn("B"))),
        # adopted session (unknown id) revived from GRACE by any meeting id
        (State.GRACE, None, "meeting_started", ("A",), State.RUNNING, "A", ()),
        # meeting_ended from RUNNING → GRACE (no kill yet — timer decides)
        (State.RUNNING, "A", "meeting_ended", ("A",), State.GRACE, "A", ()),
        # meeting_ended while SPAWNING (short meeting) → GRACE
        (State.SPAWNING, "A", "meeting_ended", ("A",), State.GRACE, "A", ()),
        # meeting_ended for a different id is stale — ignored
        (State.RUNNING, "A", "meeting_ended", ("B",), State.RUNNING, "A", ()),
        # meeting_ended in IDLE: no-op
        (State.IDLE, None, "meeting_ended", ("A",), State.IDLE, None, ()),
        # grace expiry → kill + IDLE
        (State.GRACE, "A", "timer_expired", (), State.IDLE, None, (Kill(),)),
        # stale timer outside GRACE: no-op
        (State.RUNNING, "A", "timer_expired", (), State.RUNNING, "A", ()),
        (State.IDLE, None, "timer_expired", (), State.IDLE, None, ()),
        # session died mid-meeting → respawn same meeting
        (State.RUNNING, "A", "session_died", (), State.SPAWNING, "A", (Spawn("A"),)),
        # session died while spawning → respawn attempt
        (State.SPAWNING, "A", "session_died", (), State.SPAWNING, "A", (Spawn("A"),)),
        # session died during grace → IDLE, nothing to kill
        (State.GRACE, "A", "session_died", (), State.IDLE, None, ()),
        # session died in IDLE: no-op
        (State.IDLE, None, "session_died", (), State.IDLE, None, ()),
        # watchdog confirms session up: SPAWNING → RUNNING
        (State.SPAWNING, "A", "session_ready", (), State.RUNNING, "A", ()),
        # session_ready outside SPAWNING: no-op
        (State.RUNNING, "A", "session_ready", (), State.RUNNING, "A", ()),
    ],
)
def test_lifecycle_transitions(start_state, start_mid, event, args, end_state, end_mid, commands):
    machine = SessionLifecycle()
    machine.state = start_state
    machine.meeting_id = start_mid

    result = getattr(machine, event)(*args)

    assert result == commands
    assert machine.state is end_state
    assert machine.meeting_id == end_mid


def test_lifecycle_full_meeting_cycle():
    """given: a fresh machine
    when: meeting starts, session comes up, meeting ends, grace expires
    then: commands are spawn → (none) → (none) → kill and state returns to IDLE
    """
    machine = SessionLifecycle()
    assert machine.meeting_started("A") == (Spawn("A"),)
    assert machine.session_ready() == ()
    assert machine.state is State.RUNNING
    assert machine.meeting_ended("A") == ()
    assert machine.state is State.GRACE
    assert machine.timer_expired() == (Kill(),)
    assert machine.state is State.IDLE


def test_lifecycle_flap_preserves_session():
    """given: RUNNING(A) went to GRACE on meeting_ended
    when: meeting_started(A) arrives within grace
    then: no kill/spawn commands at any point — the session lives through the flap
    """
    machine = SessionLifecycle()
    machine.meeting_started("A")
    machine.session_ready()
    assert machine.meeting_ended("A") == ()
    assert machine.meeting_started("A") == ()
    assert machine.state is State.RUNNING
    assert machine.meeting_id == "A"


def test_lifecycle_adopt_attaches_without_commands():
    machine = SessionLifecycle()
    assert machine.adopt("A") == ()
    assert machine.state is State.RUNNING
    assert machine.meeting_id == "A"


# ---------------------------------------------------------------------------
# Supervisor glue: grace timer, autospawn switch
# ---------------------------------------------------------------------------


async def test_grace_expiry_kills_session():
    """given: a running session whose meeting ended
    when: the grace timer expires
    then: the runner receives kill() and lifecycle returns to IDLE
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=0.05, autospawn=True)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    assert runner.spawned == ["A"]
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    assert sup.lifecycle.state is State.GRACE
    assert runner.kills == 0

    await asyncio.sleep(0.15)
    assert runner.kills == 1
    assert sup.lifecycle.state is State.IDLE


async def test_flap_within_grace_cancels_kill():
    """given: meeting ended and grace timer is running
    when: the same meeting id starts again before expiry
    then: the timer is cancelled, session is never killed
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=0.1, autospawn=True)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    assert sup.lifecycle.state is State.RUNNING

    await asyncio.sleep(0.25)
    assert runner.kills == 0
    assert runner.spawned == ["A"]  # only the initial spawn


async def test_meeting_change_from_grace_kills_and_respawns():
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    await sup.handle_frame({"type": "meeting_started", "meeting_id": "B"})

    assert runner.kills == 1
    assert runner.spawned == ["A", "B"]
    assert sup.lifecycle.state is State.SPAWNING
    assert sup.lifecycle.meeting_id == "B"
    assert sup._grace_task is None  # timer cancelled on leaving GRACE
    await sup.stop()


async def test_stale_grace_kill_skipped_when_spawn_wins_race():
    """given: a GRACE machine whose expired timer queued a Kill that is parked
           on the execute lock (state already flipped to IDLE)
    when: a meeting_started frame claims the still-live session before the
          stale Kill acquires the lock
    then: the Kill is skipped — the runner records zero kill() calls and the
          session survives under the new meeting id (DW-10)
    """
    fake_tmux = FakeTmux(has_session=True)
    sup = Supervisor(
        runner=TmuxRunner(run=fake_tmux), grace_s=0.05, autospawn=True
    )

    sup.lifecycle.adopt("A")
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    assert sup.lifecycle.state is State.GRACE
    grace_task = sup._grace_task

    async with sup._execute_lock:
        # Grace expires while the lock is held elsewhere: timer_expired flips
        # the machine to IDLE and the queued Kill parks on the lock.
        for _ in range(100):
            if sup.lifecycle.state is State.IDLE:
                break
            await asyncio.sleep(0.01)
        assert sup.lifecycle.state is State.IDLE
        # The frame takes the IDLE branch synchronously; its Spawn queues on
        # the lock behind the parked stale Kill.
        frame_task = asyncio.create_task(
            sup.handle_frame({"type": "meeting_started", "meeting_id": "B"})
        )
        for _ in range(100):
            if sup.lifecycle.state is State.SPAWNING:
                break
            await asyncio.sleep(0.01)
        assert sup.lifecycle.state is State.SPAWNING

    await asyncio.wait_for(frame_task, timeout=1.0)
    await asyncio.wait_for(grace_task, timeout=1.0)

    assert fake_tmux.subcommands() == ["has-session"]
    assert fake_tmux.has_session is True  # B adopted the surviving session
    assert sup.lifecycle.state is State.SPAWNING
    assert sup.lifecycle.meeting_id == "B"
    await sup.stop()


async def test_stale_grace_kill_skipped_when_claim_reaches_running():
    """given: the stale grace Kill is parked on the execute lock and a
           meeting_started frame claimed the still-live session (IDLE branch)
    when: the watchdog confirms the session up (SPAWNING → RUNNING) before
          the parked Kill acquires the lock
    then: the re-check still skips the Kill — the guard covers every
          non-IDLE claim, not just SPAWNING (DW-10)
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=0.05, autospawn=True)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    sup.lifecycle.session_ready()
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    assert sup.lifecycle.state is State.GRACE
    grace_task = sup._grace_task

    async with sup._execute_lock:
        for _ in range(100):
            if sup.lifecycle.state is State.IDLE:
                break
            await asyncio.sleep(0.01)
        assert sup.lifecycle.state is State.IDLE
        frame_task = asyncio.create_task(
            sup.handle_frame({"type": "meeting_started", "meeting_id": "B"})
        )
        for _ in range(100):
            if sup.lifecycle.state is State.SPAWNING:
                break
            await asyncio.sleep(0.01)
        assert sup.lifecycle.state is State.SPAWNING
        # The watchdog confirms the claimed session before the Kill runs.
        sup.lifecycle.session_ready()
        assert sup.lifecycle.state is State.RUNNING

    await asyncio.wait_for(frame_task, timeout=1.0)
    await asyncio.wait_for(grace_task, timeout=1.0)

    assert runner.kills == 0
    assert runner.spawned == ["A", "B"]
    assert sup.lifecycle.state is State.RUNNING
    assert sup.lifecycle.meeting_id == "B"
    await sup.stop()


async def test_stale_grace_kill_skipped_when_claim_returns_to_grace():
    """given: an expired grace Kill is parked on the execute lock and meeting
           B claims the session, then ends before its queued Spawn runs
    when: the old grace Kill acquires the lock with lifecycle back in GRACE
    then: it is still skipped — only IDLE permits the queued Kill (DW-10)
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=0.05, autospawn=True)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    sup.lifecycle.session_ready()
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    old_grace_task = sup._grace_task

    async with sup._execute_lock:
        for _ in range(100):
            if sup.lifecycle.state is State.IDLE:
                break
            await asyncio.sleep(0.01)
        assert sup.lifecycle.state is State.IDLE

        frame_task = asyncio.create_task(
            sup.handle_frame({"type": "meeting_started", "meeting_id": "B"})
        )
        for _ in range(100):
            if sup.lifecycle.state is State.SPAWNING:
                break
            await asyncio.sleep(0.01)
        assert sup.lifecycle.state is State.SPAWNING

        end_task = asyncio.create_task(
            sup.handle_frame({"type": "meeting_ended", "meeting_id": "B"})
        )
        for _ in range(100):
            if sup.lifecycle.state is State.GRACE:
                break
            await asyncio.sleep(0.01)
        assert sup.lifecycle.state is State.GRACE
        new_grace_task = sup._grace_task
        new_grace_task.cancel()
        sup._grace_task = None

    await asyncio.wait_for(frame_task, timeout=1.0)
    await asyncio.wait_for(end_task, timeout=1.0)
    await asyncio.wait_for(old_grace_task, timeout=1.0)
    with pytest.raises(asyncio.CancelledError):
        await new_grace_task

    assert runner.kills == 0
    assert runner.spawned == ["A", "B"]
    assert sup.lifecycle.state is State.GRACE
    assert sup.lifecycle.meeting_id == "B"
    await sup.stop()


async def test_spawn_waits_out_in_flight_grace_kill_then_fresh_session():
    """given: the grace-expiry Kill holds the execute lock mid-kill
    when: a meeting_started frame arrives while the kill is in flight
    then: its spawn runs only after the kill completes and creates a fresh
          session — never adopt-then-destroy (DW-10)
    """
    class BlockingSessionTmux(FakeTmux):
        def __init__(self):
            super().__init__(has_session=True)
            self.kill_started = asyncio.Event()
            self.release_kill = asyncio.Event()

        async def __call__(self, args):
            if args[3] == "kill-session":
                self.calls.append(list(args))
                self.has_session = False
                self.kill_started.set()
                await self.release_kill.wait()
                return (0, "")
            if args[3] == "new-session":
                self.calls.append(list(args))
                self.has_session = True
                return (0, "")
            return await super().__call__(args)

    fake_tmux = BlockingSessionTmux()
    sup = Supervisor(
        runner=TmuxRunner(run=fake_tmux), grace_s=0.05, autospawn=True
    )

    sup.lifecycle.adopt("A")
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    grace_task = sup._grace_task

    await asyncio.wait_for(fake_tmux.kill_started.wait(), timeout=1.0)
    frame_task = asyncio.create_task(
        sup.handle_frame({"type": "meeting_started", "meeting_id": "B"})
    )
    for _ in range(100):
        if sup.lifecycle.state is State.SPAWNING and sup._spawn_in_flight:
            break
        await asyncio.sleep(0.01)
    assert sup.lifecycle.state is State.SPAWNING
    assert sup._spawn_in_flight is True

    await sup.watchdog.tick()
    assert sup._respawn_attempts == 0
    assert sup._respawn_task is None
    assert "new-session" not in fake_tmux.subcommands()

    fake_tmux.release_kill.set()
    await asyncio.wait_for(frame_task, timeout=1.0)
    await asyncio.wait_for(grace_task, timeout=1.0)

    assert fake_tmux.subcommands().count("kill-session") == 1
    assert fake_tmux.subcommands().count("new-session") == 1
    assert fake_tmux.has_session is True
    assert sup.lifecycle.state is State.SPAWNING
    assert sup.lifecycle.meeting_id == "B"
    assert sup._spawn_in_flight is False
    await sup.stop()


async def test_autospawn_off_meeting_frames_are_inert():
    """given: RECOG_COPILOT_AUTOSPAWN=0 (autospawn=False)
    when: conference lifecycle and meeting_state frames arrive
    then: no commands are issued and the machine stays IDLE
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=0.05, autospawn=False)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    await sup.handle_frame({"type": "meeting_state", "active": True, "meeting_id": "A"})
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})

    assert runner.spawned == []
    assert runner.kills == 0
    assert sup.lifecycle.state is State.IDLE


async def test_autospawn_off_manual_frames_still_work():
    """given: RECOG_COPILOT_AUTOSPAWN=0 — the gate covers only meeting frames (D3)
    when: a manual spawn is followed by a manual stop and grace expiry
    then: the session spawns and is killed on expiry — the flag never blocks
          an explicit user request
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=0.05, autospawn=False)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    assert runner.spawned == [MANUAL_MEETING_ID]
    await sup.handle_frame({"type": "copilot_stop_request", "request_id": "r2"})
    assert sup.lifecycle.state is State.GRACE
    assert runner.kills == 0

    await asyncio.sleep(0.15)
    assert runner.spawned == [MANUAL_MEETING_ID]
    assert runner.kills == 1
    assert sup.lifecycle.state is State.IDLE


def test_autospawn_env_flag_parsing(monkeypatch):
    monkeypatch.setenv("RECOG_COPILOT_AUTOSPAWN", "0")
    assert Supervisor().autospawn is False
    monkeypatch.setenv("RECOG_COPILOT_AUTOSPAWN", "1")
    assert Supervisor().autospawn is True
    monkeypatch.delenv("RECOG_COPILOT_AUTOSPAWN")
    assert Supervisor().autospawn is True  # default on


def test_grace_env_default(monkeypatch):
    monkeypatch.setenv("RECOG_COPILOT_GRACE_S", "17.5")
    assert Supervisor().grace_s == 17.5
    monkeypatch.delenv("RECOG_COPILOT_GRACE_S")
    assert Supervisor().grace_s == 90.0


# ---------------------------------------------------------------------------
# Reconciliation via meeting_state (three branches from the plan)
# ---------------------------------------------------------------------------


async def test_reconcile_live_session_no_meeting_goes_grace_not_kill():
    """given: supervisor tracks RUNNING(A); bridge restarted and replies
           meeting_state active=false (extension has not rejoined yet)
    when: the meeting_state frame is handled
    then: synthetic meeting_ended moves RUNNING→GRACE — no instant kill;
          a same-id meeting_started within grace revives the session
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)
    sup.lifecycle.adopt("A")  # simulate an established RUNNING(A)

    await sup.handle_frame({"type": "meeting_state", "active": False, "meeting_id": None})
    assert sup.lifecycle.state is State.GRACE
    assert runner.kills == 0

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    assert sup.lifecycle.state is State.RUNNING
    assert runner.kills == 0
    assert runner.spawned == []
    await sup.stop()


async def test_reconcile_no_session_active_meeting_spawns():
    """given: machine IDLE, no tmux session
    when: meeting_state reports an active meeting
    then: the supervisor spawns for that meeting
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)

    await sup.handle_frame({"type": "meeting_state", "active": True, "meeting_id": "A"})

    assert runner.spawned == ["A"]
    assert sup.lifecycle.state is State.SPAWNING
    assert sup.lifecycle.meeting_id == "A"


async def test_reconcile_adopts_live_session_mid_meeting():
    """given: machine IDLE but a tmux session is alive (supervisor restarted
           mid-meeting) and the meeting is active
    then: adopt — no respawn, no kill; machine tracks the meeting as RUNNING
    """
    runner = FakeRunner()
    sup = Supervisor(
        runner=runner,
        grace_s=10.0,
        autospawn=True,
        session_alive_probe=lambda: True,
    )

    await sup.handle_frame({"type": "meeting_state", "active": True, "meeting_id": "A"})

    assert runner.spawned == []
    assert runner.kills == 0
    assert sup.lifecycle.state is State.RUNNING
    assert sup.lifecycle.meeting_id == "A"


async def test_reconcile_orphan_session_grace_then_kill():
    """given: machine IDLE but a stale tmux session is alive, no active meeting
    when: meeting_state active=false arrives and grace expires with no flap
    then: the orphan session is adopted into GRACE and killed on expiry
    """
    runner = FakeRunner()
    sup = Supervisor(
        runner=runner,
        grace_s=0.05,
        autospawn=True,
        session_alive_probe=lambda: True,
    )

    await sup.handle_frame({"type": "meeting_state", "active": False, "meeting_id": None})
    assert sup.lifecycle.state is State.GRACE
    assert runner.kills == 0

    await asyncio.sleep(0.15)
    assert runner.kills == 1
    assert sup.lifecycle.state is State.IDLE


async def test_reconcile_idle_no_session_no_meeting_noop():
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)

    await sup.handle_frame({"type": "meeting_state", "active": False, "meeting_id": None})

    assert runner.spawned == []
    assert runner.kills == 0
    assert sup.lifecycle.state is State.IDLE


# ---------------------------------------------------------------------------
# Manual copilot lifecycle (webapp spawn/stop under the synthetic "manual" id)
# ---------------------------------------------------------------------------


def test_manual_meeting_id_matches_bridge_constant():
    """given: the bridge relays manual frames under its own MANUAL_MEETING_ID
    when: the supervisor's duplicated constant is compared with the bridge one
    then: the values match — a drift would break flap/adopt id matching
    """
    from ktalk_bridge import MANUAL_MEETING_ID as BRIDGE_MANUAL_MEETING_ID

    assert MANUAL_MEETING_ID == "manual"
    assert MANUAL_MEETING_ID == BRIDGE_MANUAL_MEETING_ID


@pytest.mark.parametrize("autospawn", [True, False])
async def test_manual_spawn_request_spawns_session(autospawn):
    """given: an idle supervisor, autospawn on or off
    when: copilot_spawn_request arrives
    then: a session is spawned under the synthetic "manual" lifecycle id
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=autospawn)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})

    assert runner.spawned == [MANUAL_MEETING_ID]
    assert runner.kills == 0
    assert sup.lifecycle.state is State.SPAWNING
    assert sup.lifecycle.meeting_id == MANUAL_MEETING_ID
    await sup.stop()


async def test_manual_spawn_request_twice_is_idempotent():
    """given: a live manual session
    when: a second copilot_spawn_request arrives (double click)
    then: no second spawn and no kill — the same-id flap is a no-op
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=False)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    sup.lifecycle.session_ready()
    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r2"})

    assert runner.spawned == [MANUAL_MEETING_ID]
    assert runner.kills == 0
    assert sup.lifecycle.state is State.RUNNING
    await sup.stop()


async def test_manual_spawn_request_within_grace_revives_session():
    """given: a manual session put into GRACE by a stop request
    when: the user presses the button again before the timer expires
    then: GRACE→RUNNING with no kill and no second spawn, timer cancelled
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=False)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    sup.lifecycle.session_ready()
    await sup.handle_frame({"type": "copilot_stop_request", "request_id": "r2"})
    assert sup.lifecycle.state is State.GRACE

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r3"})

    assert sup.lifecycle.state is State.RUNNING
    assert runner.spawned == [MANUAL_MEETING_ID]
    assert runner.kills == 0
    assert sup._grace_task is None  # timer cancelled on leaving GRACE
    await sup.stop()


async def test_manual_stop_request_graces_then_kills():
    """given: a live manual session with autospawn off
    when: copilot_stop_request arrives and the grace timer expires
    then: the kill is deferred to the timer — exactly one spawn, one kill
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=0.05, autospawn=False)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    sup.lifecycle.session_ready()
    await sup.handle_frame({"type": "copilot_stop_request", "request_id": "r2"})
    assert sup.lifecycle.state is State.GRACE
    assert sup._grace_task is not None  # grace timer armed via _apply
    assert runner.kills == 0

    await asyncio.sleep(0.15)
    assert runner.spawned == [MANUAL_MEETING_ID]
    assert runner.kills == 1
    assert sup.lifecycle.state is State.IDLE


async def test_conference_meeting_preempts_manual_session():
    """given: a live manual session and autospawn on
    when: a conference meeting starts
    then: the manual session is killed and a session for the meeting spawned
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    sup.lifecycle.session_ready()
    await sup.handle_frame({"type": "meeting_started", "meeting_id": "B"})

    assert runner.kills == 1
    assert runner.spawned == [MANUAL_MEETING_ID, "B"]
    assert sup.lifecycle.state is State.SPAWNING
    assert sup.lifecycle.meeting_id == "B"
    await sup.stop()


async def test_reconcile_adopts_live_manual_session():
    """given: machine IDLE but a manual tmux session is alive (supervisor
           restarted while the user's manual session ran)
    when: the bridge replies meeting_state {active: true, meeting_id: "manual"}
    then: adopt — no spawn, no kill; the machine tracks the manual session
    """
    runner = FakeRunner()
    sup = Supervisor(
        runner=runner,
        grace_s=10.0,
        autospawn=True,
        session_alive_probe=lambda: True,
    )

    await sup.handle_frame(
        {"type": "meeting_state", "active": True, "meeting_id": MANUAL_MEETING_ID}
    )

    assert runner.spawned == []
    assert runner.kills == 0
    assert sup.lifecycle.state is State.RUNNING
    assert sup.lifecycle.meeting_id == MANUAL_MEETING_ID


async def test_manual_session_respawns_after_crash_with_autospawn_off():
    """given: autospawn off and a live manual session
    when: the watchdog keeps reporting session_died
    then: the session respawns on the same ≤3 backoff budget as a conference
          one — the removed gate never blocked crash recovery (D3)
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=False)
    sup.RESPAWN_BACKOFF_S = (0.01, 0.01, 0.01)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    sup.lifecycle.session_ready()

    for _ in range(6):
        await sup.on_session_died()
        await asyncio.sleep(0.05)

    assert runner.spawned == [MANUAL_MEETING_ID] * 4  # initial + 3 respawns
    assert sup.lifecycle.state is State.SPAWNING
    await sup.stop()


async def test_manual_spawn_request_resets_exhausted_respawn_budget():
    """given: a manual session that burned its whole respawn budget and parked
           in SPAWNING("manual")
    when: the user presses Start again — a same-id flap that emits no commands
    then: the budget is fresh, so the next session_died revives the session;
          without the reset the button would stay inert until Stop + grace
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=False)
    sup.RESPAWN_BACKOFF_S = (0.01, 0.01, 0.01)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    for _ in range(6):
        await sup.on_session_died()
        await asyncio.sleep(0.05)
    assert runner.spawned == [MANUAL_MEETING_ID] * 4  # budget spent

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r2"})
    assert runner.spawned == [MANUAL_MEETING_ID] * 4  # the flap itself spawns nothing
    assert sup._respawn_attempts == 0

    await sup.on_session_died()
    await asyncio.sleep(0.05)

    assert runner.spawned == [MANUAL_MEETING_ID] * 5
    await sup.stop()


async def test_manual_spawn_request_preserves_pending_respawn():
    """given: a manual session with a respawn already waiting on backoff
    when: the user presses Start again before that retry runs
    then: the pending retry and its spent budget are preserved — the click
          neither cancels nor postpones recovery
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=False)
    sup.RESPAWN_BACKOFF_S = (0.1, 0.1, 0.1)

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    sup.lifecycle.session_ready()
    await sup.on_session_died()
    pending_respawn = sup._respawn_task
    assert pending_respawn is not None
    assert sup._respawn_attempts == 1

    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r2"})

    assert sup._respawn_task is pending_respawn
    assert sup._respawn_attempts == 1
    await asyncio.sleep(0.15)
    assert runner.spawned == [MANUAL_MEETING_ID] * 2
    await sup.stop()


async def test_manual_stop_during_conference_is_inert():
    """given: a session bound to conference meeting "B" (the bridge relays a
           stray webapp copilot_stop_request with no conference guard and
           documents it as inert here — ktalk_bridge.py:3178-3181)
    when: copilot_stop_request arrives while RUNNING("B")
    then: the id mismatch makes meeting_ended a no-op — the conference session
          stays RUNNING, no grace timer is armed and nothing is killed
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=0.05, autospawn=True)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "B"})
    sup.lifecycle.session_ready()
    await sup.handle_frame({"type": "copilot_stop_request", "request_id": "r1"})

    assert sup.lifecycle.state is State.RUNNING
    assert sup.lifecycle.meeting_id == "B"
    assert runner.kills == 0
    assert sup._grace_task is None
    await sup.stop()


async def test_manual_frames_malformed_are_ignored():
    """given: an idle supervisor
    when: a non-dict frame, an unknown type and an empty frame arrive
    then: nothing is spawned — handle_frame's existing guards drop them
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=False)

    await sup.handle_frame(["copilot_spawn_request"])
    await sup.handle_frame({"type": "copilot_unknown_request", "request_id": "r1"})
    await sup.handle_frame({})

    assert runner.spawned == []
    assert runner.kills == 0
    assert sup.lifecycle.state is State.IDLE


@pytest.mark.parametrize(
    "state,meeting_id,expected",
    [
        (State.IDLE, None, None),
        (State.SPAWNING, MANUAL_MEETING_ID, MANUAL_MEETING_ID),
        (State.RUNNING, MANUAL_MEETING_ID, MANUAL_MEETING_ID),
        (State.RUNNING, "mtg-1", "mtg-1"),
        # adopted session of unknown provenance: never claim it is manual
        (State.RUNNING, None, None),
        (State.GRACE, MANUAL_MEETING_ID, None),
    ],
)
def test_live_meeting_id_reports_only_sessions_meant_to_live(state, meeting_id, expected):
    """given: the lifecycle parked in each state
    when: supervisor_hello's current_meeting_id is computed
    then: only SPAWNING/RUNNING report an id — a GRACE session the user
          already stopped must not be re-armed and revived by the bridge reply
    """
    sup = Supervisor(runner=FakeRunner())
    sup.lifecycle.state = state
    sup.lifecycle.meeting_id = meeting_id

    assert sup._live_meeting_id() == expected


# ---------------------------------------------------------------------------
# Bridge connection: supervisor_hello + reconnect behaviour
# ---------------------------------------------------------------------------


async def _start_mock_bridge(handler):
    server = await websockets.serve(handler, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    return server, port


async def _collect_hello(handler_received):
    for _ in range(20):
        if handler_received:
            break
        await asyncio.sleep(0.05)
    assert handler_received, "supervisor did not send any frame"
    return json.loads(handler_received[0])


async def test_supervisor_sends_hello_on_connect():
    """given: a mock bridge accepting connections and an idle supervisor
    when: the supervisor connects
    then: the first frame is supervisor_hello with a null current_meeting_id
    """
    received = []

    async def handler(websocket):
        try:
            first = await asyncio.wait_for(websocket.recv(), timeout=1.0)
            received.append(first)
        except Exception:
            pass
        await asyncio.sleep(0.2)

    server, port = await _start_mock_bridge(handler)
    sup = Supervisor(f"ws://127.0.0.1:{port}", runner=FakeRunner())
    await sup.start()
    try:
        assert await _collect_hello(received) == {
            "type": "supervisor_hello",
            "current_meeting_id": None,
        }
    finally:
        await sup.stop()
        server.close()
        await server.wait_closed()


async def test_supervisor_hello_reports_live_manual_session():
    """given: a live manual session and a bridge that (re)appears afterwards
    when: the supervisor connects
    then: supervisor_hello carries current_meeting_id="manual" so the bridge
          re-arms its manual mirror and answers with an adoptable meeting_state
    """
    received = []

    async def handler(websocket):
        try:
            first = await asyncio.wait_for(websocket.recv(), timeout=1.0)
            received.append(first)
        except Exception:
            pass
        await asyncio.sleep(0.2)

    server, port = await _start_mock_bridge(handler)
    sup = Supervisor(f"ws://127.0.0.1:{port}", runner=FakeRunner(), autospawn=False)
    await sup.handle_frame({"type": "copilot_spawn_request", "request_id": "r1"})
    sup.lifecycle.session_ready()
    await sup.start()
    try:
        assert await _collect_hello(received) == {
            "type": "supervisor_hello",
            "current_meeting_id": MANUAL_MEETING_ID,
        }
    finally:
        await sup.stop()
        server.close()
        await server.wait_closed()


async def test_supervisor_hello_then_meeting_state_spawns():
    """given: a mock bridge that answers supervisor_hello with meeting_state
    when: the supervisor connects with no session and an active meeting exists
    then: it spawns a session for that meeting (reconciliation over real WS)
    """
    runner = FakeRunner()

    async def handler(websocket):
        first = await asyncio.wait_for(websocket.recv(), timeout=1.0)
        assert json.loads(first)["type"] == "supervisor_hello"
        await websocket.send(json.dumps({
            "type": "meeting_state",
            "active": True,
            "meeting_id": "mtg-1",
            "meeting_title": "Standup",
        }))
        await asyncio.sleep(0.5)

    server, port = await _start_mock_bridge(handler)
    sup = Supervisor(f"ws://127.0.0.1:{port}", runner=runner, grace_s=10.0, autospawn=True)
    await sup.start()
    try:
        for _ in range(40):
            if runner.spawned:
                break
            await asyncio.sleep(0.05)
        assert runner.spawned == ["mtg-1"]
        assert sup.lifecycle.state is State.SPAWNING
    finally:
        await sup.stop()
        server.close()
        await server.wait_closed()


async def test_supervisor_survives_bridge_unavailable():
    """given: nothing listens on the bridge URL
    when: the supervisor starts
    then: it does not crash; the loop keeps retrying in the background
    """
    sup = Supervisor("ws://127.0.0.1:1", runner=FakeRunner())
    sup.INITIAL_BACKOFF_S = 0.05
    await sup.start()
    try:
        await asyncio.sleep(0.2)
        assert sup._task is not None
        assert not sup._task.done()
    finally:
        await sup.stop()


async def test_supervisor_drops_malformed_frames():
    """given: bridge sends garbage before a valid lifecycle frame
    when: the supervisor processes the stream
    then: garbage is ignored, the valid frame still spawns the session
    """
    runner = FakeRunner()

    async def handler(websocket):
        await websocket.recv()  # supervisor_hello
        await websocket.send("not-json")
        await websocket.send(json.dumps(["not", "a", "dict"]))
        await websocket.send(json.dumps({"type": "meeting_started", "meeting_id": "A"}))
        await asyncio.sleep(0.5)

    server, port = await _start_mock_bridge(handler)
    sup = Supervisor(f"ws://127.0.0.1:{port}", runner=runner, grace_s=10.0, autospawn=True)
    await sup.start()
    try:
        for _ in range(40):
            if runner.spawned:
                break
            await asyncio.sleep(0.05)
        assert runner.spawned == ["A"]
    finally:
        await sup.stop()
        server.close()
        await server.wait_closed()


# ---------------------------------------------------------------------------
# TmuxRunner: command building + tmux invocations via injected runner
# ---------------------------------------------------------------------------


def test_build_claude_command_flags_and_json():
    """given: a TmuxRunner with an explicit MCP URL
    when: the claude command line is built
    then: it carries all required flags and the --mcp-config value survives
          shell quoting as valid JSON pointing at the copilot daemon
    """
    runner = TmuxRunner(mcp_url="http://127.0.0.1:8767/mcp", run=FakeTmux())
    tokens = shlex.split(runner.build_claude_command())

    assert tokens[0] == "claude"
    assert tokens[1:3] == ["--agent", "copilot-session"]
    assert "--strict-mcp-config" in tokens
    config = json.loads(tokens[tokens.index("--mcp-config") + 1])
    assert config == {
        "mcpServers": {
            "copilot": {"type": "http", "url": "http://127.0.0.1:8767/mcp"}
        }
    }
    dev = tokens.index("--dangerously-load-development-channels")
    assert tokens[dev + 1] == "server:copilot"


def test_mcp_url_from_env(monkeypatch):
    monkeypatch.setenv("RECOG_COPILOT_MCP_URL", "http://127.0.0.1:9999/mcp")
    assert TmuxRunner(run=FakeTmux()).mcp_url == "http://127.0.0.1:9999/mcp"
    monkeypatch.delenv("RECOG_COPILOT_MCP_URL")
    assert TmuxRunner(run=FakeTmux()).mcp_url == DEFAULT_MCP_URL


async def test_spawn_runs_new_session_with_socket_and_cwd(tmp_path):
    fake = FakeTmux(has_session=False)
    runner = TmuxRunner(repo_root=tmp_path, mcp_url="http://x/mcp", run=fake)

    created = await runner.spawn("A")

    assert created is True
    assert fake.subcommands() == ["has-session", "new-session"]
    new_session = fake.calls[1]
    assert new_session[:3] == ["tmux", "-L", "recog-copilot"]
    assert new_session[3:9] == ["new-session", "-d", "-s", "copilot", "-c", str(tmp_path)]
    assert new_session[9] == runner.build_claude_command()


async def test_spawn_adopts_live_session_without_new_session():
    """given: a tmux session named copilot is already alive
    when: spawn() is called
    then: no new-session is issued (duplicate name would error and waste
          a respawn-backoff attempt) — the live session is adopted
    """
    fake = FakeTmux(has_session=True)

    created = await TmuxRunner(run=fake).spawn("A")

    assert created is False
    assert fake.subcommands() == ["has-session"]


async def test_spawn_raises_on_tmux_failure():
    fake = FakeTmux()
    fake.responses["new-session"] = (1, "error connecting to socket")

    with pytest.raises(RuntimeError, match="new-session failed"):
        await TmuxRunner(run=fake).spawn("A")


async def test_kill_targets_named_session_and_ignores_missing():
    """given: kill-session fails but has-session confirms the session is gone
    when: kill() runs
    then: it returns without raising and without retrying — kill stays
          idempotent (rc != 0 with the session gone is a success)
    """
    fake = FakeTmux()
    fake.responses["kill-session"] = (1, "no server running")

    await TmuxRunner(run=fake).kill()  # must not raise

    assert fake.calls[0] == ["tmux", "-L", "recog-copilot", "kill-session", "-t", "copilot"]
    assert fake.subcommands() == ["kill-session", "has-session"]  # probe, no retry


class SequencedKillTmux(FakeTmux):
    """FakeTmux whose kill-session responses are consumed in sequence."""

    def __init__(self, kill_results, *, has_session=True, has_session_results=None):
        super().__init__(has_session=has_session)
        self.kill_results = list(kill_results)
        self.has_session_results = (
            list(has_session_results) if has_session_results is not None else None
        )

    async def __call__(self, args):
        if args[3] == "kill-session" and self.kill_results:
            self.calls.append(list(args))
            return self.kill_results.pop(0)
        if (
            args[3] == "has-session"
            and self.has_session_results is not None
            and self.has_session_results
        ):
            self.calls.append(list(args))
            has_session = self.has_session_results.pop(0)
            return (0, "") if has_session else (1, "can't find session: copilot")
        return await super().__call__(args)


def test_kill_retry_policy_defaults_are_pinned():
    """given: the production tmux kill retry policy
    when: its class-level defaults are inspected
    then: it remains three total attempts paced by half a second
    """
    assert TmuxRunner.KILL_RETRIES == 3
    assert TmuxRunner.KILL_RETRY_DELAY_S == 0.5


async def test_kill_retries_transient_failure_then_succeeds():
    """given: the first kill-session fails while the session is still alive
    when: kill() retries after the (zeroed) delay
    then: the second attempt succeeds and nothing is raised
    """
    fake = SequencedKillTmux([(1, "server busy"), (0, "")])
    runner = TmuxRunner(run=fake)
    runner.KILL_RETRY_DELAY_S = 0.0

    await runner.kill()  # must not raise

    assert fake.subcommands() == ["kill-session", "has-session", "kill-session"]


async def test_kill_stops_retrying_when_session_disappears_after_later_attempt():
    """given: two failed kill attempts, with the session alive after the first
           failure but gone after the second
    when: kill() probes liveness after each failed attempt
    then: it returns after the second probe without a third kill or an error
    """
    fake = SequencedKillTmux(
        [(1, "busy"), (1, "already gone"), (1, "must not run")],
        has_session_results=[True, False],
    )
    runner = TmuxRunner(run=fake)
    runner.KILL_RETRY_DELAY_S = 0.0

    await runner.kill()

    assert fake.subcommands() == [
        "kill-session",
        "has-session",
        "kill-session",
        "has-session",
    ]


async def test_kill_persistent_failure_raises_after_bounded_retries():
    """given: kill-session keeps failing while has-session still succeeds
    when: kill() exhausts its bounded retry budget
    then: RuntimeError surfaces with the rc and trimmed tmux output (DW-12 —
          a live session must not be silently orphaned)
    """
    fake = SequencedKillTmux([(1, "cannot kill\n")] * 5)
    runner = TmuxRunner(run=fake)
    runner.KILL_RETRY_DELAY_S = 0.0

    with pytest.raises(RuntimeError, match=r"kill-session failed \(rc=1\): cannot kill"):
        await runner.kill()

    assert fake.subcommands().count("kill-session") == TmuxRunner.KILL_RETRIES


async def test_kill_retry_pacing_sleeps_only_between_attempts(monkeypatch):
    """given: kill() paths with and without retries, with asyncio.sleep
           replaced by a recorder (real KILL_RETRY_DELAY_S kept)
    when: each path runs
    then: no sleep on single-attempt success or the idempotent session-gone
          path; exactly attempts-1 sleeps of KILL_RETRY_DELAY_S between
          attempts on the retry and exhaustion paths
    """
    import recog.copilot_supervisor as sup_mod

    sleeps = []

    async def record_sleep(delay):
        sleeps.append(delay)

    monkeypatch.setattr(sup_mod.asyncio, "sleep", record_sleep)

    await TmuxRunner(run=SequencedKillTmux([(0, "")])).kill()
    assert sleeps == []  # first-attempt success: no pacing

    await TmuxRunner(
        run=SequencedKillTmux([(1, "gone")], has_session=False)
    ).kill()
    assert sleeps == []  # idempotent session-gone: no retry, no sleep

    await TmuxRunner(run=SequencedKillTmux([(1, "busy"), (1, "busy"), (0, "")])).kill()
    assert sleeps == [TmuxRunner.KILL_RETRY_DELAY_S] * 2  # between attempts only

    sleeps.clear()
    with pytest.raises(RuntimeError):
        await TmuxRunner(run=SequencedKillTmux([(1, "stuck")] * 5)).kill()
    assert sleeps == [TmuxRunner.KILL_RETRY_DELAY_S] * (TmuxRunner.KILL_RETRIES - 1)


async def test_execute_logs_persistent_kill_failure_without_crashing(capsys):
    """given: a runner whose kill() raises after exhausted retries
    when: a Kill command is routed through _execute
    then: the per-command error path prints the failure and the supervisor
          does not crash
    """
    class FailingKillRunner(FakeRunner):
        async def kill(self):
            raise RuntimeError("tmux kill-session failed (rc=1): boom")

    sup = Supervisor(runner=FailingKillRunner())

    await sup._execute((Kill(),))  # must not raise

    error = capsys.readouterr().err
    assert "command Kill() failed" in error
    assert "rc=1" in error
    assert "boom" in error


async def test_grace_expiry_surfaces_exhausted_tmux_kill_and_stays_idle(capsys):
    """given: a GRACE session whose real TmuxRunner exhausts all kill retries
           while has-session keeps confirming the session is alive
    when: the grace timer expires
    then: the composite supervisor path logs the tmux diagnostic, performs
          the bounded attempts, and leaves lifecycle IDLE without crashing
    """
    fake = SequencedKillTmux([(1, "still alive")] * 5, has_session=True)
    runner = TmuxRunner(run=fake)
    runner.KILL_RETRY_DELAY_S = 0.0
    sup = Supervisor(runner=runner, grace_s=0.0, autospawn=True)
    sup.lifecycle.adopt("A")

    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    grace_task = sup._grace_task
    await asyncio.wait_for(grace_task, timeout=1.0)

    error = capsys.readouterr().err
    assert "command Kill() failed" in error
    assert "rc=1" in error
    assert "still alive" in error
    assert fake.subcommands().count("kill-session") == TmuxRunner.KILL_RETRIES
    assert sup.lifecycle.state is State.IDLE
    await sup.stop()


async def test_alive_reflects_has_session():
    assert await TmuxRunner(run=FakeTmux(has_session=True)).alive() is True
    assert await TmuxRunner(run=FakeTmux(has_session=False)).alive() is False


async def test_send_keys_and_capture():
    fake = FakeTmux()
    fake.responses["capture-pane"] = (0, "❯ prompt line")
    runner = TmuxRunner(run=fake)

    await runner.send_keys("1")
    text = await runner.capture()

    assert fake.calls[0] == ["tmux", "-L", "recog-copilot", "send-keys", "-t", "copilot", "1"]
    assert fake.calls[1] == ["tmux", "-L", "recog-copilot", "capture-pane", "-p", "-t", "copilot"]
    assert text == "❯ prompt line"


async def test_capture_returns_empty_when_session_gone():
    fake = FakeTmux()
    fake.responses["capture-pane"] = (1, "no server running")
    assert await TmuxRunner(run=fake).capture() == ""


# ---------------------------------------------------------------------------
# Respawn with backoff (session_died glue)
# ---------------------------------------------------------------------------


async def test_respawn_backoff_exhausts_after_three_attempts():
    """given: a running meeting whose session keeps dying
    when: session_died is reported more times than the backoff budget
    then: exactly 3 respawns happen (initial spawn + 3), then the
          supervisor gives up for this meeting
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)
    sup.RESPAWN_BACKOFF_S = (0.01, 0.01, 0.01)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    assert runner.spawned == ["A"]

    for _ in range(6):
        await sup.on_session_died()
        await asyncio.sleep(0.05)

    assert runner.spawned == ["A"] * 4  # initial + 3 respawns, then gave up
    assert sup.lifecycle.state is State.SPAWNING
    await sup.stop()


async def test_respawn_pending_absorbs_repeat_reports():
    """given: a respawn is already scheduled (backoff sleep in progress)
    when: session_died is reported again before it fires
    then: no extra respawn is scheduled — one respawn total"""
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)
    sup.RESPAWN_BACKOFF_S = (0.05, 0.05, 0.05)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    await sup.on_session_died()
    await sup.on_session_died()
    await sup.on_session_died()
    await asyncio.sleep(0.15)

    assert runner.spawned == ["A", "A"]  # initial + single respawn
    await sup.stop()


async def test_meeting_change_cancels_pending_respawn_and_resets_budget():
    """given: a respawn for meeting A is pending
    when: meeting B starts
    then: the pending respawn is cancelled (no late spawn of A) and the
          backoff budget is fresh for B"""
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)
    sup.RESPAWN_BACKOFF_S = (0.05, 0.05, 0.05)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    await sup.on_session_died()
    await sup.handle_frame({"type": "meeting_started", "meeting_id": "B"})
    await asyncio.sleep(0.15)

    assert runner.spawned == ["A", "B"]
    assert runner.kills == 1
    assert sup._respawn_attempts == 0

    await sup.on_session_died()
    await asyncio.sleep(0.15)
    assert runner.spawned == ["A", "B", "B"]  # budget reset — respawn works for B
    await sup.stop()


async def test_respawn_aborted_when_meeting_ends_during_backoff():
    """given: a respawn is pending after session death
    when: the meeting ends during the backoff sleep (state leaves SPAWNING)
    then: the respawn does not fire into GRACE"""
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)
    sup.RESPAWN_BACKOFF_S = (0.05, 0.05, 0.05)

    await sup.handle_frame({"type": "meeting_started", "meeting_id": "A"})
    await sup.on_session_died()
    await sup.handle_frame({"type": "meeting_ended", "meeting_id": "A"})
    assert sup.lifecycle.state is State.GRACE

    await asyncio.sleep(0.15)
    assert runner.spawned == ["A"]  # no respawn after the meeting ended
    await sup.stop()


async def test_session_died_while_idle_does_not_spawn():
    """given: an IDLE machine — no session should exist
    when: session_died is reported
    then: nothing is spawned. D3 removed the autospawn gate from this path, so
          the state machine is what holds the line: `session_died()` in IDLE
          yields no Spawn command regardless of the flag
    """
    runner = FakeRunner()
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=False)

    await sup.on_session_died()
    await asyncio.sleep(0.02)

    assert runner.spawned == []
    assert sup._respawn_task is None
    assert sup.lifecycle.state is State.IDLE


async def test_reconcile_uses_async_runner_alive_probe():
    """given: no explicit session_alive_probe — the supervisor defaults to
           the runner's async alive()
    when: meeting_state reports an active meeting and tmux has a session
    then: the live session is adopted instead of respawned"""
    fake = FakeTmux(has_session=True)
    runner = TmuxRunner(run=fake)
    sup = Supervisor(runner=runner, grace_s=10.0, autospawn=True)

    await sup.handle_frame({"type": "meeting_state", "active": True, "meeting_id": "A"})

    assert sup.lifecycle.state is State.RUNNING
    assert sup.lifecycle.meeting_id == "A"
    assert "new-session" not in fake.subcommands()


# ---------------------------------------------------------------------------
# Watchdog: screen classification + tick behaviour
# ---------------------------------------------------------------------------

# Fixtures approximate real claude 2.1.197 rendered panes (spike captures).
DEV_CHANNELS_SCREEN = (
    "╭──────────────────────────────────────────────╮\n"
    "│ Load local development channels?             │\n"
    "│                                              │\n"
    "│ ❯ 1. Yes, load them                          │\n"
    "│   2. No                                      │\n"
    "│                                              │\n"
    "│ Enter to confirm · Esc to cancel             │\n"
    "╰──────────────────────────────────────────────╯\n"
)
MODEL_DIALOG_SCREEN = (
    "╭──────────────────────────────────────────────╮\n"
    "│ Select a model                               │\n"
    "│                                              │\n"
    "│ ❯ 1. Default (recommended)                   │\n"
    "│   2. Opus                                    │\n"
    "│   3. Sonnet                                  │\n"
    "│                                              │\n"
    "│ Enter to confirm · Esc to cancel             │\n"
    "╰──────────────────────────────────────────────╯\n"
)
REPL_PROMPT_SCREEN = (
    " ▐▛███▜▌   Claude Code v2.1.197\n"
    " ▝▜█████▛▘\n"
    "\n"
    "╭──────────────────────────────────────────────╮\n"
    "│ ❯                                            │\n"
    "╰──────────────────────────────────────────────╯\n"
    "  ? for shortcuts\n"
)
SPINNER_SCREEN = (
    "✻ Baking… (12s · esc to interrupt)\n"
    "\n"
    "╭──────────────────────────────────────────────╮\n"
    "│ ❯                                            │\n"
    "╰──────────────────────────────────────────────╯\n"
)
STARTUP_SCREEN = "\n\n  Starting up…\n"


@pytest.mark.parametrize(
    "screen,expected",
    [
        (DEV_CHANNELS_SCREEN, AgentState.BLOCKED),   # known dev-channels dialog
        (MODEL_DIALOG_SCREEN, AgentState.BLOCKED),   # unknown selection dialog
        (SPINNER_SCREEN, AgentState.WORKING),        # spinner beats the prompt line
        (REPL_PROMPT_SCREEN, AgentState.ONLINE),     # idle prompt
        ("", AgentState.SPAWNING),                   # empty pane is transitional
        (STARTUP_SCREEN, AgentState.SPAWNING),       # startup, no markers yet
        (None, AgentState.OFFLINE),                  # tmux session gone
    ],
)
def test_classify_screen(screen, expected):
    assert classify_screen(screen) is expected


class WatchdogRunner:
    """Programmable runner for the watchdog: alive flag + pane text + key log."""

    def __init__(self, *, alive=True, screen=""):
        self._alive = alive
        self.screen = screen
        self.sent_keys = []

    async def alive(self):
        return self._alive

    async def capture(self):
        return self.screen

    async def send_keys(self, *keys):
        self.sent_keys.append(keys)


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def advance(self, dt):
        self.now += dt


def _make_watchdog(
    runner, *, state=State.SPAWNING, meeting_id="A", clock=None, spawn_in_flight=None
):
    lifecycle = SessionLifecycle()
    lifecycle.state = state
    lifecycle.meeting_id = meeting_id
    statuses = []
    died = []

    async def send_status(status):
        statuses.append(status)

    async def on_died():
        died.append(True)

    watchdog = Watchdog(
        runner=runner,
        lifecycle=lifecycle,
        send_status=send_status,
        on_session_died=on_died,
        clock=clock or (lambda: 0.0),
        spawn_in_flight=spawn_in_flight or (lambda: False),
    )
    return watchdog, lifecycle, statuses, died


async def test_watchdog_auto_dismisses_dev_channels_once():
    """given: the copilot pane shows the known dev-channels dialog
    when: the watchdog ticks twice on the same dialog
    then: it sends "1" exactly once and reports spawning (never blocked)
    """
    clock = FakeClock()
    runner = WatchdogRunner(screen=DEV_CHANNELS_SCREEN)
    watchdog, _lifecycle, statuses, _died = _make_watchdog(runner, clock=clock)

    await watchdog.tick()
    await watchdog.tick()

    assert runner.sent_keys == [("1",)]
    assert AgentState.BLOCKED not in statuses
    assert statuses == [AgentState.SPAWNING]


async def test_watchdog_redismisses_when_dialog_reappears():
    """given: the dialog was dismissed and the prompt returned
    when: the dialog appears again
    then: "1" is sent a second time (once per occurrence)
    """
    clock = FakeClock()
    runner = WatchdogRunner(screen=DEV_CHANNELS_SCREEN)
    watchdog, _lifecycle, _statuses, _died = _make_watchdog(runner, clock=clock)

    await watchdog.tick()                 # dialog → send "1"
    runner.screen = REPL_PROMPT_SCREEN
    await watchdog.tick()                 # prompt → clears the dismiss latch
    runner.screen = DEV_CHANNELS_SCREEN
    await watchdog.tick()                 # dialog again → send "1" again

    assert runner.sent_keys == [("1",), ("1",)]


async def test_watchdog_unknown_dialog_reports_blocked_without_dismiss():
    """given: an unknown selection dialog (e.g. /model)
    when: the watchdog ticks
    then: it surfaces blocked and does NOT auto-answer it
    """
    runner = WatchdogRunner(screen=MODEL_DIALOG_SCREEN)
    watchdog, _lifecycle, statuses, _died = _make_watchdog(runner)

    await watchdog.tick()

    assert statuses == [AgentState.BLOCKED]
    assert runner.sent_keys == []


async def test_watchdog_reports_on_change_and_heartbeat():
    """given: a stable online prompt
    when: the watchdog ticks below and at the heartbeat interval
    then: it reports on the first observation, stays silent below the
          interval, repeats as a heartbeat at the interval, and reports
          again when the state changes
    """
    clock = FakeClock()
    runner = WatchdogRunner(screen=REPL_PROMPT_SCREEN)
    watchdog, lifecycle, statuses, _died = _make_watchdog(runner, clock=clock)

    await watchdog.tick()                         # t=0: online (first report)
    assert statuses == [AgentState.ONLINE]
    assert lifecycle.state is State.RUNNING       # online confirms session_ready

    clock.advance(2.0)
    await watchdog.tick()                         # t=2: below heartbeat → silent
    assert statuses == [AgentState.ONLINE]

    clock.advance(3.0)
    await watchdog.tick()                         # t=5: heartbeat repeats status
    assert statuses == [AgentState.ONLINE, AgentState.ONLINE]

    runner.screen = SPINNER_SCREEN
    clock.advance(1.0)
    await watchdog.tick()                         # t=6: working (state change)
    assert statuses[-1] is AgentState.WORKING


async def test_watchdog_reports_session_died_when_pane_gone():
    """given: the tmux session vanished
    when: the watchdog ticks
    then: it reports offline and feeds session_died back to the lifecycle
    """
    runner = WatchdogRunner(alive=False)
    watchdog, _lifecycle, statuses, died = _make_watchdog(runner, state=State.RUNNING)

    await watchdog.tick()

    assert statuses == [AgentState.OFFLINE]
    assert died == [True]


async def test_watchdog_offline_during_spawn_is_not_death():
    """given: a spawn command is in-flight (tmux session not created yet)
    when: the watchdog ticks and observes the pane as offline
    then: it reports spawning (not offline) and does NOT fire session_died —
          the pre-spawn gap must not burn a respawn attempt or flicker the chip
    """
    runner = WatchdogRunner(alive=False)
    watchdog, lifecycle, statuses, died = _make_watchdog(
        runner, state=State.SPAWNING, spawn_in_flight=lambda: True
    )

    await watchdog.tick()

    assert statuses == [AgentState.SPAWNING]
    assert died == []
    assert lifecycle.state is State.SPAWNING


async def test_watchdog_live_pane_during_spawn_does_not_mark_replacement_ready():
    """given: a replacement Spawn is queued while the old pane is still online
    when: the watchdog ticks before and after the Spawn command settles
    then: it stays SPAWNING until settlement and only then marks the session ready
    """
    spawn_in_flight = True
    runner = WatchdogRunner(alive=True, screen=REPL_PROMPT_SCREEN)
    watchdog, lifecycle, statuses, died = _make_watchdog(
        runner,
        state=State.SPAWNING,
        spawn_in_flight=lambda: spawn_in_flight,
    )

    await watchdog.tick()

    assert statuses == [AgentState.SPAWNING]
    assert lifecycle.state is State.SPAWNING
    assert died == []

    spawn_in_flight = False
    await watchdog.tick()

    assert statuses[-1] is AgentState.ONLINE
    assert lifecycle.state is State.RUNNING


async def test_watchdog_offline_after_spawn_completes_is_death():
    """given: no spawn is in-flight (the session should be up) but the pane is gone
    when: the watchdog ticks
    then: it reports offline and feeds session_died back — a genuine post-spawn
          crash is still detected once the in-flight flag has cleared
    """
    runner = WatchdogRunner(alive=False)
    watchdog, _lifecycle, statuses, died = _make_watchdog(
        runner, state=State.RUNNING, spawn_in_flight=lambda: False
    )

    await watchdog.tick()

    assert statuses == [AgentState.OFFLINE]
    assert died == [True]


async def test_watchdog_spawning_past_deadline_reports_blocked():
    """given: the pane stays in the transitional spawning state
    when: it exceeds the spawn deadline
    then: the watchdog reports blocked (fail-visible) without respawning
    """
    clock = FakeClock()
    runner = WatchdogRunner(screen=STARTUP_SCREEN)
    watchdog, lifecycle, statuses, died = _make_watchdog(runner, clock=clock)

    await watchdog.tick()                                 # t=0: spawning
    assert statuses == [AgentState.SPAWNING]

    clock.advance(Watchdog.SPAWN_DEADLINE_S)
    await watchdog.tick()                                 # t=60: blocked
    assert statuses[-1] is AgentState.BLOCKED
    assert lifecycle.state is State.SPAWNING              # respawn NOT triggered
    assert died == []


async def test_watchdog_idle_reports_offline_once_then_silent():
    """given: the lifecycle is IDLE (no session should exist)
    when: the watchdog ticks repeatedly
    then: it closes the chip to offline once and never probes tmux
    """
    runner = WatchdogRunner(alive=True, screen=REPL_PROMPT_SCREEN)
    watchdog, _lifecycle, statuses, _died = _make_watchdog(
        runner, state=State.IDLE, meeting_id=None
    )
    watchdog._last_reported = AgentState.ONLINE  # a prior live report to close out

    await watchdog.tick()
    await watchdog.tick()

    assert statuses == [AgentState.OFFLINE]
    assert runner.sent_keys == []


# ---------------------------------------------------------------------------
# Supervisor ↔ watchdog wiring: status send over the bridge socket, run loop
# ---------------------------------------------------------------------------


async def test_execute_marks_spawn_in_flight_during_spawn():
    """given: a runner whose spawn blocks partway through
    when: _execute runs a Spawn command
    then: _spawn_in_flight is True while spawn awaits and cleared once it
          returns — so a concurrent watchdog tick can suppress the pre-spawn
          OFFLINE gap (the supervisor wires this flag into its watchdog)
    """
    started = asyncio.Event()
    release = asyncio.Event()

    class BlockingRunner(FakeRunner):
        async def spawn(self, meeting_id):
            await super().spawn(meeting_id)
            started.set()
            await release.wait()

    sup = Supervisor(runner=BlockingRunner())
    assert sup._spawn_in_flight is False
    task = asyncio.create_task(sup._execute((Spawn("A"),)))
    try:
        await asyncio.wait_for(started.wait(), timeout=1.0)
        assert sup._spawn_in_flight is True
    finally:
        release.set()
        await task
    assert sup._spawn_in_flight is False


async def test_execute_empty_commands_does_not_wait_for_lock():
    """given: another command owns the execution lock
    when: a no-op lifecycle event emits an empty command tuple
    then: _execute returns without waiting for unrelated tmux I/O
    """
    sup = Supervisor(runner=FakeRunner())

    async with sup._execute_lock:
        await asyncio.wait_for(sup._execute(()), timeout=0.1)


async def test_execute_keeps_spawn_in_flight_set_across_queued_spawns():
    """given: one Spawn is executing and a second Spawn is queued on the lock
    when: the first finishes and the second begins
    then: the shared watchdog flag stays true until both callers finish
    """
    first_started = asyncio.Event()
    release_first = asyncio.Event()
    second_started = asyncio.Event()
    release_second = asyncio.Event()

    class TwoSpawnRunner(FakeRunner):
        async def spawn(self, meeting_id):
            await super().spawn(meeting_id)
            if meeting_id == "A":
                first_started.set()
                await release_first.wait()
            else:
                second_started.set()
                await release_second.wait()

    sup = Supervisor(runner=TwoSpawnRunner())
    first_task = asyncio.create_task(sup._execute((Spawn("A"),)))
    await asyncio.wait_for(first_started.wait(), timeout=1.0)
    second_task = asyncio.create_task(sup._execute((Spawn("B"),)))
    await asyncio.sleep(0)
    assert sup._spawn_in_flight is True

    release_first.set()
    await asyncio.wait_for(second_started.wait(), timeout=1.0)
    await asyncio.wait_for(first_task, timeout=1.0)
    assert sup._spawn_in_flight is True

    release_second.set()
    await asyncio.wait_for(second_task, timeout=1.0)
    assert sup._spawn_in_flight is False


async def test_send_agent_status_uses_connected_ws():
    """given: the supervisor is connected (self._ws set)
    when: the watchdog callback reports a status
    then: a copilot_agent_status frame with the current meeting id is sent
    """
    sent = []

    class FakeWS:
        async def send(self, payload):
            sent.append(payload)

    sup = Supervisor(runner=FakeRunner())
    sup.lifecycle.adopt("mtg-9")
    sup._ws = FakeWS()

    await sup._send_agent_status(AgentState.WORKING)

    assert len(sent) == 1
    assert json.loads(sent[0]) == {
        "type": "copilot_agent_status",
        "status": "working",
        "meeting_id": "mtg-9",
    }


async def test_send_agent_status_dropped_when_disconnected():
    """given: no live bridge connection (self._ws is None)
    when: the watchdog reports a status
    then: it is dropped silently — no frame reaches the (detached) socket and
          the call does not raise (the bridge merge reads silence as offline)
    """
    sent = []

    class FakeWS:
        async def send(self, payload):
            sent.append(payload)

    detached = FakeWS()
    sup = Supervisor(runner=FakeRunner())
    sup._ws = detached
    sup._ws = None  # connection dropped before the report

    await sup._send_agent_status(AgentState.ONLINE)  # must not raise

    assert sent == []


async def test_run_tracks_ws_and_starts_watchdog():
    """given: a watchdog-capable runner and a mock bridge
    when: the supervisor connects
    then: self._ws is set so status can flow and the watchdog task is live;
          stop() tears the watchdog task down
    """
    fake_tmux = FakeTmux(has_session=True)
    fake_tmux.responses["capture-pane"] = (0, REPL_PROMPT_SCREEN)
    runner = TmuxRunner(run=fake_tmux)

    async def handler(websocket):
        await websocket.recv()  # supervisor_hello
        await asyncio.sleep(0.3)

    server, port = await _start_mock_bridge(handler)
    sup = Supervisor(f"ws://127.0.0.1:{port}", runner=runner)
    assert sup.watchdog is not None
    await sup.start()
    try:
        for _ in range(20):
            if sup._ws is not None:
                break
            await asyncio.sleep(0.05)
        assert sup._ws is not None
        assert sup._watchdog_task is not None and not sup._watchdog_task.done()
    finally:
        await sup.stop()
    assert sup._watchdog_task is None
