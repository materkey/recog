"""Tests for ktalk_bridge.py."""
import asyncio
import base64
import collections
import json
import time
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


def _attach_meeting_storage(bridge: KTalkBridge, source: str = "ktalk") -> Mock:
    storage = Mock()
    storage.current_meeting_path = Path("/tmp/recog-test-meeting")
    storage._load_meta = Mock(return_value={"source": source})
    bridge.set_meeting_storage(storage)
    return storage


@pytest.mark.asyncio
async def test_pcm_chunk_parsing_dispatches_to_callback():
    """
    given: bridge with registered pcm_chunk callback
    when: _process_message receives a well-formed pcm_chunk frame
    then: callback is invoked once with a PcmChunk carrying decoded PCM and
          timestamps corrected by clock_offset
    """
    bridge = KTalkBridge()
    _attach_meeting_storage(bridge)
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
    _attach_meeting_storage(bridge)
    assert bridge._on_pcm_chunk is None

    message = _make_pcm_chunk_message()
    await bridge._process_message(message)  # must not raise


@pytest.mark.asyncio
async def test_pcm_transport_stats_updates_debug_state(tmp_path):
    """
    given: bridge receiving browser-side PCM/WebRTC transport diagnostics
    when: _process_message receives pcm_transport_stats
    then: stats are kept separately from PerSpeakerIngest pcm stats and written
          to the debug file for /api/pcm-diagnostics
    """
    bridge = KTalkBridge()
    bridge._debug_file = tmp_path / "ktalk_debug.json"

    await bridge._process_message({
        "type": "pcm_transport_stats",
        "stats": {
            "active_count": 1,
            "total_rebinds": 2,
            "per_pid": {"pid-a": {"bound_via": "receiver"}},
        },
    })

    assert bridge._pcm_transport_stats["active_count"] == 1
    assert bridge._pcm_transport_stats["total_rebinds"] == 2
    assert "received_at" in bridge._pcm_transport_stats

    state = json.loads(bridge._debug_file.read_text())
    assert state["pcm_transport"]["active_count"] == 1
    assert state["pcm_transport"]["per_pid"]["pid-a"]["bound_via"] == "receiver"


@pytest.mark.asyncio
async def test_pcm_chunk_malformed_payload_logged_not_raised(capsys):
    """
    given: bridge with pcm_chunk callback registered
    when: _process_message receives a frame with invalid base64 PCM or
          missing required fields
    then: bridge logs a warning and does NOT call the callback; no exception
    """
    bridge = KTalkBridge()
    _attach_meeting_storage(bridge)
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
    _attach_meeting_storage(bridge)
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
    _attach_meeting_storage(bridge)
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
    _attach_meeting_storage(bridge)
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
    _attach_meeting_storage(bridge)
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
    _attach_meeting_storage(bridge)
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    forged_message = _make_pcm_chunk_message(is_local=True)
    await bridge._process_message(forged_message)

    assert len(received) == 1
    assert received[0].is_local is False


@pytest.mark.asyncio
async def test_pcm_chunk_without_meeting_storage_is_dropped(capsys):
    """
    given: bridge with PCM callback but no meeting storage
    when: a valid pcm_chunk arrives
    then: callback is not invoked because there is no active KTalk meeting
    """
    bridge = KTalkBridge()
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    await bridge._process_message(_make_pcm_chunk_message())

    assert received == []
    err_out = capsys.readouterr().err
    assert "outside active ktalk meeting" in err_out


@pytest.mark.asyncio
async def test_pcm_chunk_for_orphan_meeting_is_dropped(capsys):
    """
    given: bridge storage has already switched back to orphan after leave
    when: a stale valid pcm_chunk arrives
    then: callback is not invoked so stale PCM cannot enter the new meeting
    """
    bridge = KTalkBridge()
    _attach_meeting_storage(bridge, source="orphan")
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    await bridge._process_message(_make_pcm_chunk_message())

    assert received == []
    err_out = capsys.readouterr().err
    assert "outside active ktalk meeting" in err_out


@pytest.mark.asyncio
async def test_pcm_chunk_callback_exception_does_not_break_bridge(capsys):
    """
    given: callback that raises on first call
    when: _process_message receives a pcm_chunk
    then: exception is caught and logged; subsequent messages still work
    """
    bridge = KTalkBridge()
    _attach_meeting_storage(bridge)

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


# --- send_refine: tier-2 refined segment broadcast ---


class RecordingClient:
    """Fake WS client whose `send` is a coroutine recording dispatched messages."""

    def __init__(self):
        self.sent: list[str] = []

    async def send(self, message: str):
        self.sent.append(message)


@pytest.mark.asyncio
async def test_send_refine_broadcasts_correct_shape():
    """
    given: KTalkBridge with a connected client and a running event loop
    when: send_refine called with a list of {segment_id, text} updates
    then: each connected client receives a single JSON frame with
          type="transcript_refine", updates list shaped {segment_id, text},
          and an envelope-level refined_at ISO8601 timestamp
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    client = RecordingClient()
    bridge._connected_clients.add(client)

    bridge.send_refine(
        [
            {"segment_id": 7, "text": "первый вариант текста"},
            {"segment_id": 8, "text": "второй"},
        ],
        meeting_id="2026-04-24_100101_meeting",
    )

    # asyncio.run_coroutine_threadsafe routes through call_soon_threadsafe
    # and needs a real scheduler tick on the target loop before the
    # coroutine runs. A short sleep (not 0) gives it room to complete.
    await asyncio.sleep(0.05)

    assert len(client.sent) == 1
    payload = json.loads(client.sent[0])
    assert payload["type"] == "transcript_refine"
    assert payload["updates"] == [
        {"segment_id": 7, "text": "первый вариант текста"},
        {"segment_id": 8, "text": "второй"},
    ]
    assert payload["meeting_id"] == "2026-04-24_100101_meeting"
    assert isinstance(payload["refined_at"], str)
    # datetime.now().isoformat() always contains a 'T' separator.
    assert "T" in payload["refined_at"]


@pytest.mark.asyncio
async def test_send_refine_empty_list_is_noop():
    """
    given: KTalkBridge with a connected client
    when: send_refine called with an empty updates list
    then: no message is dispatched (noop — no WS traffic on a no-op refine)
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    client = RecordingClient()
    bridge._connected_clients.add(client)

    bridge.send_refine([])

    await asyncio.sleep(0.05)

    assert client.sent == []


@pytest.mark.asyncio
async def test_send_refine_no_clients_is_noop():
    """
    given: KTalkBridge with no connected clients
    when: send_refine called with valid updates
    then: method returns without raising (idempotent, no-op broadcast)
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    assert not bridge._connected_clients

    # Should not raise.
    bridge.send_refine([{"segment_id": 1, "text": "x"}])


@pytest.mark.asyncio
async def test_send_refine_drops_extra_fields_from_updates():
    """
    given: bridge with a connected client; caller passes updates that
           include extra keys (e.g. `refined: True` left over from the
           update_segments payload)
    when: send_refine is invoked
    then: only the documented `segment_id` and `text` keys are forwarded;
          extra fields are stripped so extension/webapp consumers can rely
          on a stable schema
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    client = RecordingClient()
    bridge._connected_clients.add(client)

    bridge.send_refine([
        {"segment_id": 42, "text": "уточнённый", "refined": True, "extra": 1},
    ])

    await asyncio.sleep(0.05)
    assert len(client.sent) == 1
    payload = json.loads(client.sent[0])
    assert payload["updates"] == [{"segment_id": 42, "text": "уточнённый"}]
    # meeting_id not supplied → not included in envelope (back-compat for
    # consumers that predate the meeting-filter guard).
    assert "meeting_id" not in payload


@pytest.mark.asyncio
async def test_send_refine_omits_meeting_id_when_none():
    """
    given: bridge with a connected client
    when: send_refine called without a meeting_id argument
    then: the serialized payload omits `meeting_id` entirely
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    client = RecordingClient()
    bridge._connected_clients.add(client)

    bridge.send_refine([{"segment_id": 1, "text": "x"}])

    await asyncio.sleep(0.05)
    assert len(client.sent) == 1
    payload = json.loads(client.sent[0])
    assert "meeting_id" not in payload


# --- subscriber_hello protocol (frontend/bridge_subscriber.py ←→ bridge) ---


class ControllableFakeWebSocket(FakeWebSocket):
    """FakeWebSocket that stays open until `signal_close()` is invoked.

    Used to inspect mid-handler state (e.g. set membership after
    `subscriber_hello` is processed but before disconnect unwinds).
    """

    def __init__(self):
        super().__init__()
        self._close_event = asyncio.Event()

    async def __anext__(self):
        if self._messages:
            return self._messages.pop(0)
        await self._close_event.wait()
        raise StopAsyncIteration

    def signal_close(self):
        self._close_event.set()

    async def send(self, message: str):  # Supports outbound send in case
        pass                               # _handle_client writes to it.


@pytest.mark.asyncio
async def test_subscriber_hello_moves_client_to_subscriber_set():
    """
    given: a WS client connects and sends a `subscriber_hello` message first
    when: _handle_client processes the stream
    then: the client is moved out of _connected_clients into
          _subscriber_clients while the connection is still open
    """
    bridge = KTalkBridge()

    ws = ControllableFakeWebSocket()
    ws.add_message(json.dumps({"type": "subscriber_hello"}))

    # Run the handler as a task so the connection stays "open" until we
    # close it explicitly, letting us inspect the live state.
    handler_task = asyncio.create_task(bridge._handle_client(ws))
    for _ in range(20):
        if ws in bridge._subscriber_clients:
            break
        await asyncio.sleep(0.01)
    try:
        assert ws in bridge._subscriber_clients
        assert ws not in bridge._connected_clients
    finally:
        ws.signal_close()
        await handler_task

    # Finally-block cleanup must purge from both sets regardless of role.
    assert ws not in bridge._connected_clients
    assert ws not in bridge._subscriber_clients


@pytest.mark.asyncio
async def test_subscriber_disconnect_does_not_end_ktalk_meeting():
    """
    given: KTalkBridge with an active ktalk meeting AND an extension client
           still connected; a subscriber also connects and sends hello
    when: the subscriber disconnects
    then: on_ktalk_leave is NOT called — the extension is still present,
          so the meeting must continue. Previous regression: subscriber
          lived in _connected_clients so its disconnect could end meetings.
    """
    bridge = KTalkBridge()
    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    mock_storage.on_ktalk_leave = Mock()
    bridge.set_meeting_storage(mock_storage)

    # Extension client present.
    extension_ws = FakeWebSocket()
    bridge._connected_clients.add(extension_ws)

    subscriber_ws = FakeWebSocket()
    subscriber_ws.add_message(json.dumps({"type": "subscriber_hello"}))
    await bridge._handle_client(subscriber_ws)

    mock_storage.on_ktalk_leave.assert_not_called()
    assert extension_ws in bridge._connected_clients

    # Cleanup
    bridge._connected_clients.discard(extension_ws)


@pytest.mark.asyncio
async def test_extension_disconnect_ends_meeting_even_with_subscriber_connected():
    """
    given: KTalkBridge with an active ktalk meeting, one extension client
           AND one registered subscriber
    when: the extension disconnects
    then: on_ktalk_leave("disconnected") IS called. Previous regression:
          subscriber presence in _connected_clients blocked this path.
    """
    bridge = KTalkBridge()
    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    mock_storage.on_ktalk_leave = Mock(return_value="next_meeting_id")
    bridge.set_meeting_storage(mock_storage)

    # Pre-register a subscriber in the subscriber set (simulates prior
    # hello handshake).
    subscriber_ws = FakeWebSocket()
    bridge._subscriber_clients.add(subscriber_ws)

    # Extension client connects and immediately disconnects.
    extension_ws = FakeWebSocket()
    await bridge._handle_client(extension_ws)

    mock_storage.on_ktalk_leave.assert_called_once_with("disconnected")


@pytest.mark.asyncio
async def test_clock_offset_preserved_across_client_connects():
    """
    given: bridge has a non-zero _clock_offset from a prior clock_sync
    when: a new client connects via _handle_client
    then: _clock_offset is NOT reset to 0.0 — the extension's synced
          offset must survive subscriber reconnects and incidental
          extension reopens. Prior regression: every connect wiped offset
          to 0 for up to ~30 s (next clock_sync interval).
    """
    bridge = KTalkBridge()
    bridge._clock_offset = 1.234

    ws = FakeWebSocket()  # No messages — disconnect immediately.
    await bridge._handle_client(ws)

    assert bridge._clock_offset == 1.234


@pytest.mark.asyncio
async def test_send_refine_broadcasts_to_subscribers():
    """
    given: bridge with ONLY a subscriber registered (no extensions)
    when: send_refine is invoked
    then: the subscriber still receives the transcript_refine envelope so
          the webapp subscriber path works even when no extension is live.
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    subscriber = RecordingClient()
    bridge._subscriber_clients.add(subscriber)

    bridge.send_refine([{"segment_id": 3, "text": "t"}])

    await asyncio.sleep(0.05)
    assert len(subscriber.sent) == 1
    payload = json.loads(subscriber.sent[0])
    assert payload["type"] == "transcript_refine"
    assert payload["updates"] == [{"segment_id": 3, "text": "t"}]


@pytest.mark.asyncio
async def test_subscriber_hello_triggers_end_meeting_if_extension_left_during_race():
    """
    given: active ktalk meeting; extension disconnects while a subscriber
           WS is open but still transiently in _connected_clients (before
           its subscriber_hello message is processed)
    when:  the subscriber_hello finally arrives and the socket moves into
           _subscriber_clients, revealing _connected_clients is empty
    then:  on_ktalk_leave("disconnected") IS called — the race-window
           lifecycle miss is recovered. Prior regression: the extension's
           own finally-block saw _connected_clients={subscriber_ws} and
           skipped the cleanup; subscriber_hello did not re-check.
    """
    bridge = KTalkBridge()
    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    mock_storage.on_ktalk_leave = Mock(return_value="next_meeting_id")
    mock_storage.current_meeting_path = Path("/tmp/m.jsonl")
    mock_storage.current_meeting_id = "m"
    bridge.set_meeting_storage(mock_storage)

    # Simulate race end-state: the subscriber is in _connected_clients
    # (it has connected but has not yet had its hello processed). The
    # extension already disconnected; its finally-block ran, saw
    # _connected_clients={subscriber_ws}, and skipped cleanup.
    subscriber_ws = ControllableFakeWebSocket()
    subscriber_ws.add_message(json.dumps({"type": "subscriber_hello"}))

    handler_task = asyncio.create_task(bridge._handle_client(subscriber_ws))

    # Wait for the hello-handler side-effect: on_ktalk_leave is fired
    # the moment _connected_clients empties out as the socket is
    # moved into _subscriber_clients.
    for _ in range(50):
        if mock_storage.on_ktalk_leave.called:
            break
        await asyncio.sleep(0.01)
    try:
        mock_storage.on_ktalk_leave.assert_called_once_with("disconnected")
        assert subscriber_ws in bridge._subscriber_clients
        assert subscriber_ws not in bridge._connected_clients
    finally:
        subscriber_ws.signal_close()
        await handler_task


@pytest.mark.asyncio
async def test_subscriber_hello_without_active_meeting_is_noop():
    """
    given: no active meeting, no extensions, a subscriber connects
    when:  its subscriber_hello is processed
    then:  _end_meeting_on_last_extension_gone is safe to call — no
           on_ktalk_leave invocation because no meeting is active.
    """
    bridge = KTalkBridge()
    mock_storage = Mock()
    mock_storage.current_meeting_path = None
    mock_storage.on_ktalk_leave = Mock()
    bridge.set_meeting_storage(mock_storage)

    subscriber_ws = ControllableFakeWebSocket()
    subscriber_ws.add_message(json.dumps({"type": "subscriber_hello"}))

    handler_task = asyncio.create_task(bridge._handle_client(subscriber_ws))
    for _ in range(30):
        if subscriber_ws in bridge._subscriber_clients:
            break
        await asyncio.sleep(0.01)
    try:
        assert subscriber_ws in bridge._subscriber_clients
        mock_storage.on_ktalk_leave.assert_not_called()
    finally:
        subscriber_ws.signal_close()
        await handler_task


# --- copilot_channel_hello: copilot channel client registration ---


@pytest.mark.asyncio
async def test_copilot_channel_hello_registers_client():
    """
    given: a WS client connects and sends `copilot_channel_hello`
    when:  _handle_client processes the stream
    then:  the client is moved out of _connected_clients into
           _copilot_channel_clients while the connection is still open;
           the disconnect cleanup purges it from both sets
    """
    bridge = KTalkBridge()

    ws = ControllableFakeWebSocket()
    ws.add_message(json.dumps({"type": "copilot_channel_hello"}))

    handler_task = asyncio.create_task(bridge._handle_client(ws))
    for _ in range(20):
        if ws in bridge._copilot_channel_clients:
            break
        await asyncio.sleep(0.01)
    try:
        assert ws in bridge._copilot_channel_clients
        assert ws not in bridge._connected_clients
        assert ws not in bridge._subscriber_clients
    finally:
        ws.signal_close()
        await handler_task

    # Finally-block cleanup must purge from all three sets.
    assert ws not in bridge._connected_clients
    assert ws not in bridge._subscriber_clients
    assert ws not in bridge._copilot_channel_clients


@pytest.mark.asyncio
async def test_copilot_channel_hello_triggers_end_meeting_if_extension_left_during_race():
    """
    given: active ktalk meeting; extension disconnects while a copilot
           channel WS is open but still transiently in _connected_clients
           (before its copilot_channel_hello message is processed)
    when:  the copilot_channel_hello finally arrives and the socket moves
           into _copilot_channel_clients, revealing _connected_clients is
           empty
    then:  on_ktalk_leave("disconnected") IS called — the race-window
           lifecycle miss is recovered. Mirrors the analogous
           subscriber_hello race-recovery contract.
    """
    bridge = KTalkBridge()
    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    mock_storage.on_ktalk_leave = Mock(return_value="next_meeting_id")
    mock_storage.current_meeting_path = Path("/tmp/m.jsonl")
    mock_storage.current_meeting_id = "m"
    bridge.set_meeting_storage(mock_storage)

    # Simulate race end-state: the channel client is in _connected_clients
    # (it has connected but has not yet had its hello processed). The
    # extension already disconnected; its finally-block ran, saw
    # _connected_clients={channel_ws}, and skipped cleanup.
    channel_ws = ControllableFakeWebSocket()
    channel_ws.add_message(json.dumps({"type": "copilot_channel_hello"}))

    handler_task = asyncio.create_task(bridge._handle_client(channel_ws))

    # Wait for the hello-handler side-effect: on_ktalk_leave is fired
    # the moment _connected_clients empties out as the socket is
    # moved into _copilot_channel_clients.
    for _ in range(50):
        if mock_storage.on_ktalk_leave.called:
            break
        await asyncio.sleep(0.01)
    try:
        mock_storage.on_ktalk_leave.assert_called_once_with("disconnected")
        assert channel_ws in bridge._copilot_channel_clients
        assert channel_ws not in bridge._connected_clients
    finally:
        channel_ws.signal_close()
        await handler_task


@pytest.mark.asyncio
async def test_copilot_channel_disconnect_does_not_end_ktalk_meeting():
    """
    given: KTalkBridge with an active ktalk meeting AND an extension client
           still connected; a copilot channel client also connects and sends
           hello
    when:  the copilot channel disconnects
    then:  on_ktalk_leave is NOT called — the extension is still present,
           so the meeting must continue. Channel clients live in their own
           set (`_copilot_channel_clients`) so they never block lifecycle.
    """
    bridge = KTalkBridge()
    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    mock_storage.on_ktalk_leave = Mock()
    bridge.set_meeting_storage(mock_storage)

    # Extension client present.
    extension_ws = FakeWebSocket()
    bridge._connected_clients.add(extension_ws)

    channel_ws = FakeWebSocket()
    channel_ws.add_message(json.dumps({"type": "copilot_channel_hello"}))
    await bridge._handle_client(channel_ws)

    mock_storage.on_ktalk_leave.assert_not_called()
    assert extension_ws in bridge._connected_clients

    # Cleanup
    bridge._connected_clients.discard(extension_ws)


# --- copilot_chat_message: extension -> channel forwarding ---


@pytest.mark.asyncio
async def test_copilot_chat_message_forwarded_to_channel_client():
    """
    given: a registered Copilot channel client AND a mock meeting_storage
           reporting a current meeting_id; the extension sends
           `copilot_chat_message` with request_id, text, and a
           transcript_window passthrough payload
    when:  _process_message dispatches the frame
    then:  the channel client receives one `copilot_user_message` frame
           that preserves request_id/text and carries the meeting_id from
           storage, the meeting_title/transcript_window from the inbound
           data, and a numeric created_at; the originating extension
           websocket receives no status frame
    """
    bridge = KTalkBridge()

    mock_storage = Mock()
    mock_storage.get_current_meeting_id = Mock(return_value="2026-04-26_test_meeting")
    bridge.set_meeting_storage(mock_storage)

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)

    extension_ws = RecordingClient()

    transcript_window = [
        {"speaker": "Alice", "text": "пробуем", "ts": 1700000000.0},
    ]
    await bridge._process_message(
        {
            "type": "copilot_chat_message",
            "request_id": "req-42",
            "text": "что только что сказала Алиса?",
            "meeting_title": "Sprint review",
            "transcript_window": transcript_window,
            "created_at": 1700000123456.0,
        },
        websocket=extension_ws,
    )

    assert len(channel_client.sent) == 1
    payload = json.loads(channel_client.sent[0])
    assert payload["type"] == "copilot_user_message"
    assert payload["request_id"] == "req-42"
    assert payload["text"] == "что только что сказала Алиса?"
    assert payload["meeting_id"] == "2026-04-26_test_meeting"
    assert payload["meeting_title"] == "Sprint review"
    assert payload["transcript_window"] == transcript_window
    assert payload["created_at"] == 1700000123456.0

    # Originator should not receive any status frame on the happy path.
    assert extension_ws.sent == []


@pytest.mark.asyncio
async def test_copilot_chat_message_without_channel_returns_disconnected_status():
    """
    given: no Copilot channel client is connected
    when:  the extension sends `copilot_chat_message`
    then:  the originating websocket receives a `copilot_status` frame
           with status="channel_disconnected" carrying the same request_id
           so the UI can surface the failure
    """
    bridge = KTalkBridge()
    assert not bridge._copilot_channel_clients

    extension_ws = RecordingClient()

    await bridge._process_message(
        {
            "type": "copilot_chat_message",
            "request_id": "req-no-channel",
            "text": "сводка",
        },
        websocket=extension_ws,
    )

    assert len(extension_ws.sent) == 1
    payload = json.loads(extension_ws.sent[0])
    assert payload["type"] == "copilot_status"
    assert payload["request_id"] == "req-no-channel"
    assert payload["status"] == "channel_disconnected"
    assert isinstance(payload.get("message"), str) and payload["message"]


@pytest.mark.asyncio
async def test_copilot_chat_message_with_invalid_fields_is_ignored():
    """
    given: a registered Copilot channel client
    when:  copilot_chat_message frames arrive with missing/empty
           request_id or text
    then:  no frame is forwarded to the channel client and no status
           is sent back — invalid input is dropped silently per the
           dispatcher's "log and return" precedent
    """
    bridge = KTalkBridge()

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)

    extension_ws = RecordingClient()

    # Missing request_id.
    await bridge._process_message(
        {"type": "copilot_chat_message", "text": "x"},
        websocket=extension_ws,
    )
    # Empty request_id.
    await bridge._process_message(
        {"type": "copilot_chat_message", "request_id": "", "text": "x"},
        websocket=extension_ws,
    )
    # Missing text.
    await bridge._process_message(
        {"type": "copilot_chat_message", "request_id": "req-1"},
        websocket=extension_ws,
    )
    # Empty text.
    await bridge._process_message(
        {"type": "copilot_chat_message", "request_id": "req-1", "text": ""},
        websocket=extension_ws,
    )

    assert channel_client.sent == []
    assert extension_ws.sent == []


# --- copilot_channel_reply / copilot_channel_status: channel -> extension relay ---


@pytest.mark.asyncio
async def test_copilot_channel_reply_relayed_to_extension_clients():
    """
    given: an extension client registered in `_connected_clients`
    when:  the channel server sends `copilot_channel_reply` with
           request_id, kind, text, meeting_id, created_at
    then:  the extension client receives a single `copilot_reply` frame
           preserving all fields (only the type is renamed)
    """
    bridge = KTalkBridge()

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message(
        {
            "type": "copilot_channel_reply",
            "request_id": "req-42",
            "kind": "manual",
            "text": "Алиса спросила про метрики.",
            "meeting_id": "2026-04-26_test_meeting",
            "created_at": 1700000123456.0,
        }
    )

    assert len(extension_client.sent) == 1
    payload = json.loads(extension_client.sent[0])
    assert payload["type"] == "copilot_reply"
    assert payload["request_id"] == "req-42"
    assert payload["kind"] == "manual"
    assert payload["text"] == "Алиса спросила про метрики."
    assert payload["meeting_id"] == "2026-04-26_test_meeting"
    assert payload["created_at"] == 1700000123456.0


@pytest.mark.asyncio
async def test_copilot_channel_reply_does_not_loopback_to_channel_clients():
    """
    given: a channel client in `_copilot_channel_clients` AND an extension
           client in `_connected_clients`
    when:  the channel server sends `copilot_channel_reply`
    then:  only the extension client receives the relayed `copilot_reply`
           — the channel client (the originator) does not see the frame
           because `_broadcast_message` only iterates `_connected_clients`
    """
    bridge = KTalkBridge()

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message(
        {
            "type": "copilot_channel_reply",
            "request_id": "req-7",
            "kind": "live",
            "text": "ok",
            "meeting_id": None,
        }
    )

    assert len(extension_client.sent) == 1
    assert channel_client.sent == []


@pytest.mark.asyncio
async def test_copilot_channel_status_relayed_to_extension_clients():
    """
    given: an extension client registered in `_connected_clients`
    when:  the channel server sends `copilot_channel_status` with
           request_id, status, message
    then:  the extension client receives a single `copilot_status` frame
           preserving all fields (only the type is renamed)
    """
    bridge = KTalkBridge()

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message(
        {
            "type": "copilot_channel_status",
            "request_id": "req-42",
            "status": "working",
            "message": "Claude is thinking",
        }
    )

    assert len(extension_client.sent) == 1
    payload = json.loads(extension_client.sent[0])
    assert payload["type"] == "copilot_status"
    assert payload["request_id"] == "req-42"
    assert payload["status"] == "working"
    assert payload["message"] == "Claude is thinking"


@pytest.mark.asyncio
async def test_copilot_channel_reply_with_invalid_fields_is_ignored():
    """
    given: an extension client registered in `_connected_clients`
    when:  copilot_channel_reply frames arrive with missing/empty
           request_id, kind, or with a non-string text
    then:  no `copilot_reply` is broadcast — invalid input is dropped
           silently per the dispatcher's "log and return" precedent
    """
    bridge = KTalkBridge()
    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    # Missing request_id.
    await bridge._process_message(
        {"type": "copilot_channel_reply", "kind": "manual", "text": "x"}
    )
    # Empty request_id.
    await bridge._process_message(
        {"type": "copilot_channel_reply", "request_id": "", "kind": "manual", "text": "x"}
    )
    # Missing kind.
    await bridge._process_message(
        {"type": "copilot_channel_reply", "request_id": "req-1", "text": "x"}
    )
    # Non-string text.
    await bridge._process_message(
        {"type": "copilot_channel_reply", "request_id": "req-1", "kind": "manual", "text": 123}
    )

    assert extension_client.sent == []


@pytest.mark.asyncio
async def test_copilot_channel_status_with_invalid_fields_is_ignored():
    """
    given: an extension client registered in `_connected_clients`
    when:  copilot_channel_status frames arrive with missing/empty
           request_id or status
    then:  no `copilot_status` is broadcast — invalid input is dropped
           silently
    """
    bridge = KTalkBridge()
    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message(
        {"type": "copilot_channel_status", "status": "working"}
    )
    await bridge._process_message(
        {"type": "copilot_channel_status", "request_id": "req-1"}
    )
    await bridge._process_message(
        {"type": "copilot_channel_status", "request_id": "req-1", "status": ""}
    )

    assert extension_client.sent == []


# --- meeting_started / meeting_ended: lifecycle frames to channel clients ---


def _meeting_storage_mock(meeting_id="meet-1", new_meeting_id="meet-2"):
    """Build a Mock MeetingStorage shaped for meeting_join/leave dispatch."""
    storage = Mock()
    storage.current_meeting_path = Path("/tmp/old.jsonl")
    storage.current_meeting_id = meeting_id
    storage.on_meeting_join = Mock(return_value=meeting_id)
    storage.on_ktalk_leave = Mock(return_value=new_meeting_id)
    storage.get_current_transcript = Mock(return_value=[])
    storage.get_current_meeting_id = Mock(return_value=meeting_id)
    storage._load_meta = Mock(return_value={"source": "ktalk"})
    return storage


@pytest.mark.asyncio
async def test_meeting_join_emits_meeting_started_to_channel_and_extension():
    """
    given: a registered Copilot channel client AND an extension client;
           meeting_storage configured to accept on_meeting_join
    when:  the bridge processes a `meeting_join` frame
    then:  both the channel client and the extension client receive a
           `meeting_started` frame carrying the new meeting_id, the
           supplied meeting_title, and a numeric created_at
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_meeting_storage_mock(meeting_id="2026-04-26_review"))

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)
    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/abc",
        "title": "Sprint review",
        "platform": "ktalk",
    })

    channel_started = [
        json.loads(m) for m in channel_client.sent if json.loads(m)["type"] == "meeting_started"
    ]
    assert len(channel_started) == 1
    payload = channel_started[0]
    assert payload["meeting_id"] == "2026-04-26_review"
    assert payload["meeting_title"] == "Sprint review"
    assert isinstance(payload.get("created_at"), (int, float))

    extension_started = [
        json.loads(m) for m in extension_client.sent if json.loads(m)["type"] == "meeting_started"
    ]
    assert len(extension_started) == 1
    assert extension_started[0]["meeting_id"] == "2026-04-26_review"


@pytest.mark.asyncio
async def test_meeting_leave_emits_meeting_ended_to_channel_and_extension():
    """
    given: a registered Copilot channel client AND an extension client;
           meeting_storage with current_meeting_id="m-prev"
    when:  the bridge processes a `meeting_leave` frame
    then:  both the channel client and the extension client receive a
           `meeting_ended` frame carrying the previous meeting_id
           (captured before on_ktalk_leave switches storage state)
    """
    bridge = KTalkBridge()
    storage = _meeting_storage_mock(meeting_id="m-prev", new_meeting_id="m-next")
    bridge.set_meeting_storage(storage)

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)
    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message({
        "type": "meeting_leave",
        "reason": "user_left",
    })

    channel_ended = [
        json.loads(m) for m in channel_client.sent if json.loads(m)["type"] == "meeting_ended"
    ]
    assert len(channel_ended) == 1
    assert channel_ended[0]["meeting_id"] == "m-prev"
    assert isinstance(channel_ended[0].get("created_at"), (int, float))

    extension_ended = [
        json.loads(m) for m in extension_client.sent if json.loads(m)["type"] == "meeting_ended"
    ]
    assert len(extension_ended) == 1
    assert extension_ended[0]["meeting_id"] == "m-prev"


# --- stale request_id: late replies dropped after meeting_leave ---


@pytest.mark.asyncio
async def test_late_copilot_reply_for_previous_meeting_is_dropped():
    """
    given: a `copilot_chat_message` was forwarded for meeting "m-prev",
           recording the request_id in `_copilot_pending_requests`;
           then `meeting_leave` marked the request as stale
    when:  a `copilot_channel_reply` arrives carrying the same request_id
    then:  the reply is NOT relayed to extension clients — the stale
           bookkeeping protects the next meeting's chat tab from leaks,
           and the entry is purged so the dict cannot grow forever
    """
    bridge = KTalkBridge()
    storage = _meeting_storage_mock(meeting_id="m-prev", new_meeting_id="m-next")
    bridge.set_meeting_storage(storage)

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)
    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    # Step 1: extension sends a chat message during meeting "m-prev".
    await bridge._process_message({
        "type": "copilot_chat_message",
        "request_id": "req-late",
        "text": "сводка за последние 5 минут",
    }, websocket=RecordingClient())
    assert bridge._copilot_pending_requests.get("req-late") == "m-prev"

    # Step 2: user leaves the meeting; the entry must be marked stale.
    await bridge._process_message({"type": "meeting_leave", "reason": "user_left"})
    assert bridge._copilot_pending_requests["req-late"].startswith("stale:")

    # Step 3: late reply arrives — must be dropped, not relayed.
    extension_client.sent.clear()
    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-late",
        "kind": "manual",
        "text": "поздняя сводка",
    })
    relayed = [m for m in extension_client.sent if json.loads(m)["type"] == "copilot_reply"]
    assert relayed == []
    # The stale entry is purged after a reply lands so the dict cannot
    # grow indefinitely across meetings.
    assert "req-late" not in bridge._copilot_pending_requests


@pytest.mark.asyncio
async def test_late_copilot_status_for_previous_meeting_is_dropped():
    """
    given: a stale request_id (already marked via meeting_leave)
    when:  a `copilot_channel_status` arrives carrying that request_id
    then:  no `copilot_status` frame is broadcast to extension clients
    """
    bridge = KTalkBridge()
    bridge._copilot_pending_requests["req-late"] = "stale:m-prev"

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message({
        "type": "copilot_channel_status",
        "request_id": "req-late",
        "status": "working",
    })

    assert extension_client.sent == []


@pytest.mark.asyncio
async def test_non_stale_copilot_reply_is_still_relayed():
    """
    given: a non-stale request_id recorded for the active meeting
    when:  a `copilot_channel_reply` arrives carrying that request_id
    then:  the reply IS relayed to extension clients (the stale-drop
           gate is opt-in — no marker means alive)
    """
    bridge = KTalkBridge()
    bridge._copilot_pending_requests["req-active"] = "m-current"

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-active",
        "kind": "manual",
        "text": "ответ",
        "meeting_id": "m-current",
    })

    relayed = [json.loads(m) for m in extension_client.sent if json.loads(m)["type"] == "copilot_reply"]
    assert len(relayed) == 1
    assert relayed[0]["request_id"] == "req-active"
    assert relayed[0]["text"] == "ответ"


# --- send_transcript: source_id payload serialization ---


def test_send_transcript_includes_source_id_when_non_empty():
    """
    given: send_transcript called with a non-empty source_id
    when:  payload is serialized for broadcast
    then:  the `source_id` key is present with the supplied value
    """
    from unittest.mock import patch

    bridge = KTalkBridge()
    bridge._loop = None  # skip dispatch; we only inspect the built payload
    with patch("ktalk_bridge.json.dumps") as mock_dumps:
        bridge.send_transcript(
            "Alice", "привет", "10:00:00", source_id="pid-a"
        )
        payload = mock_dumps.call_args.args[0]
        assert payload.get("source_id") == "pid-a"


def test_send_transcript_omits_source_id_when_empty_default():
    """
    given: send_transcript called without source_id (empty default)
    when:  payload is serialized for broadcast
    then:  the `source_id` key is absent — backward-compatible with older
           extension builds that never looked at it
    """
    from unittest.mock import patch

    bridge = KTalkBridge()
    bridge._loop = None
    with patch("ktalk_bridge.json.dumps") as mock_dumps:
        bridge.send_transcript("Alice", "привет", "10:00:00")
        payload = mock_dumps.call_args.args[0]
        assert "source_id" not in payload


# --- Recent transcript buffer + get_recent_transcript snapshot ---


def _make_storage_for_meeting(meeting_id: str) -> Mock:
    """Build a Mock MeetingStorage that always reports `meeting_id`."""
    storage = Mock()
    storage.get_current_meeting_id = Mock(return_value=meeting_id)
    return storage


def test_send_transcript_appends_buffer_entry_with_meeting_id():
    """
    given: bridge with meeting_storage reporting "m-1" and no connected
           clients (broadcast is a noop)
    when:  send_transcript is called with a positive segment_id
    then:  the recent-transcript buffer carries an entry keyed by
           segment_id with speaker/text/meeting_id/refined=False and a
           wall-clock end_ms in the present
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    bridge.send_transcript("Alice", "привет всем", "10:00:00", segment_id=42)

    assert 42 in bridge._copilot_transcript_buffer
    entry = bridge._copilot_transcript_buffer[42]
    assert entry["segment_id"] == 42
    assert entry["speaker"] == "Alice"
    assert entry["text"] == "привет всем"
    assert entry["meeting_id"] == "m-1"
    assert entry["refined"] is False
    assert entry["start_ms"] <= entry["end_ms"]
    assert list(bridge._copilot_transcript_order) == [42]


def test_send_transcript_skips_buffer_when_segment_id_zero():
    """
    given: bridge with meeting_storage; send_transcript called without
           a segment_id (defaults to 0)
    when:  the call completes
    then:  the buffer stays empty — segment_id=0 is the "no id" sentinel
           used by mic-only paths and must not pollute the snapshot
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    bridge.send_transcript("Alice", "no-id", "10:00:00")

    assert bridge._copilot_transcript_buffer == {}
    assert list(bridge._copilot_transcript_order) == []


def test_send_transcript_replaces_entry_on_continuation():
    """
    given: an existing buffer entry for segment_id=7 (partial transcript)
    when:  a continuation chunk lands carrying the same segment_id with
           appended text
    then:  the entry text is overwritten while start_ms is preserved and
           end_ms advances; the deque is not duplicated
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    bridge.send_transcript("Alice", "первая", "10:00:00", segment_id=7)
    first_entry = bridge._copilot_transcript_buffer[7]
    first_start_ms = first_entry["start_ms"]
    first_end_ms = first_entry["end_ms"]

    # Force a measurable delta so end_ms moves forward without flaking.
    time.sleep(0.005)

    bridge.send_transcript(
        "Alice", "первая часть", "10:00:01", is_continuation=True, segment_id=7
    )

    assert list(bridge._copilot_transcript_order) == [7]
    updated = bridge._copilot_transcript_buffer[7]
    assert updated["text"] == "первая часть"
    assert updated["start_ms"] == first_start_ms
    assert updated["end_ms"] >= first_end_ms


def test_send_refine_updates_buffer_text_and_marks_refined():
    """
    given: a buffer entry from a prior send_transcript
    when:  send_refine carries an update for that segment_id
    then:  the entry text is replaced with the refined string and
           refined=True; segments not in the buffer are silently ignored
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    bridge.send_transcript("Alice", "тираж1", "10:00:00", segment_id=1)
    bridge.send_transcript("Alice", "тираж2", "10:00:01", segment_id=2)

    bridge.send_refine([
        {"segment_id": 1, "text": "тираж 1 точный"},
        {"segment_id": 99, "text": "не существует"},  # ignored
    ])

    refined = bridge._copilot_transcript_buffer[1]
    assert refined["text"] == "тираж 1 точный"
    assert refined["refined"] is True

    # Untouched entry stays as-is.
    assert bridge._copilot_transcript_buffer[2]["refined"] is False
    assert bridge._copilot_transcript_buffer[2]["text"] == "тираж2"
    # Phantom segment_id was not inserted.
    assert 99 not in bridge._copilot_transcript_buffer


@pytest.mark.asyncio
async def test_get_recent_transcript_request_returns_segments_within_window():
    """
    given: bridge with three buffered segments, one of which is older than
           the requested window
    when:  get_recent_transcript_request arrives over the channel WS
    then:  the response is sent only on the originator websocket and
           contains segments newer than the window cutoff in insertion
           order
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    bridge.send_transcript("Alice", "первая", "10:00:00", segment_id=1)
    bridge.send_transcript("Bob",   "вторая", "10:00:05", segment_id=2)
    bridge.send_transcript("Alice", "третья", "10:00:10", segment_id=3)

    # Push segment 1 outside a 60-second window by rewriting end_ms to the
    # past — direct mutation matches how the buffer is filtered.
    now_ms = time.time() * 1000
    bridge._copilot_transcript_buffer[1]["end_ms"] = now_ms - 5 * 60 * 1000

    requester = RecordingClient()
    other_client = RecordingClient()
    bridge._connected_clients.add(other_client)

    await bridge._process_message(
        {
            "type": "get_recent_transcript_request",
            "request_id": "snap-1",
            "window_seconds": 60,
        },
        websocket=requester,
    )

    assert len(requester.sent) == 1
    payload = json.loads(requester.sent[0])
    assert payload["type"] == "get_recent_transcript_response"
    assert payload["request_id"] == "snap-1"
    ids = [s["segment_id"] for s in payload["segments"]]
    assert ids == [2, 3]
    # Non-originator clients must not receive the snapshot.
    assert other_client.sent == []


@pytest.mark.asyncio
async def test_get_recent_transcript_request_filters_by_meeting_id():
    """
    given: buffer carries entries from a prior meeting "m-prev" and the
           current meeting "m-now"
    when:  the request asks for the active-meeting snapshot
    then:  only entries with meeting_id == current meeting are returned
    """
    bridge = KTalkBridge()
    bridge._loop = None
    storage = _make_storage_for_meeting("m-now")
    bridge.set_meeting_storage(storage)

    # Prior meeting entries — manually inject so we can control meeting_id.
    bridge._copilot_transcript_buffer[10] = {
        "segment_id": 10,
        "speaker": "Alice",
        "text": "old",
        "start_ms": time.time() * 1000,
        "end_ms": time.time() * 1000,
        "refined": False,
        "meeting_id": "m-prev",
    }
    bridge._copilot_transcript_order.append(10)

    bridge.send_transcript("Bob", "new", "10:00:00", segment_id=11)

    requester = RecordingClient()
    await bridge._process_message(
        {
            "type": "get_recent_transcript_request",
            "request_id": "snap-2",
            "window_seconds": 600,
        },
        websocket=requester,
    )

    payload = json.loads(requester.sent[0])
    ids = [s["segment_id"] for s in payload["segments"]]
    assert ids == [11]


@pytest.mark.asyncio
async def test_get_recent_transcript_request_respects_since_segment_id():
    """
    given: buffer with three segments in the active meeting
    when:  the request supplies since_segment_id=2
    then:  only segments with segment_id strictly greater than 2 come back
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    bridge.send_transcript("Alice", "1", "10:00:00", segment_id=1)
    bridge.send_transcript("Bob",   "2", "10:00:01", segment_id=2)
    bridge.send_transcript("Alice", "3", "10:00:02", segment_id=3)

    requester = RecordingClient()
    await bridge._process_message(
        {
            "type": "get_recent_transcript_request",
            "request_id": "snap-3",
            "window_seconds": 600,
            "since_segment_id": 2,
        },
        websocket=requester,
    )

    payload = json.loads(requester.sent[0])
    ids = [s["segment_id"] for s in payload["segments"]]
    assert ids == [3]


@pytest.mark.asyncio
async def test_get_recent_transcript_request_can_skip_refined_entries():
    """
    given: buffer with one refined and one tier-1 segment
    when:  the request sets include_refined=False
    then:  the refined entry is omitted from the response
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    bridge.send_transcript("Alice", "tier1", "10:00:00", segment_id=1)
    bridge.send_transcript("Bob",   "tier2", "10:00:01", segment_id=2)
    bridge.send_refine([{"segment_id": 2, "text": "tier2 refined"}])

    requester = RecordingClient()
    await bridge._process_message(
        {
            "type": "get_recent_transcript_request",
            "request_id": "snap-4",
            "window_seconds": 600,
            "include_refined": False,
        },
        websocket=requester,
    )

    payload = json.loads(requester.sent[0])
    ids = [s["segment_id"] for s in payload["segments"]]
    assert ids == [1]


@pytest.mark.asyncio
async def test_get_recent_transcript_request_invalid_request_id_logs_and_drops(capsys):
    """
    given: an originator websocket
    when:  get_recent_transcript_request arrives without a request_id
    then:  no response is sent and a warning is logged — invalid input is
           dropped silently per the dispatcher's "log and return" precedent
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    requester = RecordingClient()
    await bridge._process_message(
        {"type": "get_recent_transcript_request"},
        websocket=requester,
    )
    assert requester.sent == []
    assert "get_recent_transcript_request" in capsys.readouterr().err


def test_record_copilot_transcript_evicts_oldest_when_deque_full():
    """
    given: a bridge whose buffer cap is much smaller than 300 (test deque)
    when:  more than `maxlen` segments are recorded
    then:  the oldest segment_id is evicted from BOTH the deque and the
           dict so the buffer cannot grow without bound
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    # Shrink the deque to exercise eviction without 300 inserts.
    bridge._copilot_transcript_order = collections.deque(maxlen=3)
    for sid in range(1, 5):  # inserts 1..4 — should evict 1
        bridge.send_transcript("Alice", f"t{sid}", "10:00:00", segment_id=sid)

    assert list(bridge._copilot_transcript_order) == [2, 3, 4]
    assert set(bridge._copilot_transcript_buffer.keys()) == {2, 3, 4}
