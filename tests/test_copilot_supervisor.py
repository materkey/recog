"""Tests for recog/copilot_supervisor.py — lifecycle state machine + bridge glue."""
import asyncio
import json
import sys
from pathlib import Path

import pytest
import websockets

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.copilot_supervisor import (
    Kill,
    SessionLifecycle,
    Spawn,
    State,
    Supervisor,
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


async def test_autospawn_off_is_noop():
    """given: RECOG_COPILOT_AUTOSPAWN=0 (autospawn=False)
    when: lifecycle and meeting_state frames arrive
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
# Bridge connection: supervisor_hello + reconnect behaviour
# ---------------------------------------------------------------------------


async def _start_mock_bridge(handler):
    server = await websockets.serve(handler, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    return server, port


async def test_supervisor_sends_hello_on_connect():
    """given: a mock bridge accepting connections
    when: the supervisor connects
    then: the first frame it sends is `{"type": "supervisor_hello"}`
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
        for _ in range(20):
            if received:
                break
            await asyncio.sleep(0.05)
        assert received, "supervisor did not send any frame"
        assert json.loads(received[0]) == {"type": "supervisor_hello"}
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
