"""Concurrent recording transitions must retain conference identity."""

import json
import threading
from concurrent.futures import ThreadPoolExecutor

from recog.meeting_storage import MeetingStorage


def test_orphan_split_when_title_waits_and_conference_joins_then_join_stays_active(
    tmp_path, monkeypatch
):
    """given a slow title - when KTalk joins during a split - then keep KTalk."""
    storage = MeetingStorage(tmp_path / "meetings")
    storage.start_session(ktalk=True)
    storage.append_segment("Alice", "alice", "Previous recording speech")
    orphan_path = storage.current_meeting_path
    title_started = threading.Event()
    release_title = threading.Event()

    def slow_title(*args, **kwargs):
        title_started.set()
        assert release_title.wait(5), "title was not released"
        return "Previous recording"

    monkeypatch.setattr("recog.title_generator.generate_title", slow_title)
    with ThreadPoolExecutor(max_workers=2) as workers:
        split = workers.submit(storage.split_on_silence_timeout)
        try:
            assert title_started.wait(5)
            join = workers.submit(storage.on_meeting_join, "https://example.ktalk.ru/active-room")
            joined_id = join.result(timeout=2)
            # Neither transition may wait for an AI provider.
            split.result(timeout=2)
        finally:
            release_title.set()
        assert storage.current_meeting_id == joined_id
        meta = storage._load_meta()
        assert meta["id"] == joined_id
        assert meta["source"] == "ktalk"
        assert meta["conference"]["live_id"] == "active-room"
        assert meta["ended_at"] is None
        storage.append_segment("Bob", "bob", "Current conference speech")
        storage.end_session()

    orphan_meta = json.loads((orphan_path / "meta.json").read_text())
    assert orphan_meta["id"] == orphan_path.name
    assert orphan_meta["title"] == "Previous recording"
    assert orphan_meta["end_reason"] == "silence_timeout"
    conference_meta = json.loads((tmp_path / "meetings" / joined_id / "meta.json").read_text())
    assert conference_meta["source"] == "ktalk"
    assert conference_meta["title"] is None


def test_orphan_when_joined_from_event_loop_then_title_finishes_for_old_recording(
    tmp_path, monkeypatch
):
    """given an orphan - when the async bridge joins - then title runs off-loop."""
    import asyncio

    storage = MeetingStorage(tmp_path / "meetings")
    storage.start_session(ktalk=True)
    storage.append_segment("Alice", "alice", "Earlier recording")
    old_path = storage.current_meeting_path

    async def generate(*args, **kwargs):
        return "Earlier title"

    monkeypatch.setattr("recog.title_generator.generate_title_async", generate)

    async def join():
        storage.on_meeting_join("https://example.ktalk.ru/room")

    asyncio.run(join())
    storage.end_session()
    assert json.loads((old_path / "meta.json").read_text())["title"] == "Earlier title"


def test_pending_title_when_audio_paths_saved_then_preserves_audio_metadata(tmp_path, monkeypatch):
    """given pending AI work - when WAVs rotate - then both updates survive."""
    storage = MeetingStorage(tmp_path / "meetings")
    storage.start_session(ktalk=True)
    storage.append_segment("Alice", "alice", "Earlier recording")
    old_path = storage.current_meeting_path
    started = threading.Event()
    release = threading.Event()

    def title(*args, **kwargs):
        started.set()
        assert release.wait(5)
        return "Earlier title"

    monkeypatch.setattr("recog.title_generator.generate_title", title)
    try:
        storage.split_on_silence_timeout()
        assert started.wait(5)
        storage.set_audio_paths(system_path="old.wav", meeting_path=old_path)
    finally:
        release.set()
        storage.end_session()
    meta = json.loads((old_path / "meta.json").read_text())
    assert meta["audio_system_path"] == "old.wav"
    assert meta["title"] == "Earlier title"
    index = json.loads((storage.base_path / "index.json").read_text())
    assert (
        next(m for m in index["meetings"] if m["id"] == old_path.name)["title"] == "Earlier title"
    )
