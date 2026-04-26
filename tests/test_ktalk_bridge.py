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
