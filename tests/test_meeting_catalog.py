"""Catalog consistency across recorder writes and filesystem notification loss."""

import asyncio
import json
import os
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest
from watchdog.events import FileModifiedEvent, FileMovedEvent

from recog.web.meeting_catalog import MeetingCatalog


def write_meta(root, meeting_id="meeting", **updates):
    meta = {
        "id": meeting_id, "created_at": "2026-09-01T10:00:00+00:00",
        "ended_at": "2026-09-01T11:00:00+00:00", "title": "Original",
        "source": "ktalk", "stats": {"segment_count": 3, "duration_seconds": 3600},
        "participants": {"alice": {"name": "Alice"}},
        "conference": {"live_url": "https://example.test/meeting"},
    }
    meta.update(updates)
    path = root / meeting_id / "meta.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(meta))
    return path


def write_index(root, *rows):
    path = root / "index.json"
    path.write_text(json.dumps({"meetings": list(rows)}))
    return path


async def until(predicate):
    async with asyncio.timeout(2):
        while not predicate():
            await asyncio.sleep(0.005)


async def test_cached_files_when_reconciled_then_unchanged_json_is_not_reopened(tmp_path, monkeypatch):
    write_meta(tmp_path)
    write_index(tmp_path)
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    read = Path.read_text
    reads = []

    def counted(path, *args, **kwargs):
        reads.append(path)
        return read(path, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", counted)
    await catalog.refresh()
    assert reads == []

    def no_filesystem(*args, **kwargs):
        raise AssertionError("snapshot reads must not touch files")

    monkeypatch.setattr(Path, "stat", no_filesystem)
    monkeypatch.setattr(os, "scandir", no_filesystem)
    assert catalog.list_meetings()[0]["title"] == "Original"


async def test_index_and_metadata_when_merged_then_metadata_wins_and_archives_remain(tmp_path):
    write_meta(tmp_path)
    write_index(tmp_path,
                {"id": "meeting", "created_at": "2026-09-01", "title": "Stale"},
                {"id": "archive", "created_at": "2026-08-01", "title": "Archive"})
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    assert [(row["id"], row["title"]) for row in catalog.list_meetings()] == [
        ("meeting", "Original"), ("archive", "Archive"),
    ]


async def test_active_meeting_when_ended_and_reopened_then_no_gap_or_duplicate(tmp_path):
    meta = write_meta(tmp_path, ended_at=None)
    index = write_index(tmp_path)
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    assert catalog.list_meetings()[0]["is_active"]
    write_meta(tmp_path)
    await catalog.refresh({meta})  # ended metadata lands before the index write
    assert len(catalog.list_meetings()) == 1
    assert not catalog.list_meetings()[0].get("is_active")
    write_index(tmp_path, catalog.list_meetings()[0])
    await catalog.refresh({index})
    write_meta(tmp_path, ended_at=None)
    await catalog.refresh({meta})  # reopened metadata lands before index removal
    assert len(catalog.list_meetings()) == 1
    assert catalog.list_meetings()[0]["is_active"]
    write_index(tmp_path)
    await catalog.refresh({index})
    assert len(catalog.list_meetings()) == 1


async def test_active_snapshot_when_read_then_duration_is_current_and_copies_are_isolated(tmp_path):
    created = (datetime.now(timezone.utc) - timedelta(seconds=120)).isoformat()
    write_meta(tmp_path, ended_at=None, created_at=created)
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    rows = catalog.list_meetings()
    assert 120 <= rows[0]["duration_seconds"] <= 122
    rows[0]["participants"].append("Injected")
    rows[0]["title"] = "Injected"
    assert catalog.list_meetings()[0]["participants"] == ["Alice"]
    assert catalog.list_meetings()[0]["title"] == "Original"


async def test_external_metadata_when_edited_then_directory_mtime_does_not_hide_change(tmp_path):
    local, external = tmp_path / "local", tmp_path / "external"
    local.mkdir()
    meta = write_meta(external, ended_at=None)
    root_mtime = external.stat().st_mtime_ns
    catalog = MeetingCatalog(local, [(external, "export")])
    await catalog.refresh()
    write_meta(external, title="Edited", ended_at=None)
    assert external.stat().st_mtime_ns == root_mtime
    await catalog.refresh()
    row = catalog.list_meetings()[0]
    assert row["title"] == "Edited"
    assert row["source"] == "export"
    assert row["_external_path"] == str(external)
    assert row["conference_url"] == "https://example.test/meeting"
    assert "ktalk_live_url" not in row and "is_active" not in row
    meta.unlink()
    await catalog.refresh()
    assert catalog.list_meetings() == []


async def test_atomic_replace_when_mtime_and_size_match_then_new_inode_is_detected(tmp_path):
    meta = write_meta(tmp_path)
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    before = meta.stat()
    replacement = tmp_path / "replacement"
    replacement.write_text(meta.read_text().replace("Original", "Replaced"))
    os.utime(replacement, ns=(before.st_atime_ns, before.st_mtime_ns))
    replacement.replace(meta)
    await catalog.refresh()
    assert catalog.list_meetings()[0]["title"] == "Replaced"


async def test_replacement_during_read_when_refreshed_then_old_json_is_not_cached_as_new(tmp_path, monkeypatch):
    meta = write_meta(tmp_path)
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    write_meta(tmp_path, title="Intermediate")
    read = Path.read_text

    def replacing_read(path, *args, **kwargs):
        data = read(path, *args, **kwargs)
        if path == meta:
            replacement = tmp_path / "replacement"
            replacement.write_text(data.replace("Intermediate", "Final"))
            replacement.replace(path)
        return data

    monkeypatch.setattr(Path, "read_text", replacing_read)
    await catalog.refresh({meta})
    assert catalog.list_meetings()[0]["title"] == "Original"
    monkeypatch.setattr(Path, "read_text", read)
    await catalog.refresh(set())  # retries unstable reads even without a new event
    assert catalog.list_meetings()[0]["title"] == "Final"


@pytest.mark.parametrize("bad_json", ['{"id":', '[]', '{"id":"meeting"}'])
async def test_invalid_metadata_when_refreshed_then_last_good_entry_survives(tmp_path, bad_json):
    meta = write_meta(tmp_path)
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    meta.write_text(bad_json)
    await catalog.refresh({meta})
    assert catalog.list_meetings()[0]["title"] == "Original"
    write_meta(tmp_path, title="Recovered")
    await catalog.refresh(set())
    assert catalog.list_meetings()[0]["title"] == "Recovered"


async def test_invalid_index_when_refreshed_then_archive_survives_until_repaired(tmp_path):
    index = write_index(tmp_path, {"id": "archive", "created_at": "2026-08-01"})
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    index.write_text('{"meetings": [null]}')
    await catalog.refresh({index})
    assert catalog.list_meetings()[0]["id"] == "archive"
    write_index(tmp_path)
    await catalog.refresh(set())
    assert catalog.list_meetings() == []


@pytest.mark.parametrize("bad_fields", [{"created_at": 123}, {"participants": None},
                                       {"title": ["bad"]}, {"is_active": True, "created_at": None}])
async def test_invalid_index_fields_when_refreshed_then_last_good_archive_survives(tmp_path, bad_fields):
    write_meta(tmp_path)
    archived = {"id": "archive", "created_at": "2026-08-01", "title": "Valid"}
    index = write_index(tmp_path, archived)
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    write_index(tmp_path, archived | bad_fields)
    await catalog.refresh({index})
    assert catalog.list_meetings()[1] == archived


async def test_cancelled_refresh_when_another_starts_then_workers_remain_serialized(tmp_path, monkeypatch):
    write_meta(tmp_path)
    catalog = MeetingCatalog(tmp_path)
    await catalog.refresh()
    refresh = catalog._refresh_files
    entered, release, overlapping = threading.Event(), threading.Event(), threading.Event()
    first = True

    def paused(paths):
        nonlocal first
        if first:
            first = False
            entered.set()
            assert release.wait(2)
        else:
            overlapping.set()
        return refresh(paths)

    monkeypatch.setattr(catalog, "_refresh_files", paused)
    pending = asyncio.create_task(catalog.refresh())
    following = None
    try:
        await until(entered.is_set)
        pending.cancel()
        following = asyncio.create_task(catalog.refresh())
        await asyncio.sleep(0.03)
        assert not overlapping.is_set()
    finally:
        release.set()
        await asyncio.gather(*[task for task in (pending, following) if task], return_exceptions=True)
    assert pending.cancelled()
    assert overlapping.is_set()


async def test_move_event_when_received_then_destination_metadata_refreshes(tmp_path):
    meta = write_meta(tmp_path)
    catalog = MeetingCatalog(tmp_path, debounce_seconds=0)
    await catalog.start()
    try:
        write_meta(tmp_path, title="Changed")
        catalog.on_any_event(FileMovedEvent(str(meta) + ".tmp", str(meta)))
        await until(lambda: catalog.list_meetings()[0]["title"] == "Changed")
    finally:
        await catalog.stop()


async def test_lost_event_when_reconciliation_runs_then_new_and_deleted_meetings_are_seen(tmp_path):
    meta = write_meta(tmp_path)
    catalog = MeetingCatalog(tmp_path, reconcile_seconds=0.02, debounce_seconds=0)
    await catalog.start()
    try:
        meta.unlink()
        write_meta(tmp_path, meeting_id="new")
        await until(lambda: [row["id"] for row in catalog.list_meetings()] == ["new"])
    finally:
        await catalog.stop()


async def test_event_during_refresh_when_published_then_followup_refresh_is_not_lost(tmp_path, monkeypatch):
    meta = write_meta(tmp_path)
    catalog = MeetingCatalog(tmp_path, debounce_seconds=0)
    await catalog.start()
    started, release = threading.Event(), threading.Event()
    refresh = catalog._refresh_files
    first = True

    def paused_refresh(paths):
        nonlocal first
        result = refresh(paths)
        if first:
            first = False
            started.set()
            assert release.wait(2)
        return result

    monkeypatch.setattr(catalog, "_refresh_files", paused_refresh)
    try:
        write_meta(tmp_path, title="First")
        catalog.on_any_event(FileModifiedEvent(str(meta)))
        await until(started.is_set)
        write_meta(tmp_path, title="Second")
        catalog.on_any_event(FileModifiedEvent(str(meta)))
        await asyncio.sleep(0)  # deliver the queued invalidation while refresh is in flight
        release.set()
        await until(lambda: catalog.list_meetings()[0]["title"] == "Second")
    finally:
        release.set()
        await catalog.stop()


async def test_scan_error_when_directory_recovers_then_retry_does_not_wait_for_reconcile(tmp_path, monkeypatch):
    write_meta(tmp_path)
    catalog = MeetingCatalog(tmp_path, reconcile_seconds=60, debounce_seconds=0, retry_seconds=0.01)
    await catalog.start()
    scan = os.scandir

    def unavailable(path):
        raise PermissionError("temporarily unavailable")

    try:
        monkeypatch.setattr(os, "scandir", unavailable)
        await catalog.refresh()
        assert len(catalog.list_meetings()) == 1
        write_meta(tmp_path, meeting_id="new")
        monkeypatch.setattr(os, "scandir", scan)
        await until(lambda: len(catalog.list_meetings()) == 2)
    finally:
        await catalog.stop()
