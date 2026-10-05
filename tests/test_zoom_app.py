"""Tests for recog/zoom_app.py — the native Zoom application signal source."""

import asyncio
import contextlib
import json
import os
import plistlib
import threading
from pathlib import Path

import pytest

from recog import zoom_app
from recog.zoom_app import (
    ENGLISH,
    JOIN_RETRY_S,
    LEAVE_DEBOUNCE_S,
    MEETING_URL,
    Tile,
    ZoomAppSession,
    account_name,
    find_or_build_helper,
    load_vocabulary,
    parse_tile,
)

ACCOUNT = "Zoom, Me, In a Zoom Meeting, Basic account"


def tile(name, *states):
    return {"name": name, "description": ", ".join([name, *states])}


def meeting(*tiles, view="window", **extra):
    return {"state": "meeting", "view": view, "tiles": list(tiles), **extra}


def joined_session(now=100.0):
    session = ZoomAppSession()
    assert [f["type"] for f in session.frames(meeting(tile("Host")), now)] == ["meeting_join"]
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


def _zoom_app(tmp_path, **languages):
    """A stand-in Zoom bundle with one Localizable.strings per language."""
    app = tmp_path / "zoom.us.app"
    for language, entries in languages.items():
        folder = app / f"Contents/Resources/{language}.lproj"
        folder.mkdir(parents=True)
        body = "".join(f'"{key}" = "{value}";\n' for key, value in entries.items())
        (folder / "Localizable.strings").write_text(body, encoding="utf-16")
    return app


RUSSIAN = {
    "LN_Plist_Accessibility_VoIP_Muted_20044": "Звук компьютера выключен",
    "LN_Plist_Accessibility_VoIP_Unmuted_20044": "Звук компьютера включен",
    "LN_Plist_Accessibility_Tele_Muted_20044": "Звук телефона выключен",
    "LN_Audio_Unmuted_Accessibility_1152855": "Звук включен",
    "LN_Plist_Accessibility_Video_Off_20044": "Видео выкл.",
}


def test_given_installed_zoom_when_vocabulary_is_loaded_then_it_has_its_mute_wording_and_english(
    tmp_path,
):
    vocabulary = load_vocabulary((_zoom_app(tmp_path, ru=RUSSIAN),))

    assert {
        "звук компьютера выключен",
        "звук телефона выключен",
        "computer audio muted",
    } <= vocabulary.muted
    assert {
        "звук компьютера включен",
        "звук включен",
        "computer audio unmuted",
    } <= vocabulary.unmuted
    assert "видео выкл." not in vocabulary.muted | vocabulary.unmuted


def test_given_string_ids_of_another_zoom_version_when_loaded_then_keys_still_match(tmp_path):
    renumbered = {"LN_Plist_Accessibility_VoIP_Muted_99999": "Ton aus"}

    assert "ton aus" in load_vocabulary((_zoom_app(tmp_path, de=renumbered),)).muted


def test_given_no_zoom_or_an_unreadable_file_when_loaded_then_english_wording_remains(tmp_path):
    app = _zoom_app(tmp_path, ru=RUSSIAN)
    (app / "Contents/Resources/ru.lproj/Localizable.strings").write_bytes(b"\xff\xfe\x00\xd8")

    assert load_vocabulary((tmp_path / "absent.app", app)) == ENGLISH


def test_given_russian_interface_when_a_tile_is_parsed_with_loaded_wording_then_mute_and_speaker_are_read(
    tmp_path,
):
    vocabulary = load_vocabulary((_zoom_app(tmp_path, ru=RUSSIAN),))

    muted = parse_tile(
        "Иван", "Иван, Звук компьютера выключен, Видео выкл., active speaker", vocabulary
    )
    speaking = parse_tile(
        "Иван", "Иван, Звук компьютера включен, Видео выкл., active speaker", vocabulary
    )

    assert muted == Tile("Иван", True, True)
    assert speaking == Tile("Иван", False, True)


def test_given_russian_interface_when_session_runs_with_loaded_wording_then_a_muted_speaker_is_not_reported(
    tmp_path,
):
    session = ZoomAppSession(load_vocabulary((_zoom_app(tmp_path, ru=RUSSIAN),)))
    session.frames(meeting(tile("Host")), 100.0)

    frames = session.frames(
        meeting(
            tile("Иван", "Звук компьютера выключен", "active speaker"),
            tile("Анна", "Звук компьютера включен"),
        ),
        101.0,
    )

    assert [p["micMuted"] for p in frames[0]["participants"]] == [True, False]
    assert speaker_of(frames) is None


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


def test_given_join_preview_without_tiles_when_processed_then_nothing_until_the_first_tile():
    # The preview shown before "Start" is already the meeting window.
    session = ZoomAppSession()

    assert session.frames(meeting(), 100.0) == []
    assert session.joined is False
    assert types(session.frames(meeting(tile("Me")), 104.0)) == ["meeting_join"]


def test_given_preview_cancelled_when_the_window_goes_away_then_no_leave_is_sent():
    session = ZoomAppSession()
    session.frames(meeting(), 100.0)

    assert session.frames({"state": "idle"}, 100.0 + LEAVE_DEBOUNCE_S + 1) == []


def test_given_joined_meeting_when_tiles_vanish_then_it_stays_joined():
    session = joined_session()

    frames = session.frames(meeting(), 101.0)

    assert session.joined is True
    assert "meeting_leave" not in types(frames)


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

    assert types(session.frames(meeting(tile("Host")), 102.0)) == ["meeting_join"]


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


def test_given_tiles_scroll_off_screen_when_processed_then_the_roster_keeps_everyone_seen():
    # A small window or a large meeting shows a page of tiles; the speaker is
    # always among them, the rest of the roster is not.
    session = joined_session()
    session.frames(meeting(tile("Alice", "active speaker"), tile("Bob"), tile("Carol")), 101.0)

    frames = session.frames(meeting(tile("Bob", "active speaker")), 102.0)

    assert types(frames) == ["speaker_update"]  # the roster did not shrink
    assert speaker_of(frames)["name"] == "Bob"
    later = session.frames(meeting(tile("Dave", "Computer audio muted")), 103.0)
    assert [p["name"] for p in later[0]["participants"]] == ["Alice", "Bob", "Carol", "Dave"]


def test_given_a_new_meeting_when_joined_then_the_roster_starts_empty():
    session = joined_session(now=100.0)
    session.frames(meeting(tile("Alice")), 101.0)
    session.frames({"state": "idle"}, 200.0)
    session.frames(meeting(tile("Host")), 300.0)

    frames = session.frames(meeting(tile("Bob")), 301.0)

    assert [p["name"] for p in frames[0]["participants"]] == ["Bob"]


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


def test_given_helper_reports_a_topic_when_processed_then_no_title_is_sent():
    # Observed live: the "topic" read "Meeting  40-minutes", the free-plan timer.
    session = joined_session()

    frames = session.frames(meeting(tile("Bob"), topic="Meeting  40-minutes"), 101.0)

    assert "meeting_title_update" not in types(frames)


# --- helper application ------------------------------------------------------


def _repo(tmp_path, source="// source"):
    path = tmp_path / zoom_app.HELPER_SOURCE
    path.parent.mkdir(parents=True)
    path.write_text(source)
    return tmp_path


def _fake_toolchain(monkeypatch, *, fail=None):
    """swiftc and codesign that write what the real ones would; returns the calls."""
    calls = []

    def fake_run(command, **kwargs):
        calls.append(command)
        if command[0] == fail:
            return type("Result", (), {"returncode": 1, "stderr": "boom"})()
        if command[0] == "swiftc":
            Path(command[command.index("-o") + 1]).write_text("binary")
        if command[0] == "codesign":
            signature = Path(command[-1]) / "Contents/_CodeSignature"
            signature.mkdir(exist_ok=True)
            (signature / "CodeResources").write_text("seal")
        return type("Result", (), {"returncode": 0, "stderr": ""})()

    monkeypatch.setattr(zoom_app.shutil, "which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr(zoom_app.subprocess, "run", fake_run)
    return calls


def test_given_no_helper_when_located_then_a_signed_bundle_with_a_stable_identifier_is_built(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    monkeypatch.delenv("RECOG_CODESIGN_IDENTITY", raising=False)
    calls = _fake_toolchain(monkeypatch)

    app = find_or_build_helper(repo)

    assert app == repo / "RecogZoomHelper.app"
    info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
    assert info["CFBundleIdentifier"] == "com.recog.zoom-helper"
    assert info["CFBundleName"] == "Recog Zoom Helper"
    assert info["LSBackgroundOnly"] is True
    assert [command[0] for command in calls] == ["swiftc", "codesign"]
    sign = calls[1]
    assert sign[sign.index("--sign") + 1] == "-"
    assert sign[sign.index("--identifier") + 1] == "com.recog.zoom-helper"


def test_given_signing_identity_configured_when_built_then_the_bundle_is_signed_with_it(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    monkeypatch.setenv("RECOG_CODESIGN_IDENTITY", "Developer ID Application: Example")
    calls = _fake_toolchain(monkeypatch)

    find_or_build_helper(repo)

    assert calls[1][calls[1].index("--sign") + 1] == "Developer ID Application: Example"


def test_given_built_helper_when_the_source_is_only_touched_then_it_is_not_rebuilt(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    calls = _fake_toolchain(monkeypatch)
    find_or_build_helper(repo)
    source = repo / zoom_app.HELPER_SOURCE
    later = source.stat().st_mtime + 3600

    os.utime(source, (later, later))  # what a checkout does; the content is the same
    find_or_build_helper(repo)

    assert len(calls) == 2  # a rebuild would lose the granted permission


def test_given_built_helper_when_the_source_content_changes_then_it_is_rebuilt(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    calls = _fake_toolchain(monkeypatch)
    find_or_build_helper(repo)

    (repo / zoom_app.HELPER_SOURCE).write_text("// changed")
    find_or_build_helper(repo)

    assert [command[0] for command in calls] == ["swiftc", "codesign", "swiftc", "codesign"]


def test_given_stale_helper_and_no_compiler_when_located_then_the_stale_helper_is_used(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    _fake_toolchain(monkeypatch)
    app = find_or_build_helper(repo)
    (repo / zoom_app.HELPER_SOURCE).write_text("// changed")
    monkeypatch.setattr(zoom_app.shutil, "which", lambda name: None)

    assert find_or_build_helper(repo) == app


def test_given_no_helper_and_no_compiler_when_located_then_error_names_the_fix(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    monkeypatch.setattr(zoom_app.shutil, "which", lambda name: None)

    with pytest.raises(RuntimeError, match="xcode-select --install"):
        find_or_build_helper(repo)


@pytest.mark.parametrize("failing", ["swiftc", "codesign"])
def test_given_a_build_step_fails_when_building_then_the_error_names_it_and_the_next_call_rebuilds(
    tmp_path, monkeypatch, failing
):
    repo = _repo(tmp_path)
    _fake_toolchain(monkeypatch, fail=failing)

    with pytest.raises(RuntimeError, match=rf"\({failing}\): boom"):
        find_or_build_helper(repo)

    calls = _fake_toolchain(monkeypatch)
    find_or_build_helper(repo)
    assert [command[0] for command in calls] == ["swiftc", "codesign"]


# --- helper link -------------------------------------------------------------


def _launching(monkeypatch, behave):
    """`open` that starts `behave(socket_path)` as the helper would be started."""
    launched = []

    def fake_run(command, **kwargs):
        launched.append(command)
        task = behave(command[command.index("--socket") + 1])
        if task is not None:
            asyncio.ensure_future(task)
        return type("Result", (), {"returncode": 0, "stderr": ""})()

    monkeypatch.setattr(zoom_app.subprocess, "run", fake_run)
    return launched


@pytest.mark.asyncio
async def test_given_helper_application_when_started_then_it_is_launched_by_launchservices_and_its_lines_arrive(
    monkeypatch, tmp_path
):
    async def helper(socket_path):
        _, writer = await asyncio.open_unix_connection(socket_path)
        writer.write(b'{"state": "idle"}\n')
        await writer.drain()
        writer.close()

    launched = _launching(monkeypatch, helper)
    app = tmp_path / "RecogZoomHelper.app"

    async with zoom_app.helper_lines(app) as lines:
        first = await lines.readline()
        rest = await lines.readline()

    assert json.loads(first) == {"state": "idle"}
    assert rest == b""  # the helper went away: the reader sees EOF
    command = launched[0]
    assert command[:5] == ["open", "-g", "-n", "-a", str(app)]
    socket_path = Path(command[command.index("--socket") + 1])
    assert not socket_path.parent.exists()  # the private directory is removed


@pytest.mark.asyncio
async def test_given_helper_never_connects_when_started_then_error_says_so(monkeypatch, tmp_path):
    _launching(monkeypatch, lambda socket_path: None)
    monkeypatch.setattr(zoom_app, "HELPER_CONNECT_S", 0.05)

    with pytest.raises(RuntimeError, match="did not connect"):
        async with zoom_app.helper_lines(tmp_path / "RecogZoomHelper.app"):
            pass


@pytest.mark.asyncio
async def test_given_launch_fails_when_started_then_error_carries_the_reason(monkeypatch, tmp_path):
    failure = type("Result", (), {"returncode": 1, "stderr": "Unable to find application"})()
    monkeypatch.setattr(zoom_app.subprocess, "run", lambda *a, **k: failure)

    with pytest.raises(
        RuntimeError, match="cannot launch RecogZoomHelper.app: Unable to find application"
    ):
        async with zoom_app.helper_lines(tmp_path / "RecogZoomHelper.app"):
            pass


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
    reader = _reader(meeting(tile("Host")), meeting(tile("Bob", "active speaker")))

    with pytest.raises(RuntimeError, match="the Zoom helper disconnected"):
        await zoom_app._pump(socket, reader, ZoomAppSession(), zoom_app._StatusLog())

    assert types(socket.sent) == ["meeting_join", "participants_update", "speaker_update"]


@pytest.mark.asyncio
async def test_given_bridge_rejects_the_join_when_pumped_then_no_state_frames_follow():
    socket = FakeSocket()
    session = ZoomAppSession()
    reader = asyncio.StreamReader()
    reader.feed_data((json.dumps(meeting(tile("Host"))) + "\n").encode())
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


# --- running inside the live process -----------------------------------------


def test_given_macos_when_started_in_background_then_a_daemon_thread_supervises_the_source(
    monkeypatch,
):
    supervised = threading.Event()
    monkeypatch.setattr(zoom_app.sys, "platform", "darwin")
    # The supervisor loops for the life of the process; a stand-in keeps the
    # thread from outliving the test and reaching the real helper.
    monkeypatch.setattr(zoom_app, "_supervise", lambda stop: supervised.set())

    thread = zoom_app.start_in_background()
    thread.join(timeout=5)

    assert thread.daemon is True and thread.name == "zoom-app"
    assert supervised.is_set() and not thread.is_alive()


def test_given_other_os_when_started_in_background_then_nothing_starts_and_it_says_why(
    monkeypatch, capsys
):
    monkeypatch.setattr(zoom_app.sys, "platform", "linux")

    assert zoom_app.start_in_background() is None
    assert "macOS only" in capsys.readouterr().err


def test_given_source_fails_when_supervised_then_it_is_logged_and_started_over(monkeypatch, capsys):
    stop = threading.Event()
    attempts = []

    async def flaky_run():
        attempts.append(1)
        if len(attempts) == 1:
            raise RuntimeError("the Zoom helper disconnected")
        stop.set()

    monkeypatch.setattr(zoom_app, "RESTART_S", 0)
    monkeypatch.setattr(zoom_app, "run", flaky_run)

    zoom_app._supervise(stop)

    assert len(attempts) == 2
    assert "stopped: the Zoom helper disconnected; starting over" in capsys.readouterr().err


@pytest.mark.parametrize(
    ("argv", "env_enabled", "expected"),
    [
        (["live", "--zoom-app"], False, True),
        (["live"], True, True),
        (["live"], False, False),
        (["live", "--zoom-app", "--list-devices"], False, False),
    ],
)
def test_given_live_command_when_run_then_zoom_source_starts_only_when_asked(
    monkeypatch, argv, env_enabled, expected
):
    import recog.config
    import recog.live
    from recog import cli

    started = []
    monkeypatch.setattr(cli.sys, "argv", ["recog", *argv])
    monkeypatch.setattr(recog.config, "RECOG_ZOOM_APP", env_enabled)
    monkeypatch.setattr(recog.live, "live_pipeline", lambda **kwargs: None)
    monkeypatch.setattr(zoom_app, "start_in_background", lambda: started.append(True))

    cli.main()

    assert bool(started) is expected


# --- run: helper and bridge link ---------------------------------------------


class FakeConnection:
    async def __aenter__(self):
        return object()

    async def __aexit__(self, *exc):
        return False


def _patch_run(monkeypatch, connect, pump):
    state = {"helper_open": False, "closed": False}

    @contextlib.asynccontextmanager
    async def fake_helper_lines(app):
        state["helper_open"] = True
        try:
            yield object()
        finally:
            state["closed"] = True

    monkeypatch.setattr(zoom_app, "find_or_build_helper", lambda: Path("/tmp/RecogZoomHelper.app"))
    monkeypatch.setattr(zoom_app, "helper_lines", fake_helper_lines)
    monkeypatch.setattr(zoom_app, "RECONNECT_S", 0)
    monkeypatch.setattr("websockets.connect", connect)
    monkeypatch.setattr(zoom_app, "_pump", pump)
    return state


@pytest.mark.asyncio
async def test_given_bridge_down_then_up_when_running_then_it_reconnects_with_the_same_helper(
    monkeypatch, capsys
):
    attempts = []
    sessions = []

    def connect(url):
        attempts.append(url)
        if len(attempts) == 1:
            raise OSError("connection refused")
        return FakeConnection()

    async def pump(websocket, lines, session, log):
        sessions.append(session)
        if len(sessions) == 1:
            session.joined = True
            return  # the bridge closed the link
        raise RuntimeError("the Zoom helper disconnected")

    state = _patch_run(monkeypatch, connect, pump)

    with pytest.raises(RuntimeError, match="the Zoom helper disconnected"):
        await zoom_app.run("ws://bridge.test")

    assert attempts == ["ws://bridge.test"] * 3
    assert "bridge link lost: connection refused" in capsys.readouterr().err
    assert (
        sessions[0] is sessions[1] and sessions[1].joined is False
    )  # on_disconnect ran in between
    assert state == {"helper_open": True, "closed": True}
