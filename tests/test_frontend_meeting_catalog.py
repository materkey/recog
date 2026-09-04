"""Meeting-list reads must not rescan the archive after startup."""

import json
import asyncio
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest
from starlette.testclient import TestClient

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "frontend"))
import server


@pytest.fixture(autouse=True)
def isolated_server_state(monkeypatch):
    monkeypatch.setattr(server, "observer", None)
    monkeypatch.setattr(server, "bridge_subscriber", None)
    monkeypatch.setattr(server, "meeting_catalog", None)
    monkeypatch.setattr(server, "live_watcher", server.LiveWatcher())
    monkeypatch.setattr(server, "transcript_watcher", server.TranscriptWatcher())


@pytest.fixture
def client(tmp_path, monkeypatch):
    (tmp_path / "index.json").write_text(json.dumps({"meetings": [{
        "id": "archived", "created_at": "2026-09-01T10:00:00+00:00",
        "ended_at": "2026-09-01T11:00:00+00:00", "title": "Archived",
    }]}))

    class Subscriber:
        def __init__(self, **kwargs):
            pass

        async def start(self):
            pass

        async def stop(self):
            pass

    monkeypatch.setattr(server, "MEETINGS_DIR", tmp_path)
    monkeypatch.setattr(server, "get_external_paths", lambda: [])
    monkeypatch.setattr(server, "BridgeSubscriber", Subscriber)

    with TestClient(server.app) as client:
        yield client


def test_warm_catalog_when_listing_then_no_storage_scan(client, monkeypatch):
    assert client.get("/api/meetings").status_code == 200

    def unexpected_scan(*args, **kwargs):
        raise RuntimeError("HTTP list request rescanned storage")

    monkeypatch.setattr(server.MeetingStorage, "list_meetings", unexpected_scan)
    response = client.get("/api/meetings")
    assert response.status_code == 200
    assert [row["id"] for row in response.json()] == ["archived"]


def test_blocked_refresh_when_http_lists_then_previous_snapshot_is_available(client, monkeypatch):
    catalog = server.meeting_catalog
    refresh = catalog._refresh_files
    started, release = threading.Event(), threading.Event()

    def blocked(paths):
        started.set()
        assert release.wait(3)
        return refresh(paths)

    monkeypatch.setattr(catalog, "_refresh_files", blocked)
    pending = asyncio.run_coroutine_threadsafe(catalog.refresh(), catalog._loop)
    try:
        assert started.wait(1)
        with ThreadPoolExecutor() as executor:
            response = executor.submit(client.get, "/api/meetings").result(timeout=1)
        assert response.status_code == 200
        assert response.json()[0]["id"] == "archived"
    finally:
        release.set()
        pending.result(timeout=3)


def test_slow_live_connection_when_http_lists_then_disk_read_does_not_block_server(client, monkeypatch):
    started, release = threading.Event(), threading.Event()

    class SlowStorage:
        def list_active_meetings(self):
            started.set()
            assert release.wait(3)
            return []

    monkeypatch.setattr(server, "get_storage", SlowStorage)

    def connect_live():
        with client.websocket_connect("/api/live/stream") as ws:
            return json.loads(ws.receive_text())

    with ThreadPoolExecutor(max_workers=2) as executor:
        live = executor.submit(connect_live)
        try:
            assert started.wait(1)
            response = executor.submit(client.get, "/api/meetings").result(timeout=0.5)
            assert response.status_code == 200
        finally:
            release.set()
        assert live.result(timeout=2)["type"] == "init"


async def test_overlapping_live_selections_when_applied_then_older_scan_cannot_win():
    watcher = server.LiveWatcher()
    started, release = threading.Event(), threading.Event()
    calls = []
    applied = []

    class Storage:
        def list_active_meetings(self):
            calls.append(len(calls))
            index = len(calls)
            if index == 1:
                started.set()
                assert release.wait(2)
            return [{"id": str(index)}]

    async def select():
        rows = await watcher.read_active_meetings(Storage())
        applied.append(rows[0]["id"])

    first = asyncio.create_task(select())
    second = None
    try:
        async with asyncio.timeout(1):
            while not started.is_set():
                await asyncio.sleep(0.005)
        second = asyncio.create_task(select())
        await asyncio.sleep(0.02)
        assert len(calls) == 1
    finally:
        release.set()
        await asyncio.gather(*[task for task in (first, second) if task])
    assert applied == ["1", "2"]


def test_deleted_meeting_when_delete_returns_then_list_is_already_refreshed(client, tmp_path):
    storage = server.MeetingStorage(tmp_path)
    storage.start_session(ktalk=True)
    meeting_id = storage.current_meeting_id
    storage.append_segment(speaker="Alice", speaker_id="alice", text="Hello")
    storage.end_session()
    asyncio.run_coroutine_threadsafe(server.meeting_catalog.refresh(), server.meeting_catalog._loop).result(2)
    assert meeting_id in [row["id"] for row in client.get("/api/meetings").json()]
    assert client.delete(f"/api/meetings/{meeting_id}").status_code == 200
    assert meeting_id not in [row["id"] for row in client.get("/api/meetings").json()]


def test_title_generated_when_post_returns_then_list_has_new_title(client, tmp_path, monkeypatch):
    import recog.title_generator

    async def generated(transcript):
        return "New title"

    monkeypatch.setattr(recog.title_generator, "generate_title_async", generated)
    storage = server.MeetingStorage(tmp_path)
    storage.start_session(ktalk=True)
    meeting_id = storage.current_meeting_id
    storage.append_segment(speaker="Alice", speaker_id="alice", text="Hello")
    storage.end_session()
    response = client.post(f"/api/meetings/{meeting_id}/generate-title")
    assert response.status_code == 200
    row = next(row for row in client.get("/api/meetings").json() if row["id"] == meeting_id)
    assert row["title"] == "New title"


def test_external_watch_unavailable_when_starting_then_catalog_uses_reconciliation(tmp_path, monkeypatch):
    external = tmp_path / "external"
    external.mkdir()
    observer_class = server.Observer

    class UnavailableExternalObserver(observer_class):
        def schedule(self, handler, path, **kwargs):
            if Path(path) == external:
                raise OSError("external watch unavailable")
            return super().schedule(handler, path, **kwargs)

    class Subscriber:
        def __init__(self, **kwargs):
            pass

        async def start(self):
            pass

        async def stop(self):
            pass

    monkeypatch.setattr(server, "Observer", UnavailableExternalObserver)
    monkeypatch.setattr(server, "MEETINGS_DIR", tmp_path / "local")
    monkeypatch.setattr(server, "get_external_paths", lambda: [(external, "external")])
    monkeypatch.setattr(server, "BridgeSubscriber", Subscriber)
    with TestClient(server.app) as client:
        assert client.get("/api/meetings").status_code == 200
