"""Tests for ktalk_bridge.py."""
import asyncio
import base64
import json
import pytest
from pathlib import Path
from unittest.mock import Mock, AsyncMock

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from ktalk_bridge import KTalkBridge, Participant, PcmChunk


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


# --- PCM transport: pcm_chunk parsing + handshake ---


def _make_pcm_chunk_message(
    *,
    pid: str = "participant-1",
    speaker_name: str = "Alice",
    is_local: bool = False,
    seq: int = 1,
    chunk_start_ms: float = 1000.0,
    chunk_end_ms: float = 4300.0,
    sample_rate: int = 16000,
    pcm_bytes: bytes = b"\x00\x01\x02\x03",
) -> dict:
    return {
        "type": "pcm_chunk",
        "pid": pid,
        "speaker_name": speaker_name,
        "is_local": is_local,
        "seq": seq,
        "chunk_start_ms": chunk_start_ms,
        "chunk_end_ms": chunk_end_ms,
        "sample_rate": sample_rate,
        "pcm_base64": base64.b64encode(pcm_bytes).decode("ascii"),
    }


@pytest.mark.asyncio
async def test_pcm_chunk_parsing_dispatches_to_callback():
    """
    given: bridge with registered pcm_chunk callback
    when: _process_message receives a well-formed pcm_chunk frame
    then: callback is invoked once with a PcmChunk carrying decoded PCM and
          timestamps corrected by clock_offset
    """
    bridge = KTalkBridge()
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    # Emulate clock_sync — bridge adds offset to all timestamps from JS.
    bridge._clock_offset = 10.0

    pcm_bytes = (1234).to_bytes(2, "little", signed=True) * 8  # 8 samples Int16LE
    message = _make_pcm_chunk_message(
        pid="pid-42",
        speaker_name="Bob",
        is_local=False,
        seq=7,
        chunk_start_ms=2_000.0,
        chunk_end_ms=5_300.0,
        sample_rate=16000,
        pcm_bytes=pcm_bytes,
    )

    await bridge._process_message(message)

    assert len(received) == 1
    chunk = received[0]
    assert isinstance(chunk, PcmChunk)
    assert chunk.pid == "pid-42"
    assert chunk.speaker_name == "Bob"
    assert chunk.is_local is False
    assert chunk.seq == 7
    assert chunk.sample_rate == 16000
    assert chunk.pcm == pcm_bytes
    # chunk_start_ms (2000) / 1000 = 2.0 s, + clock_offset 10 s = 12.0 s
    assert chunk.chunk_start_ts == pytest.approx(12.0)
    assert chunk.chunk_end_ts == pytest.approx(15.3)


@pytest.mark.asyncio
async def test_pcm_chunk_no_callback_does_not_raise():
    """
    given: bridge without pcm_chunk callback registered
    when: _process_message receives a pcm_chunk frame
    then: no exception is raised (chunk is silently dropped)
    """
    bridge = KTalkBridge()
    assert bridge._on_pcm_chunk is None

    message = _make_pcm_chunk_message()
    await bridge._process_message(message)  # must not raise


@pytest.mark.asyncio
async def test_pcm_chunk_malformed_payload_logged_not_raised(capsys):
    """
    given: bridge with pcm_chunk callback registered
    when: _process_message receives a frame with invalid base64 PCM or
          missing required fields
    then: bridge logs a warning and does NOT call the callback; no exception
    """
    bridge = KTalkBridge()
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    # Case 1: invalid base64 — non-alphabet character inside pcm_base64.
    bad_b64_message = _make_pcm_chunk_message()
    bad_b64_message["pcm_base64"] = "!!not-base64!!"
    await bridge._process_message(bad_b64_message)
    assert received == []

    # Case 2: missing required field (`seq`).
    missing_field_message = _make_pcm_chunk_message()
    del missing_field_message["seq"]
    await bridge._process_message(missing_field_message)
    assert received == []

    # Case 3: malformed type (non-int seq that can't be coerced).
    bad_type_message = _make_pcm_chunk_message()
    bad_type_message["seq"] = "not-a-number"
    await bridge._process_message(bad_type_message)
    assert received == []

    # All three errors should have produced stderr log lines.
    err_out = capsys.readouterr().err
    assert "pcm_chunk" in err_out


@pytest.mark.asyncio
async def test_pcm_chunk_coerces_non_string_pid_and_speaker(capsys):
    """
    given: bridge with pcm_chunk callback registered
    when: _process_message receives a frame whose pid is numeric (malformed
          or future-protocol) and speaker_name is None-shaped
    then: bridge coerces pid to str so downstream slicing (`pid[:6]`) and
          name sanitization (`.replace(...)`) cannot crash the batch
          callback and drop every sibling chunk along with the bad one
    """
    bridge = KTalkBridge()
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    int_pid_message = _make_pcm_chunk_message()
    int_pid_message["pid"] = 42  # non-string
    int_pid_message["speaker_name"] = None
    await bridge._process_message(int_pid_message)

    assert len(received) == 1
    chunk = received[0]
    assert isinstance(chunk.pid, str)
    assert chunk.pid == "42"
    assert isinstance(chunk.speaker_name, str)
    assert chunk.speaker_name == ""

    # Empty pid still rejected.
    empty_pid_message = _make_pcm_chunk_message()
    empty_pid_message["pid"] = ""
    await bridge._process_message(empty_pid_message)
    assert len(received) == 1  # unchanged
    err_out = capsys.readouterr().err
    assert "pcm_chunk" in err_out


@pytest.mark.asyncio
async def test_pcm_chunk_odd_length_payload_rejected_at_ingress(capsys):
    """
    given: bridge with pcm_chunk callback registered
    when: _process_message receives a frame whose decoded PCM length is odd
    then: the chunk is skipped with a warning; callback is not invoked so a
          single malformed chunk cannot crash the downstream Int16 decode and
          take a pending batch down with it
    """
    bridge = KTalkBridge()
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    # 7 bytes = 3.5 Int16 samples — cannot be reinterpreted as int16 array.
    odd_message = _make_pcm_chunk_message(pcm_bytes=b"\x01\x02\x03\x04\x05\x06\x07")
    await bridge._process_message(odd_message)

    assert received == []
    err_out = capsys.readouterr().err
    assert "odd-length" in err_out


@pytest.mark.asyncio
async def test_pcm_chunk_empty_payload_rejected_at_ingress(capsys):
    """
    given: bridge with pcm_chunk callback registered
    when: _process_message receives a frame whose decoded PCM is empty
    then: the chunk is skipped with a warning; callback is not invoked so a
          single empty chunk cannot crash GigaAM's mel preprocessor in
          transcribe_batch and take the whole batch down with it
    """
    bridge = KTalkBridge()
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    empty_message = _make_pcm_chunk_message(pcm_bytes=b"")
    await bridge._process_message(empty_message)

    assert received == []
    err_out = capsys.readouterr().err
    assert "empty payload" in err_out


@pytest.mark.asyncio
async def test_pcm_chunk_unsupported_sample_rate_rejected(capsys):
    """
    given: bridge with pcm_chunk callback registered
    when: _process_message receives a frame whose sample_rate is not 16000
    then: the chunk is skipped with a warning — GigaAM assumes 16 kHz; any
          other rate would produce silent garbage downstream with no error
    """
    bridge = KTalkBridge()
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    bad_sr_message = _make_pcm_chunk_message(sample_rate=48000)
    await bridge._process_message(bad_sr_message)

    assert received == []
    err_out = capsys.readouterr().err
    assert "sample_rate" in err_out


@pytest.mark.asyncio
async def test_pcm_chunk_is_local_forced_false_at_ingress():
    """
    given: bridge with pcm_chunk callback registered
    when: a frame claims is_local=True (forged or buggy; D9 says PCM transport
          is for remote participants only — extension skips local mic)
    then: bridge ignores the field and stamps is_local=False on the dispatched
          chunk so a malformed frame cannot tag a segment as local
    """
    bridge = KTalkBridge()
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    forged_message = _make_pcm_chunk_message(is_local=True)
    await bridge._process_message(forged_message)

    assert len(received) == 1
    assert received[0].is_local is False


@pytest.mark.asyncio
async def test_handshake_sends_pcm_transport_enabled_on_connect(monkeypatch):
    """
    given: RECOG_PCM_TRANSPORT=1 in env (captured at bridge init)
    when: a client connects and the bridge handles the connection
    then: the very first WS message sent to the client is
          {type: "pcm_transport_enabled", enabled: true}
    """
    monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
    bridge = KTalkBridge()
    # _pcm_transport_enabled snapshot happens in __init__; verify:
    assert bridge._pcm_transport_enabled is True

    sent: list[str] = []

    class RecordingWebSocket(FakeWebSocket):
        async def send(self, message: str):
            sent.append(message)

    ws = RecordingWebSocket()
    await bridge._handle_client(ws)

    assert sent, "handshake frame must be sent on connect"
    first = json.loads(sent[0])
    assert first == {"type": "pcm_transport_enabled", "enabled": True}


@pytest.mark.asyncio
async def test_handshake_disabled_when_env_unset(monkeypatch):
    """
    given: RECOG_PCM_TRANSPORT is unset (default disabled)
    when: a client connects
    then: handshake sent with enabled: false — extension must not send PCM
    """
    monkeypatch.delenv("RECOG_PCM_TRANSPORT", raising=False)
    bridge = KTalkBridge()
    assert bridge._pcm_transport_enabled is False

    sent: list[str] = []

    class RecordingWebSocket(FakeWebSocket):
        async def send(self, message: str):
            sent.append(message)

    ws = RecordingWebSocket()
    await bridge._handle_client(ws)

    assert sent, "handshake frame must be sent on connect"
    first = json.loads(sent[0])
    assert first == {"type": "pcm_transport_enabled", "enabled": False}


@pytest.mark.asyncio
async def test_pcm_chunk_callback_exception_does_not_break_bridge(capsys):
    """
    given: callback that raises on first call
    when: _process_message receives a pcm_chunk
    then: exception is caught and logged; subsequent messages still work
    """
    bridge = KTalkBridge()

    def boom(_chunk: PcmChunk):
        raise RuntimeError("callback intentionally failed")

    bridge.set_pcm_chunk_callback(boom)

    # Should not raise out of _process_message.
    await bridge._process_message(_make_pcm_chunk_message())

    err_out = capsys.readouterr().err
    assert "callback raised" in err_out

    # Bridge must remain operational — swap callback for a recorder and retry.
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)
    await bridge._process_message(_make_pcm_chunk_message(seq=2))
    assert len(received) == 1
    assert received[0].seq == 2
