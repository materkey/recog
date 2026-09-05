"""Profile HTTP and live-acknowledgement contract using only temporary data."""

import asyncio
import json
from pathlib import Path
import threading
from types import SimpleNamespace

import httpx
import pytest

from recog.user_profile import UserProfileStore, resolve_identity
from recog.web import server
from recog.web.user_profile import ProfileCoordinator


@pytest.fixture
def profile_api(tmp_path, monkeypatch):
    broadcasts = []

    async def broadcast(raw):
        broadcasts.append(json.loads(raw))

    store = UserProfileStore(tmp_path / "data")
    coordinator = ProfileCoordinator(store, "Legacy", broadcast)
    monkeypatch.setattr(server, "profile_coordinator", coordinator)
    # ASGITransport does not run lifespan, so no installed meeting services start.
    client = httpx.AsyncClient(transport=httpx.ASGITransport(app=server.app), base_url="http://test")
    return SimpleNamespace(store=store, coordinator=coordinator, client=client, broadcasts=broadcasts)


class FakeBridge:
    def __init__(self):
        self.sent = asyncio.Queue()

    async def send(self, raw):
        await self.sent.put(json.loads(raw))
        return True

    async def next_refresh(self):
        return await asyncio.wait_for(self.sent.get(), timeout=2)


async def connect(api):
    bridge = FakeBridge()
    await api.coordinator.connected(bridge.send)
    first = await bridge.next_refresh()
    assert first == {"type": "user_profile_refresh", "revision": api.store.read()["revision"]}
    return bridge


async def acknowledge(api, profile=None, override=None):
    profile = profile or api.store.read()
    await api.coordinator.handle_frame({
        "type": "user_profile_applied", "revision": profile["revision"],
        "identity": resolve_identity(profile, legacy_name="Legacy", override=override),
    })


async def test_missing_profile_when_get_then_legacy_suggested_without_persistence(profile_api):
    api = profile_api
    async with api.client:
        response = await api.client.get("/api/user-profile")
    assert response.status_code == 200
    body = response.json()
    assert body["suggested_name"] == "Legacy"
    assert body["profile"]["configured"] is False
    assert body["identity"]["local_user_name"] == "Legacy"
    assert body["application_state"] == "pending"
    assert not api.store.path.exists()


async def test_offline_bridge_when_put_then_saved_pending_and_broadcast_without_environment_writes(profile_api):
    api = profile_api
    # Detect accidental writes to either real environment file without changing it.
    root = Path(__file__).resolve().parents[1]
    paths = [root / ".env", root / "recog/web/.env"]
    before = {path: (path.read_bytes(), path.stat().st_mtime_ns) if path.exists() else None for path in paths}
    async with api.client:
        response = await api.client.put("/api/user-profile", json={"name": " Алексей ", "aliases": ["Лёша"]})
        second_browser = await api.client.get("/api/user-profile")
    assert response.status_code == 200
    body = response.json()
    assert body["profile"] == api.store.read()
    assert body["profile"]["name"] == "Алексей"
    assert body["profile"]["aliases"] == ["Лёша"]
    assert body["application_state"] == "pending"
    assert second_browser.json() == body
    assert api.broadcasts[-1] == {"type": "user_profile_status", **body}
    after = {path: (path.read_bytes(), path.stat().st_mtime_ns) if path.exists() else None for path in paths}
    assert before == after


async def test_connected_bridge_when_valid_ack_arrives_then_put_reports_applied_identity(profile_api):
    api = profile_api
    bridge = await connect(api)
    async with api.client:
        saving = asyncio.create_task(api.client.put("/api/user-profile", json={"name": "Alex", "aliases": ["Al"]}))
        refresh = await bridge.next_refresh()
        assert refresh["revision"] == api.store.read()["revision"]
        await acknowledge(api, override="CLI")
        response = await asyncio.wait_for(saving, timeout=2)
    body = response.json()
    assert response.status_code == 200
    assert body["application_state"] == "applied"
    assert body["identity"]["local_user_name"] == "CLI"
    assert body["identity"]["local_user_aliases"] == ["Alex", "CLI", "Al"]
    assert body["identity"]["local_user_profile_revision"] == body["profile"]["revision"]
    assert api.broadcasts[-1]["application_state"] == "applied"


async def test_overlapping_saves_when_older_ack_arrives_then_new_revision_stays_pending(profile_api):
    api = profile_api
    bridge = await connect(api)
    async with api.client:
        first_request = asyncio.create_task(api.client.put("/api/user-profile", json={"name": "First"}))
        await bridge.next_refresh()
        first = api.store.read()
        second_request = asyncio.create_task(api.client.put("/api/user-profile", json={"name": "Second"}))
        await bridge.next_refresh()
        second = api.store.read()
        assert first["revision"] != second["revision"]
        await acknowledge(api, first)
        pending = (await api.client.get("/api/user-profile")).json()
        assert pending["profile"] == second
        assert pending["application_state"] == "pending"
        await acknowledge(api, second)
        responses = await asyncio.wait_for(asyncio.gather(first_request, second_request), timeout=2)
    assert all(response.status_code == 200 for response in responses)
    assert responses[-1].json()["application_state"] == "applied"
    assert responses[-1].json()["identity"]["local_user_aliases"] == ["Second"]


async def test_applied_profile_when_reconnecting_then_authoritative_revision_replayed_and_old_ack_cleared(profile_api):
    api = profile_api
    original = api.store.write("Original", ["Old alias"])
    await connect(api)
    await acknowledge(api, original)
    assert (await api.coordinator.snapshot())["application_state"] == "applied"
    await api.coordinator.disconnected()
    assert (await api.coordinator.snapshot())["application_state"] == "pending"
    fresh = api.store.write("Fresh", [])
    await connect(api)
    pending = await api.coordinator.snapshot()
    assert pending["profile"] == fresh
    assert pending["application_state"] == "pending"
    assert pending["identity"]["local_user_aliases"] == ["Fresh"]
    await acknowledge(api, original)
    assert (await api.coordinator.snapshot())["application_state"] == "pending"
    await acknowledge(api, fresh)
    assert (await api.coordinator.snapshot())["application_state"] == "applied"


async def test_same_revision_when_reconnected_then_new_ack_required(profile_api):
    api = profile_api
    await connect(api)
    await acknowledge(api)
    assert (await api.coordinator.snapshot())["application_state"] == "applied"
    await connect(api)
    assert (await api.coordinator.snapshot())["application_state"] == "pending"


async def test_old_ack_read_in_flight_when_new_save_applied_then_new_identity_cannot_be_replaced(profile_api, monkeypatch):
    api = profile_api
    first = api.store.write("First", [])
    bridge = await connect(api)
    original_read = api.store.read
    captured = asyncio.Event()
    release = threading.Event()
    guard = threading.Lock()
    should_pause = True
    loop = asyncio.get_running_loop()

    def paused_first_read():
        nonlocal should_pause
        with guard:
            pause = should_pause
            should_pause = False
        result = original_read()
        if pause:
            loop.call_soon_threadsafe(captured.set)
            assert release.wait(timeout=3), "Test did not release the captured old profile read"
        return result

    monkeypatch.setattr(api.store, "read", paused_first_read)
    old_ack = asyncio.create_task(acknowledge(api, first))
    await asyncio.wait_for(captured.wait(), timeout=2)
    saving = asyncio.create_task(api.coordinator.save("Second", []))
    next_refresh = asyncio.create_task(bridge.next_refresh())
    try:
        try:
            # A lock-based repair may serialize the new save behind the old ack.
            await asyncio.wait_for(asyncio.shield(next_refresh), timeout=0.05)
        except asyncio.TimeoutError:
            release.set()
            await asyncio.wait_for(old_ack, timeout=2)
            await next_refresh
        await acknowledge(api)
        await asyncio.wait_for(saving, timeout=2)
    finally:
        release.set()
        await asyncio.wait_for(old_ack, timeout=2)
    snapshot = await api.coordinator.snapshot()
    assert snapshot["profile"]["name"] == "Second"
    assert snapshot["application_state"] == "applied"
    assert snapshot["identity"]["local_user_aliases"] == ["Second"]


async def test_old_connection_ack_read_in_flight_when_reconnected_then_fresh_ack_still_required(profile_api, monkeypatch):
    api = profile_api
    profile = api.store.write("Alex", [])
    await connect(api)
    original_read = api.store.read
    captured = asyncio.Event()
    release = threading.Event()
    guard = threading.Lock()
    should_pause = True
    loop = asyncio.get_running_loop()

    def paused_first_read():
        nonlocal should_pause
        with guard:
            pause = should_pause
            should_pause = False
        result = original_read()
        if pause:
            loop.call_soon_threadsafe(captured.set)
            assert release.wait(timeout=3), "Test did not release old connection acknowledgement"
        return result

    monkeypatch.setattr(api.store, "read", paused_first_read)
    old_ack = asyncio.create_task(acknowledge(api, profile))
    try:
        await asyncio.wait_for(captured.wait(), timeout=2)
        await api.coordinator.disconnected()
        await connect(api)
    finally:
        release.set()
        await asyncio.wait_for(old_ack, timeout=2)
    assert (await api.coordinator.snapshot())["application_state"] == "pending"
    await acknowledge(api, profile)
    assert (await api.coordinator.snapshot())["application_state"] == "applied"


async def test_saved_profile_when_deleted_then_tombstone_and_legacy_aliases_applied(profile_api):
    api = profile_api
    original = api.store.write("Alex", ["Al"])
    bridge = await connect(api)
    await acknowledge(api)
    async with api.client:
        deleting = asyncio.create_task(api.client.delete("/api/user-profile"))
        await bridge.next_refresh()
        tombstone = api.store.read()
        assert tombstone["revision"] != original["revision"]
        assert tombstone["name"] == "" and tombstone["aliases"] == []
        await acknowledge(api)
        response = await asyncio.wait_for(deleting, timeout=2)
    assert response.status_code == 200
    assert response.json()["profile"] == tombstone
    assert response.json()["profile"]["configured"] is False
    assert response.json()["application_state"] == "applied"
    assert response.json()["identity"]["local_user_aliases"] == ["Legacy"]


@pytest.mark.parametrize("payload", [
    {}, {"name": ""}, {"name": "Me"}, {"name": "Alex\n"}, {"name": "a" * 81},
    {"name": None}, {"name": 123}, {"name": "Alex", "aliases": "Al"},
    {"name": "Alex", "aliases": [None]}, {"name": "Alex", "aliases": ["a"] * 17},
])
async def test_invalid_input_when_put_then_422_and_saved_profile_unchanged(profile_api, payload):
    api = profile_api
    original = api.store.write("Original", [])
    async with api.client:
        response = await api.client.put("/api/user-profile", json=payload)
    assert response.status_code == 422
    assert api.store.read() == original
    assert api.broadcasts == []


async def test_write_failure_when_put_then_500_without_success_broadcast(profile_api, monkeypatch):
    api = profile_api
    original = api.store.write("Original", [])

    def fail_write(*args):
        raise PermissionError("private filesystem location")

    monkeypatch.setattr(api.store, "write", fail_write)
    async with api.client:
        response = await api.client.put("/api/user-profile", json={"name": "Alex"})
    assert response.status_code == 500
    assert "private filesystem" not in response.text
    assert api.store.read() == original
    assert api.broadcasts == []


async def test_corrupt_file_when_get_then_visible_error_without_mutation(profile_api):
    api = profile_api
    api.store.path.parent.mkdir()
    api.store.path.write_bytes(b"broken")
    async with api.client:
        response = await api.client.get("/api/user-profile")
    assert response.status_code == 500
    assert api.store.path.read_bytes() == b"broken"


@pytest.mark.parametrize("field,value", [
    ("local_user_name", None), ("local_user_aliases", "Alex"),
    ("local_user_aliases", [123]), ("local_user_identity_known", 1),
    ("local_user_profile_revision", "stale"),
])
async def test_malformed_ack_when_received_then_cannot_mark_applied(profile_api, field, value):
    api = profile_api
    profile = api.store.write("Alex", [])
    await connect(api)
    identity = resolve_identity(profile)
    identity[field] = value
    await api.coordinator.handle_frame({"type": "user_profile_applied", "revision": profile["revision"], "identity": identity})
    assert (await api.coordinator.snapshot())["application_state"] == "pending"


async def test_bridge_apply_error_when_save_waits_then_pending_error_without_losing_saved_profile(profile_api):
    api = profile_api
    bridge = await connect(api)
    async with api.client:
        saving = asyncio.create_task(api.client.put("/api/user-profile", json={"name": "Alex"}))
        await bridge.next_refresh()
        await api.coordinator.handle_frame({"type": "user_profile_error", "message": "private runtime details"})
        response = await asyncio.wait_for(saving, timeout=2)
    assert response.status_code == 200
    body = response.json()
    assert body["profile"] == api.store.read()
    assert body["application_state"] == "pending"
    assert body["error"]
    assert "private runtime" not in json.dumps(body)


def test_corrupt_profile_when_checking_readiness_then_existing_services_are_not_activated(tmp_path):
    import os
    from pathlib import Path
    import subprocess
    import sys

    root = Path(__file__).resolve().parent.parent
    (tmp_path / "user-profile.json").write_text("broken JSON")
    result = subprocess.run(
        ["bash", "-c", 'source "$1"; deps_asr_config_check', "--", str(root / "scripts/deps.sh")],
        env={**os.environ, "DEPS_PYTHON": sys.executable, "RECOG_DATA_DIR": str(tmp_path),
             "RECOG_ASR_BACKEND": "local"}, capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "Invalid user profile" in result.stdout
