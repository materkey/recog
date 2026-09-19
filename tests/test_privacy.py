"""Privacy preference and all three MCP delivery paths, using temporary data."""

import copy
import json

import httpx
import pytest

from recog import copilot_channel as channel
from recog.privacy import CopilotPrivacy, PrivacyStore
from recog.web import server


def event():
    return {
        "type": "transcript_tick", "meeting_id": "meeting-1",
        "local_user_name": "Alice", "local_user_aliases": ["Al"],
        "local_user_identity_known": True, "local_user_profile_revision": "rev-1",
        "recent_context": "Alice: First\nBob: Second",
        "recent_segments": [{"speaker": "Alice", "text": "First"}, {"speaker": "Bob", "text": "Second"}],
        "new_segments": [{"speaker": "Bob", "speaker_id": "bob@example.test", "text": "Second"}],
    }


def parse(result):
    return json.loads(result[0].text)


def test_missing_setting_when_read_then_default_without_creating_files(tmp_path):
    store = PrivacyStore(tmp_path)
    assert store.read() == {"enabled": False, "configured": False}
    assert list(tmp_path.iterdir()) == []


def test_saved_setting_when_reopened_then_persists_with_private_permissions(tmp_path):
    store = PrivacyStore(tmp_path)
    store.write(True)
    assert PrivacyStore(tmp_path).read() == {"enabled": True, "configured": True}
    assert store.path.stat().st_mode & 0o777 == 0o600


def test_privacy_enabled_when_delivering_then_masks_names_without_mutating_transcript(tmp_path):
    store = PrivacyStore(tmp_path)
    privacy = CopilotPrivacy(store)
    payload = event()
    original = copy.deepcopy(payload)
    assert privacy.apply(payload) == original
    store.write(True)
    masked = privacy.apply(payload)
    assert masked["recent_context"] == "speaker_0: First\nspeaker_1: Second"
    assert masked["new_segments"][0]["speaker_id"] == "speaker_1"
    assert masked["local_user_name"] == "speaker_0"
    assert masked["local_user_aliases"] == []
    assert masked["local_user_identity_known"] is False
    assert payload == original
    store.write(False)
    assert privacy.apply(payload) == original


def test_corrupt_setting_when_delivering_then_refuses_to_expose_names(tmp_path):
    store = PrivacyStore(tmp_path)
    store.path.write_text("broken")
    with pytest.raises(ValueError):
        CopilotPrivacy(store).apply(event())


@pytest.mark.asyncio
async def test_queued_before_privacy_when_polled_then_masks_queued_event():
    link = channel.BridgeLink()
    await channel._handle_inbound(json.dumps(event()), link)
    link.privacy.store.write(True)
    mcp = channel._build_mcp(link, "127.0.0.1", 0)
    result = parse(await mcp.call_tool("await_copilot_event", {"timeout_seconds": 1}))
    assert result["event"]["new_segments"][0]["speaker"] == "speaker_1"
    assert "Alice" not in json.dumps(result)
    assert "bob@example.test" not in json.dumps(result)
    assert "Privacy mode" in result["user_context"]


@pytest.mark.asyncio
async def test_privacy_enabled_when_pushing_then_masks_content_and_metadata(monkeypatch):
    link = channel.BridgeLink()
    link.privacy.store.write(True)
    delivered = []

    async def broadcast(mcp, content, meta):
        delivered.append({"content": content, "meta": meta})
        return 1

    monkeypatch.setattr(channel, "_broadcast_channel_notification", broadcast)
    await channel._handle_inbound(json.dumps(event()), link, object())
    assert "Alice" not in json.dumps(delivered)
    assert "Bob" not in json.dumps(delivered)
    assert delivered[0]["meta"]["local_user_name"] == "speaker_0"
    assert "speaker_1: Second" in delivered[0]["content"]


@pytest.mark.asyncio
async def test_privacy_enabled_when_snapshot_then_matches_push_aliases():
    link = channel.BridgeLink()
    link.privacy.store.write(True)
    link.privacy.apply(event())

    class Socket:
        async def send(self, raw):
            request = json.loads(raw)
            link.resolve_pending(request["request_id"], {
                "segments": [{"meeting_id": "meeting-1", "speaker": "Bob", "text": "More"}],
                "local_user_name": "Alice", "local_user_aliases": ["Al"],
            })

    link.set_socket(Socket())
    mcp = channel._build_mcp(link, "127.0.0.1", 0)
    result = parse(await mcp.call_tool("get_recent_transcript", {}))
    assert result["segments"][0]["speaker"] == "speaker_1"
    assert result["local_user_name"] == "speaker_0"
    assert "Alice" not in json.dumps(result)
    assert "Bob" not in json.dumps(result)


@pytest.mark.asyncio
async def test_browser_toggle_when_saved_then_channel_observes_same_setting(tmp_path, monkeypatch):
    store = PrivacyStore(tmp_path)
    monkeypatch.setattr(server, "privacy_store", store)
    broadcasts = []

    async def broadcast(raw):
        broadcasts.append(json.loads(raw))

    monkeypatch.setattr(server, "_forward_bridge_message", broadcast)
    privacy = CopilotPrivacy(PrivacyStore(tmp_path))
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=server.app), base_url="http://test") as client:
        assert (await client.get("/api/privacy")).json()["configured"] is False
        invalid = await client.put("/api/privacy", json={"enabled": "false"})
        assert invalid.status_code == 422
        assert not store.path.exists()
        assert (await client.put("/api/privacy", json={"enabled": True})).status_code == 200
        assert privacy.apply(event())["local_user_name"] == "speaker_0"
        assert (await client.get("/api/privacy")).json()["enabled"] is True
        assert broadcasts[-1] == {"type": "privacy_updated", "enabled": True, "configured": True}
        await client.put("/api/privacy", json={"enabled": False})
        assert privacy.apply(event())["local_user_name"] == "Alice"
