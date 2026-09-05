"""The model gets one literal profile representation on every MCP surface."""

import json

import pytest

from recog.copilot_channel import BridgeLink, _build_mcp, _format_channel_event


IDENTITY = {
    "local_user_name": "Имя {{GOAL}}",
    "local_user_aliases": ['Имя {{GOAL}}', 'Alias "quoted"'],
    "local_user_profile_revision": "rev-1",
    "local_user_identity_known": True,
}


def profile_data(context):
    return json.loads(context.split("\n", 1)[1])


@pytest.mark.parametrize("kind", ["transcript_tick", "meeting_started", "copilot_user_message", "user_profile_updated"])
def test_identity_when_formatting_push_then_literal_profile_precedes_event(kind):
    content, meta = _format_channel_event({"type": kind, **IDENTITY})
    context = content.split("\n\n", 1)[0]
    assert profile_data(context) == {
        "name": IDENTITY["local_user_name"],
        "aliases": IDENTITY["local_user_aliases"],
        "revision": "rev-1", "known": True,
    }
    # Existing clients may still use transport metadata during upgrade.
    assert json.loads(meta["local_user_aliases"]) == IDENTITY["local_user_aliases"]


def test_unknown_identity_when_formatting_then_empty_aliases_and_false_survive():
    content, _ = _format_channel_event({
        "type": "user_profile_updated", **IDENTITY,
        "local_user_name": "Me", "local_user_aliases": [], "local_user_identity_known": False,
    })
    data = profile_data(content.split("\n\n", 1)[0])
    assert data["aliases"] == []
    assert data["known"] is False


def test_legacy_event_when_formatting_then_no_profile_values_are_invented():
    content, _ = _format_channel_event({"type": "copilot_user_message", "text": "hi"})
    assert content.startswith("User typed")
    content, _ = _format_channel_event({"type": "transcript_tick", "local_user_name": "Legacy"})
    assert profile_data(content.split("\n\n", 1)[0]) == {"name": "Legacy"}


def tool_payload(result):
    blocks = result[0] if isinstance(result, tuple) else result
    return json.loads(next(block.text for block in blocks if hasattr(block, "text")))


async def test_profile_event_when_long_polled_then_raw_event_and_same_context_are_returned():
    link = BridgeLink()
    event = {"type": "user_profile_updated", **IDENTITY}
    await link.event_queue.put(event)
    mcp = _build_mcp(link, "127.0.0.1", 0)
    payload = tool_payload(await mcp.call_tool("await_copilot_event", {"timeout_seconds": 1}))
    assert payload["event"] == event
    push, _ = _format_channel_event(event)
    assert payload["user_context"] == push.split("\n\n", 1)[0]


async def test_transcript_when_returned_then_context_uses_the_same_profile_renderer():
    link = BridgeLink()
    link.is_connected = lambda: True

    async def send(frame):
        link.resolve_pending(frame["request_id"], {"segments": [], **IDENTITY})
        return {"ok": True}

    link.send = send
    mcp = _build_mcp(link, "127.0.0.1", 0)
    payload = tool_payload(await mcp.call_tool("get_recent_transcript", {}))
    push, _ = _format_channel_event({"type": "transcript_tick", **IDENTITY})
    assert payload["user_context"] == push.split("\n\n", 1)[0]
    assert payload["local_user_aliases"] == IDENTITY["local_user_aliases"]
