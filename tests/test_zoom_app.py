"""Tests for recog/zoom_app.py — the native Zoom application signal source."""

import asyncio
import json

import pytest

from recog import zoom_app
from recog.zoom_app import (
    JOIN_RETRY_S,
    LEAVE_DEBOUNCE_S,
    MEETING_URL,
    Tile,
    ZoomAppSession,
    account_name,
    find_or_build_probe,
    parse_tile,
)

ACCOUNT = "Zoom, Me, In a Zoom Meeting, Basic account"


def tile(name, *states):
    return {"name": name, "description": ", ".join([name, *states])}


def meeting(*tiles, view="window", **extra):
    return {"state": "meeting", "view": view, "tiles": list(tiles), **extra}


def joined_session(now=100.0):
    session = ZoomAppSession()
    assert [f["type"] for f in session.frames(meeting(), now)] == ["meeting_join"]
    return session


def types(frames):
    return [frame["type"] for frame in frames]


# --- parsing -----------------------------------------------------------------


@pytest.mark.parametrize(
    ("description", "expected"),
    [
        ("Alice, Computer audio unmuted, Video off, active speaker", Tile("Alice", False, True)),
        ("Alice, Computer audio muted, Video off", Tile("Alice", True, False)),
        ("Alice, Video off", Tile("Alice", None, False)),
        ("Alice", Tile("Alice", None, False)),
        ("Alice, Звук компьютера включён, активный докладчик", Tile("Alice", None, False)),
        ("Someone else, Computer audio muted, active speaker", Tile("Alice", None, False)),
    ],
)
def test_given_tile_description_when_parsed_then_only_known_wording_counts(description, expected):
    assert parse_tile("Alice", description) == expected


def test_given_name_with_a_comma_when_parsed_then_the_name_is_not_read_as_state():
    parsed = parse_tile(
        "Doe, Jane muted", "Doe, Jane muted, Computer audio unmuted, active speaker"
    )
    assert parsed == Tile("Doe, Jane muted", False, True)


@pytest.mark.parametrize(
    ("description", "expected"),
    [
        (ACCOUNT, "Me"),
        ("Zoom, Иван Петров, Available, Basic account", "Иван Петров"),
        ("Zoom, Me", None),
        ("Search, Me, something", None),
        ("", None),
        (None, None),
    ],
)
def test_given_account_button_description_when_read_then_display_name_or_none(
    description, expected
):
    assert account_name(description) == expected


# --- session: lifecycle ------------------------------------------------------


@pytest.mark.parametrize("state", ["idle", "zoom_not_running", "not_trusted"])
def test_given_no_meeting_when_snapshots_arrive_then_nothing_is_sent(state):
    assert ZoomAppSession().frames({"state": state}, 100.0) == []


def test_given_first_meeting_snapshot_when_processed_then_only_the_join_is_sent():
    session = ZoomAppSession()

    frames = session.frames(meeting(tile("Bob", "Computer audio unmuted", "active speaker")), 100.0)

    assert frames == [{"type": "meeting_join", "url": MEETING_URL, "platform": "zoom", "title": ""}]


def test_given_joined_when_next_snapshot_arrives_then_participants_and_speaker_follow():
    session = joined_session()
    snapshot = meeting(
        tile("Me", "Computer audio muted"),
        tile("Bob", "Computer audio unmuted", "Video off", "active speaker"),
        account=ACCOUNT,
    )

    frames = session.frames(snapshot, 101.0)

    assert frames == [
        {
            "type": "participants_update",
            "participants": [
                {"id": "Me", "name": "Me", "isLocal": True, "micMuted": True},
                {"id": "Bob", "name": "Bob", "isLocal": False, "micMuted": False},
            ],
        },
        {
            "type": "speaker_update",
            "speaker": {"id": "Bob", "name": "Bob", "isLocal": False},
            "timestamp": 101000,
        },
    ]


def test_given_unchanged_snapshot_when_heartbeat_repeats_it_then_nothing_is_resent():
    session = joined_session()
    snapshot = meeting(tile("Bob", "Computer audio unmuted", "active speaker"))
    session.frames(snapshot, 101.0)

    assert session.frames(snapshot, 103.0) == []


def test_given_meeting_window_gone_briefly_when_it_returns_then_no_leave_is_sent():
    session = joined_session(now=100.0)

    assert session.frames({"state": "idle"}, 100.0 + LEAVE_DEBOUNCE_S - 0.1) == []
    assert session.joined is True


def test_given_meeting_window_gone_for_the_debounce_when_processed_then_leave_once():
    session = joined_session(now=100.0)

    assert session.frames({"state": "idle"}, 100.0 + LEAVE_DEBOUNCE_S) == [
        {"type": "meeting_leave", "reason": "user_left"}
    ]
    assert session.frames({"state": "idle"}, 200.0) == []


def test_given_left_meeting_when_a_new_one_starts_then_join_and_full_state_are_sent_again():
    session = joined_session(now=100.0)
    session.frames(meeting(tile("Bob", "active speaker")), 101.0)
    session.frames({"state": "idle"}, 200.0)

    assert types(session.frames(meeting(tile("Bob", "active speaker")), 300.0)) == ["meeting_join"]
    assert types(session.frames(meeting(tile("Bob", "active speaker")), 301.0)) == [
        "participants_update",
        "speaker_update",
    ]


def test_given_join_rejected_when_meeting_continues_then_silent_until_the_retry_window_passes():
    session = joined_session(now=100.0)
    session.on_rejected(100.5)
    snapshot = meeting(tile("Bob", "active speaker"))

    assert session.frames(snapshot, 101.0) == []
    assert session.frames(snapshot, 100.5 + JOIN_RETRY_S - 0.1) == []
    assert types(session.frames(snapshot, 100.5 + JOIN_RETRY_S)) == ["meeting_join"]


def test_given_rejected_then_meeting_ended_when_a_new_meeting_starts_then_join_is_immediate():
    session = joined_session(now=100.0)
    session.on_rejected(100.5)
    session.frames({"state": "idle"}, 101.0)

    assert types(session.frames(meeting(), 102.0)) == ["meeting_join"]


def test_given_bridge_link_dropped_when_meeting_continues_then_join_is_sent_again():
    session = joined_session(now=100.0)
    session.frames(meeting(tile("Bob", "active speaker")), 101.0)
    session.on_disconnect()

    assert types(session.frames(meeting(tile("Bob", "active speaker")), 105.0)) == ["meeting_join"]
    assert types(session.frames(meeting(tile("Bob", "active speaker")), 106.0)) == [
        "participants_update",
        "speaker_update",
    ]


# --- session: speaker and roster ---------------------------------------------


def speaker_of(frames):
    return next(f["speaker"] for f in frames if f["type"] == "speaker_update")


def test_given_active_mark_moves_when_processed_then_speaker_changes():
    session = joined_session()
    session.frames(meeting(tile("Alice", "active speaker"), tile("Bob")), 101.0)

    frames = session.frames(meeting(tile("Alice"), tile("Bob", "active speaker")), 102.0)

    assert types(frames) == ["speaker_update"]
    assert speaker_of(frames)["name"] == "Bob"


def test_given_nobody_marked_when_processed_then_speaker_is_null():
    session = joined_session()

    frames = session.frames(meeting(tile("Alice"), tile("Bob")), 101.0)

    assert speaker_of(frames) is None


def test_given_marked_speaker_is_muted_when_processed_then_speaker_is_null():
    session = joined_session()

    frames = session.frames(meeting(tile("Alice", "Computer audio muted", "active speaker")), 101.0)

    assert speaker_of(frames) is None


def test_given_own_tile_marked_when_processed_then_speaker_is_local():
    session = joined_session()

    frames = session.frames(meeting(tile("Me", "active speaker"), account=ACCOUNT), 101.0)

    assert speaker_of(frames) == {"id": "Me", "name": "Me", "isLocal": True}


def test_given_minimal_view_when_processed_then_its_single_tile_is_the_speaker_and_roster_is_kept():
    session = joined_session()
    session.frames(meeting(tile("Alice", "active speaker"), tile("Bob")), 101.0)

    frames = session.frames(meeting(tile("Bob", "Computer audio unmuted"), view="minimal"), 102.0)

    assert types(frames) == ["speaker_update"]
    assert speaker_of(frames)["name"] == "Bob"


def test_given_minimal_view_with_a_muted_tile_when_processed_then_speaker_is_null():
    session = joined_session()

    frames = session.frames(meeting(tile("Bob", "Computer audio muted"), view="minimal"), 101.0)

    assert speaker_of(frames) is None


def test_given_namesakes_one_muted_when_processed_then_one_unmuted_participant():
    session = joined_session()

    frames = session.frames(
        meeting(tile("Alex", "Computer audio muted"), tile("Alex", "Computer audio unmuted")), 101.0
    )

    assert frames[0]["participants"] == [
        {"id": "Alex", "name": "Alex", "isLocal": False, "micMuted": False}
    ]


def test_given_tile_without_a_name_when_processed_then_skipped():
    session = joined_session()

    frames = session.frames(meeting({"name": "", "description": "x"}, tile("Bob")), 101.0)

    assert [p["name"] for p in frames[0]["participants"]] == ["Bob"]


def test_given_topic_seen_while_the_toolbar_is_shown_when_processed_then_title_is_sent_once():
    session = joined_session()

    first = session.frames(meeting(tile("Bob"), topic="Weekly sync"), 101.0)
    second = session.frames(meeting(tile("Bob"), topic="Weekly sync"), 102.0)
    third = session.frames(meeting(tile("Bob")), 103.0)

    assert first[0] == {"type": "meeting_title_update", "title": "Weekly sync"}
    assert second == [] and third == []


# --- helper binary -----------------------------------------------------------


def _repo(tmp_path, *, binary_age=None):
    source = tmp_path / "recog/native/macos/zoom_ax_probe.swift"
    source.parent.mkdir(parents=True)
    source.write_text("// source")
    if binary_age is not None:
        binary = tmp_path / "zoom_ax_probe"
        binary.write_text("binary")
        stamp = source.stat().st_mtime + binary_age
        import os

        os.utime(binary, (stamp, stamp))
    return tmp_path


def test_given_fresh_binary_when_located_then_returned_without_compiling(tmp_path, monkeypatch):
    repo = _repo(tmp_path, binary_age=10)
    monkeypatch.setattr(zoom_app.subprocess, "run", lambda *a, **k: pytest.fail("must not compile"))

    assert find_or_build_probe(repo) == str(repo / "zoom_ax_probe")


def test_given_stale_binary_and_no_compiler_when_located_then_the_stale_binary_is_used(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path, binary_age=-10)
    monkeypatch.setattr(zoom_app.shutil, "which", lambda name: None)

    assert find_or_build_probe(repo) == str(repo / "zoom_ax_probe")


def test_given_no_binary_and_no_compiler_when_located_then_error_names_the_fix(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    monkeypatch.setattr(zoom_app.shutil, "which", lambda name: None)

    with pytest.raises(RuntimeError, match="xcode-select --install"):
        find_or_build_probe(repo)


def test_given_no_binary_when_compiler_succeeds_then_swiftc_builds_it(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    calls = []

    def fake_run(command, **kwargs):
        calls.append(command)
        return type("Result", (), {"returncode": 0, "stderr": ""})()

    monkeypatch.setattr(zoom_app.shutil, "which", lambda name: "/usr/bin/swiftc")
    monkeypatch.setattr(zoom_app.subprocess, "run", fake_run)

    assert find_or_build_probe(repo) == str(repo / "zoom_ax_probe")
    assert (
        calls[0][0] == "swiftc" and str(repo / "recog/native/macos/zoom_ax_probe.swift") in calls[0]
    )


def test_given_compiler_fails_when_building_then_error_carries_its_output(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    failure = type("Result", (), {"returncode": 1, "stderr": "boom"})()
    monkeypatch.setattr(zoom_app.shutil, "which", lambda name: "/usr/bin/swiftc")
    monkeypatch.setattr(zoom_app.subprocess, "run", lambda *a, **k: failure)

    with pytest.raises(RuntimeError, match="boom"):
        find_or_build_probe(repo)


# --- pump --------------------------------------------------------------------


class FakeSocket:
    def __init__(self):
        self.sent = []
        self.incoming = asyncio.Queue()

    async def send(self, raw):
        self.sent.append(json.loads(raw))

    async def recv(self):
        return await self.incoming.get()


def _reader(*snapshots):
    reader = asyncio.StreamReader()
    for snapshot in snapshots:
        reader.feed_data((json.dumps(snapshot) + "\n").encode())
    reader.feed_eof()
    return reader


@pytest.mark.asyncio
async def test_given_helper_snapshots_when_pumped_then_frames_reach_the_bridge_and_eof_is_an_error():
    socket = FakeSocket()
    reader = _reader(meeting(), meeting(tile("Bob", "active speaker")))

    with pytest.raises(RuntimeError, match="zoom_ax_probe exited"):
        await zoom_app._pump(socket, reader, ZoomAppSession(), zoom_app._StatusLog())

    assert types(socket.sent) == ["meeting_join", "participants_update", "speaker_update"]


@pytest.mark.asyncio
async def test_given_bridge_rejects_the_join_when_pumped_then_no_state_frames_follow():
    socket = FakeSocket()
    session = ZoomAppSession()
    reader = asyncio.StreamReader()
    reader.feed_data((json.dumps(meeting()) + "\n").encode())
    pump = asyncio.ensure_future(zoom_app._pump(socket, reader, session, zoom_app._StatusLog()))
    await asyncio.sleep(0.01)

    await socket.incoming.put(
        json.dumps({"type": "meeting_join_rejected", "reason": "another_tab_active"})
    )
    await asyncio.sleep(0.01)
    reader.feed_data((json.dumps(meeting(tile("Bob", "active speaker"))) + "\n").encode())
    reader.feed_eof()
    with pytest.raises(RuntimeError):
        await pump

    assert types(socket.sent) == ["meeting_join"]
    assert session.joined is False


def test_given_helper_state_changes_when_logged_then_one_line_per_change(capsys):
    log = zoom_app._StatusLog()

    for state in ["not_trusted", "not_trusted", "idle", "meeting", "weird"]:
        log.note(state)

    lines = capsys.readouterr().err.strip().splitlines()
    assert len(lines) == 4
    assert "Accessibility" in lines[0] and "unknown helper state: weird" in lines[3]
