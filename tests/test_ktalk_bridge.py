"""Tests for ktalk_bridge.py."""
import asyncio
import pytest
from pathlib import Path
from unittest.mock import Mock, AsyncMock

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from ktalk_bridge import KTalkBridge, Participant


class FakeWebSocket:
    """Fake WebSocket that disconnects immediately after iteration."""

    def __init__(self):
        self.remote_address = ('127.0.0.1', 12345)
        self._messages = []
        self._closed = False

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self._closed or not self._messages:
            raise StopAsyncIteration
        return self._messages.pop(0)

    def add_message(self, msg: str):
        self._messages.append(msg)

    def close(self):
        self._closed = True


@pytest.mark.asyncio
async def test_ktalk_meeting_ends_when_client_disconnects():
    """
    given: KTalkBridge with active ktalk meeting and one connected client
    when: client WebSocket disconnects
    then: on_ktalk_leave("disconnected") should be called
    """
    # Arrange
    bridge = KTalkBridge()

    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    mock_storage.on_ktalk_leave = Mock(return_value="new_meeting_id")

    bridge.set_meeting_storage(mock_storage)

    fake_ws = FakeWebSocket()

    # Act - simulate client connection then immediate disconnect
    await bridge._handle_client(fake_ws)

    # Assert
    mock_storage.on_ktalk_leave.assert_called_once_with("disconnected")


@pytest.mark.asyncio
async def test_orphan_meeting_not_ended_on_disconnect():
    """
    given: KTalkBridge with active orphan meeting (not ktalk) and one client
    when: client WebSocket disconnects
    then: on_ktalk_leave should NOT be called (orphan meetings continue)
    """
    # Arrange
    bridge = KTalkBridge()

    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "orphan"})
    mock_storage.on_ktalk_leave = Mock()

    bridge.set_meeting_storage(mock_storage)

    fake_ws = FakeWebSocket()

    # Act
    await bridge._handle_client(fake_ws)

    # Assert
    mock_storage.on_ktalk_leave.assert_not_called()


@pytest.mark.asyncio
async def test_meeting_not_ended_if_other_clients_connected():
    """
    given: KTalkBridge with active ktalk meeting and two connected clients
    when: one client WebSocket disconnects
    then: on_ktalk_leave should NOT be called (other clients still connected)
    """
    # Arrange
    bridge = KTalkBridge()

    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    mock_storage.on_ktalk_leave = Mock()

    bridge.set_meeting_storage(mock_storage)

    # Simulate another client already connected
    other_client = FakeWebSocket()
    bridge._connected_clients.add(other_client)

    fake_ws = FakeWebSocket()

    # Act
    await bridge._handle_client(fake_ws)

    # Assert - meeting should NOT end because other_client is still connected
    mock_storage.on_ktalk_leave.assert_not_called()

    # Cleanup
    bridge._connected_clients.discard(other_client)


# --- get_sole_unmuted_remote_participant tests ---

def test_sole_unmuted_remote_participant_one_unmuted_returns_name():
    """
    given: 3 remote participants, exactly 1 with mic unmuted
    when: get_sole_unmuted_remote_participant called
    then: returns that participant's name
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=True),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=True),
    }
    assert bridge.get_sole_unmuted_remote_participant() == "Bob"


def test_sole_unmuted_remote_participant_zero_unmuted_returns_none():
    """
    given: 2 remote participants, all muted
    when: get_sole_unmuted_remote_participant called
    then: returns None
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=True),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=True),
    }
    assert bridge.get_sole_unmuted_remote_participant() is None


def test_sole_unmuted_remote_participant_two_unmuted_returns_none():
    """
    given: 3 remote participants, 2 with mic unmuted
    when: get_sole_unmuted_remote_participant called
    then: returns None (ambiguous - can't determine who is speaking)
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=False),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=True),
    }
    assert bridge.get_sole_unmuted_remote_participant() is None


def test_sole_unmuted_remote_participant_local_excluded():
    """
    given: 1 local participant unmuted + 0 remote unmuted
    when: get_sole_unmuted_remote_participant called
    then: returns None (local participant is excluded)
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Me", is_local=True, mic_muted=False),
        "p2": Participant(id="p2", name="Alice", is_local=False, mic_muted=True),
    }
    assert bridge.get_sole_unmuted_remote_participant() is None
