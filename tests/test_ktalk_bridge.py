"""Tests for ktalk_bridge.py."""
import asyncio
import base64
import collections
import json
import time
import pytest
import websockets
from pathlib import Path
from unittest.mock import Mock, AsyncMock, patch

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from ktalk_bridge import KTalkBridge, Participant, PcmChunk
from recog.ai_provider import DEFAULT_CODEX_MODEL


@pytest.mark.asyncio
async def test_call_codex_cli_marks_prompt_and_uses_ephemeral():
    bridge = KTalkBridge()
    created = {}

    class FakeProcess:
        returncode = 0

        async def communicate(self, input=None):
            created["input"] = input
            return (
                b'{"type":"item.completed","item":{"type":"agent_message","text":"answer"}}\n',
                b"",
            )

    async def fake_create_subprocess_exec(*cmd, **kwargs):
        created["cmd"] = cmd
        return FakeProcess()

    with patch("ktalk_bridge.resolve_cli", return_value="/mock/codex"), \
         patch("asyncio.create_subprocess_exec", new=AsyncMock(side_effect=fake_create_subprocess_exec)):
        answer = await bridge._call_codex_cli("hello", task="summarize")

    assert answer == "answer"
    assert created["cmd"][3] == DEFAULT_CODEX_MODEL
    assert "--ephemeral" in created["cmd"]
    assert created["input"].decode().startswith(
        "<!-- ccs-automation:recog task=summarize -->\nhello"
    )


@pytest.mark.asyncio
async def test_call_claude_cli_marks_prompt():
    bridge = KTalkBridge()
    created = {}

    class FakeProcess:
        returncode = 0

        async def communicate(self):
            return b"answer\n", b""

    async def fake_create_subprocess_exec(*cmd, **kwargs):
        created["cmd"] = cmd
        return FakeProcess()

    with patch("ktalk_bridge.resolve_cli", return_value="/mock/claude"), \
         patch("asyncio.create_subprocess_exec", new=AsyncMock(side_effect=fake_create_subprocess_exec)):
        answer = await bridge._call_claude_cli("hello", model="opus", task="anchor")

    assert answer == "answer"
    assert created["cmd"][:2] == ("/mock/claude", "-p")
    assert created["cmd"][2].startswith(
        "<!-- ccs-automation:recog task=anchor -->\nhello"
    )


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
async def test_pcm_chunk_cross_meeting_dropped(capsys):
    """
    given: bridge with active meeting "current-meeting-id" and a callback
    when: _process_message receives a pcm_chunk frame whose `meeting_id`
          belongs to a different meeting (sibling KTalk tab still streaming
          its own participants while this tab joined a newer meeting)
    then: the chunk is dropped before reaching the callback so audio from
          the sibling tab does not get written into the current meeting's
          transcript.jsonl
    """
    bridge = KTalkBridge()
    storage = _attach_meeting_storage(bridge)
    storage.get_current_meeting_id = Mock(return_value="current-meeting-id")
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    cross_message = _make_pcm_chunk_message(pid="pid-sibling")
    cross_message["meeting_id"] = "sibling-tab-meeting-id"
    await bridge._process_message(cross_message)

    assert received == []
    err_out = capsys.readouterr().err
    assert "cross-meeting" in err_out

    # Frame with matching meeting_id still passes.
    own_message = _make_pcm_chunk_message(pid="pid-self")
    own_message["meeting_id"] = "current-meeting-id"
    await bridge._process_message(own_message)
    assert len(received) == 1
    assert received[0].pid == "pid-self"


@pytest.mark.asyncio
async def test_pcm_chunk_without_meeting_id_passes_through():
    """
    given: bridge with active meeting and a callback
    when: _process_message receives a pcm_chunk frame WITHOUT a meeting_id
          field (older extension build that hasn't been rebuilt yet)
    then: the chunk passes through the cross-meeting guard (missing field
          == no filter), preserving the pre-fix single-tab behaviour
    """
    bridge = KTalkBridge()
    storage = _attach_meeting_storage(bridge)
    storage.get_current_meeting_id = Mock(return_value="current-meeting-id")
    received: list[PcmChunk] = []
    bridge.set_pcm_chunk_callback(received.append)

    legacy_message = _make_pcm_chunk_message()
    assert "meeting_id" not in legacy_message
    await bridge._process_message(legacy_message)
    assert len(received) == 1


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


@pytest.mark.asyncio
async def test_last_channel_client_disconnect_broadcasts_channel_disconnected_and_drops_pending():
    """
    given: a registered channel client and an extension client; pending
           chat requests populated in `_copilot_pending_requests`
    when:  the last channel client disconnects
    then:  extension receives a `copilot_status status='channel_disconnected'`
           channel-level frame (request_id="" — no specific request match);
           non-stale `_copilot_pending_requests` entries are dropped so a
           future channel reconnect cannot reuse stale ids; stale entries
           are preserved (they belong to meetings that already transitioned
           and have their own filter path)
    """
    bridge = KTalkBridge()

    extension_ws = RecordingClient()
    bridge._connected_clients.add(extension_ws)

    channel_ws = ControllableFakeWebSocket()
    channel_ws.add_message(json.dumps({"type": "copilot_channel_hello"}))

    handler_task = asyncio.create_task(bridge._handle_client(channel_ws))

    for _ in range(50):
        if channel_ws in bridge._copilot_channel_clients:
            break
        await asyncio.sleep(0.01)
    assert channel_ws in bridge._copilot_channel_clients

    # Drain the `channel_connected` primer broadcast that fired on hello.
    extension_ws.sent.clear()

    # Seed bookkeeping as if two recent `copilot_chat_message` frames had
    # been forwarded; one stale from a prior meeting transition.
    bridge._copilot_pending_requests["req-1"] = "meeting-A"
    bridge._copilot_pending_requests["req-2"] = "meeting-A"
    bridge._copilot_pending_requests["req-3-stale"] = "stale:meeting-OLD"

    channel_ws.signal_close()
    await handler_task

    assert "req-1" not in bridge._copilot_pending_requests
    assert "req-2" not in bridge._copilot_pending_requests
    assert "req-3-stale" in bridge._copilot_pending_requests

    assert len(extension_ws.sent) == 1
    payload = json.loads(extension_ws.sent[0])
    assert payload["type"] == "copilot_status"
    assert payload["request_id"] == ""
    assert payload["status"] == "channel_disconnected"

    bridge._connected_clients.discard(extension_ws)


class RecordingControllableFakeWebSocket(ControllableFakeWebSocket):
    """ControllableFakeWebSocket that records outbound `send` payloads.

    Used by tests that need to assert the bridge wrote to the inbound
    socket itself (e.g. the `channel_connected` primer sent from
    `_handle_client` initial setup), not just to other clients in
    `_connected_clients` / `_copilot_channel_clients`.
    """

    def __init__(self):
        super().__init__()
        self.sent: list[str] = []

    async def send(self, message: str):
        self.sent.append(message)


@pytest.mark.asyncio
async def test_copilot_channel_hello_broadcasts_channel_connected_on_first_client():
    """
    given: an extension client connected; no channel clients yet
    when:  a channel client sends `copilot_channel_hello` (transition 0→1)
    then:  the extension receives a `copilot_status status='channel_connected'`
           channel-level frame so the connectivity indicator can flip back
           out of `channel_disconnected`/'unknown' — without this signal
           `state.copilotChannelStatus` has no recovery path
    """
    bridge = KTalkBridge()

    extension_ws = RecordingClient()
    bridge._connected_clients.add(extension_ws)
    assert not bridge._copilot_channel_clients

    channel_ws = ControllableFakeWebSocket()
    channel_ws.add_message(json.dumps({"type": "copilot_channel_hello"}))

    handler_task = asyncio.create_task(bridge._handle_client(channel_ws))

    for _ in range(50):
        if channel_ws in bridge._copilot_channel_clients:
            break
        await asyncio.sleep(0.01)

    try:
        assert channel_ws in bridge._copilot_channel_clients
        assert len(extension_ws.sent) == 1
        payload = json.loads(extension_ws.sent[0])
        assert payload["type"] == "copilot_status"
        assert payload["request_id"] == ""
        assert payload["status"] == "channel_connected"
    finally:
        channel_ws.signal_close()
        await handler_task

    bridge._connected_clients.discard(extension_ws)


@pytest.mark.asyncio
async def test_new_client_receives_channel_connected_primer_when_channel_already_present():
    """
    given: a channel client is already registered when a new client connects
           (e.g. extension reloaded; bridge stayed up; channel never dropped)
    when:  the new client lands in `_handle_client` initial setup
    then:  it receives an immediate `copilot_status status='channel_connected'`
           primer addressed to itself — without this primer, a late-joining
           extension would never learn the channel is up until the next
           `copilot_channel_hello` (which only fires on channel reconnect)
    """
    bridge = KTalkBridge()

    pre_existing_channel = RecordingClient()
    bridge._copilot_channel_clients.add(pre_existing_channel)

    new_ws = RecordingControllableFakeWebSocket()

    handler_task = asyncio.create_task(bridge._handle_client(new_ws))

    for _ in range(50):
        if new_ws.sent:
            break
        await asyncio.sleep(0.01)

    try:
        # A new connection is primed with both the channel_connected status
        # and the current agent_state chip; assert the channel_connected
        # primer is present and well-formed (the agent_state primer has its
        # own dedicated test).
        payloads = [json.loads(m) for m in new_ws.sent]
        channel_primers = [p for p in payloads if p.get("status") == "channel_connected"]
        assert len(channel_primers) == 1
        payload = channel_primers[0]
        assert payload["type"] == "copilot_status"
        assert payload["request_id"] == ""
    finally:
        new_ws.signal_close()
        await handler_task

    bridge._copilot_channel_clients.discard(pre_existing_channel)


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
    # The extension only ever chats from inside a conference meeting, and only
    # conference requests carry a meeting stamp — spell that out so the fixture
    # matches the scenario the docstring describes.
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    bridge.set_meeting_storage(mock_storage)

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)

    extension_ws = RecordingClient()
    bridge._connected_clients.add(extension_ws)

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
           because `_broadcast_copilot_frame` iterates `_connected_clients`
           + `_subscriber_clients`, and `copilot_channel_hello` moved the
           channel client out of both into `_copilot_channel_clients`
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
async def test_copilot_channel_reply_with_non_string_meeting_id_is_ignored(capsys):
    """
    given: an extension client and a tracked pending request
    when:  copilot_channel_reply frames arrive with a non-string, non-None
           meeting_id (int / list / dict)
    then:  nothing is broadcast and the pending entry is NOT popped — the
           frame never entered the reply flow (sibling log-and-return)
    """
    bridge = KTalkBridge()
    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)
    bridge._copilot_pending_requests["req-1"] = "m-current"

    for bad_meeting_id in (123, ["m-1"], {"id": "m-1"}):
        await bridge._process_message(
            {
                "type": "copilot_channel_reply",
                "request_id": "req-1",
                "kind": "manual",
                "text": "x",
                "meeting_id": bad_meeting_id,
            }
        )

    assert extension_client.sent == []
    assert bridge._copilot_pending_requests.get("req-1") == "m-current"
    assert capsys.readouterr().err.count(
        "copilot_channel_reply invalid meeting_id type — ignored"
    ) == 3


@pytest.mark.asyncio
async def test_copilot_channel_reply_with_null_or_absent_meeting_id_is_relayed():
    """
    given: an extension client and a tracked pending request
    when:  copilot_channel_reply frames arrive with explicit
           `meeting_id: null` and with the key absent
    then:  both replies ARE relayed — None/absent stay valid (replies
           generated before any meeting lifecycle must reach the extension)
    """
    bridge = KTalkBridge()
    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)
    payloads = [
        {
            "type": "copilot_channel_reply",
            "request_id": "req-null",
            "kind": "manual",
            "text": "ответ",
            "meeting_id": None,
        },
        {
            "type": "copilot_channel_reply",
            "request_id": "req-absent",
            "kind": "manual",
            "text": "ещё ответ",
        },
    ]
    for payload in payloads:
        bridge._copilot_pending_requests[payload["request_id"]] = ""
        await bridge._process_message(payload)

    relayed = [
        json.loads(m) for m in extension_client.sent
        if json.loads(m)["type"] == "copilot_reply"
    ]
    assert [frame["request_id"] for frame in relayed] == [
        "req-null",
        "req-absent",
    ]


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
    """Build a Mock MeetingStorage shaped for meeting_join/leave dispatch.

    `on_meeting_join` mutates `current_meeting_path` so the bridge sees a
    real path change — without it, the bridge treats the join as a no-op
    reconnect and skips the lifecycle broadcast / tick-state reset.
    """
    storage = Mock()
    storage.current_meeting_path = Path("/tmp/old.jsonl")
    storage.current_meeting_id = meeting_id

    def _on_join(url, *args, **kwargs):
        storage.current_meeting_path = Path(f"/tmp/{meeting_id}.jsonl")
        return meeting_id

    storage.on_meeting_join = Mock(side_effect=_on_join)
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


def _reconnect_storage_mock(meeting_id="m-live"):
    """MeetingStorage mock whose `on_meeting_join` keeps the path unchanged.

    Models a no-op reconnect to the *same* active meeting (KTalk emits these
    during tab focus / socket churn): `current_meeting_path` before and after
    `on_meeting_join` are identical, so the bridge takes the reconnect branch.
    """
    storage = Mock()
    path = Path(f"/tmp/{meeting_id}.jsonl")
    storage.current_meeting_path = path
    storage.current_meeting_id = meeting_id

    def _on_join(url, *args, **kwargs):
        storage.current_meeting_path = path  # unchanged — same meeting
        return meeting_id

    storage.on_meeting_join = Mock(side_effect=_on_join)
    storage.on_ktalk_leave = Mock(return_value="m-next")
    storage.get_current_transcript = Mock(return_value=[])
    storage.get_current_meeting_id = Mock(return_value=meeting_id)
    storage._load_meta = Mock(return_value={"source": "ktalk"})
    return storage


@pytest.mark.asyncio
async def test_meeting_join_reconnect_rearms_nulled_tick_state():
    """
    given: an active meeting whose `_copilot_tick_state` was nulled by a
           prior (possibly spurious) meeting_leave during KTalk join/leave
           churn, then a meeting_join reconnect that does NOT change the
           meeting path
    when:  the bridge processes the `meeting_join` frame
    then:  `_copilot_tick_state` is re-armed for the active meeting so that
           proactive transcript_tick emission resumes — regression guard for
           the bug where tick-state was seeded only on a real path change,
           leaving a reconnected meeting permanently tick-blind
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_reconnect_storage_mock(meeting_id="m-live"))
    bridge._copilot_tick_state = None  # nulled by a prior meeting_leave

    await bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/live",
        "title": "Live interview",
        "platform": "ktalk",
    })

    assert bridge._copilot_tick_state is not None
    assert bridge._copilot_tick_state["meeting_id"] == "m-live"


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


@pytest.mark.asyncio
async def test_meeting_join_transition_marks_pending_requests_stale():
    """
    given: a `copilot_chat_message` was forwarded for meeting "m-prev",
           and the user transitions directly to a new meeting via
           `meeting_join` (no preceding `meeting_leave`)
    when:  a `copilot_channel_reply` arrives carrying the prior request_id
    then:  the reply is dropped — the meeting_join transition must mark
           prior pending requests stale just like meeting_leave does, or
           late replies for the previous meeting leak into the new tab
    """
    storage = Mock()
    storage.current_meeting_path = Path("/tmp/m-prev.jsonl")
    storage.current_meeting_id = "m-prev"
    join_calls = {"count": 0}

    def _on_join(url, *args, **kwargs):
        join_calls["count"] += 1
        # First call: enter "m-prev". Second call: switch to "m-next" by
        # mutating both the path and the current_meeting_id.
        if join_calls["count"] == 1:
            storage.current_meeting_path = Path("/tmp/m-prev.jsonl")
            storage.current_meeting_id = "m-prev"
            return "m-prev"
        storage.current_meeting_path = Path("/tmp/m-next.jsonl")
        storage.current_meeting_id = "m-next"
        return "m-next"

    storage.on_meeting_join = Mock(side_effect=_on_join)
    storage.get_current_transcript = Mock(return_value=[])
    storage.get_current_meeting_id = Mock(side_effect=lambda: storage.current_meeting_id)
    # Both meetings are real ktalk conferences — only conference chat requests
    # carry a meeting stamp and can therefore go stale.
    storage._load_meta = Mock(return_value={"source": "ktalk"})

    bridge = KTalkBridge()
    bridge.set_meeting_storage(storage)

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)
    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    # Step 1: enter the prior meeting and queue a chat request.
    await bridge._process_message({"type": "meeting_join", "url": "https://example.ktalk.ru/m-prev"})
    await bridge._process_message({
        "type": "copilot_chat_message",
        "request_id": "req-prev",
        "text": "вопрос",
    }, websocket=RecordingClient())
    assert bridge._copilot_pending_requests["req-prev"] == "m-prev"

    # Step 2: transition to a new meeting via meeting_join (no leave).
    await bridge._process_message({"type": "meeting_join", "url": "https://example.ktalk.ru/m-next"})
    assert bridge._copilot_pending_requests["req-prev"] == "stale:m-prev"

    # Step 3: late reply for the prior meeting must be dropped.
    extension_client.sent.clear()
    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-prev",
        "kind": "manual",
        "text": "опоздавший ответ",
    })
    relayed = [m for m in extension_client.sent if json.loads(m)["type"] == "copilot_reply"]
    assert relayed == []
    assert "req-prev" not in bridge._copilot_pending_requests


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


# --- Proactive transcript ticks: trigger conditions and suppression ---


def _activate_tick_state(bridge: "KTalkBridge", meeting_id: str = "m-1",
                         meeting_title: str = "Sprint review") -> None:
    """Populate `_copilot_tick_state` as `meeting_join` would.

    Tests bypass the full `meeting_join` flow because exercising the trigger
    only needs the tick counters and a registered channel client.
    """
    bridge._copilot_tick_state = {
        "meeting_id": meeting_id,
        "meeting_title": meeting_title,
        "seg_count": 0,
        "last_seg_count": 0,
        "last_emit_ts": time.time(),
    }


@pytest.mark.asyncio
async def test_transcript_tick_fires_after_n_new_segments():
    """
    given: an active meeting with one channel client and tick state freshly
           reset (last_seg_count=0)
    when:  five `send_transcript` calls land (count trigger threshold)
    then:  exactly one `transcript_tick` is broadcast to the channel client,
           carrying meeting_id, the trailing segments, and a non-empty
           recent_context — the count_trigger short-circuits before the
           time trigger so a sixth call does not fire a second tick
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))
    _activate_tick_state(bridge)

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)

    for sid in range(1, 6):  # 5 new segments → count trigger
        bridge.send_transcript("Alice", f"t{sid}", "10:00:00", segment_id=sid)

    await asyncio.sleep(0.05)

    ticks = [json.loads(m) for m in channel_client.sent if json.loads(m)["type"] == "transcript_tick"]
    assert len(ticks) == 1
    payload = ticks[0]
    assert payload["meeting_id"] == "m-1"
    assert payload["meeting_title"] == "Sprint review"
    assert isinstance(payload["new_segments"], list) and payload["new_segments"]
    assert payload["recent_context"]
    assert "local_user_name" in payload


@pytest.mark.asyncio
async def test_transcript_tick_fires_on_time_trigger():
    """
    given: an active meeting with one channel client and tick state where
           `last_emit_ts` is already older than the time threshold
           (`COPILOT_TICK_INTERVAL_SECONDS`)
    when:  a single `send_transcript` call lands (well below the count
           threshold of 5)
    then:  a `transcript_tick` still fires — the time trigger fires
           independently of the count trigger
    """
    import ktalk_bridge as bridge_module

    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))
    _activate_tick_state(bridge)

    # Push last_emit_ts comfortably past the interval so the next call
    # crosses the time trigger; last_seg_count stays at 0 so the lone new
    # segment is included in the tick payload.
    bridge._copilot_tick_state["last_emit_ts"] = (
        time.time() - bridge_module.COPILOT_TICK_INTERVAL_SECONDS - 1.0
    )

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)

    bridge.send_transcript("Alice", "одна реплика", "10:00:00", segment_id=1)
    await asyncio.sleep(0.05)

    ticks = [json.loads(m) for m in channel_client.sent if json.loads(m)["type"] == "transcript_tick"]
    assert len(ticks) == 1
    assert ticks[0]["meeting_id"] == "m-1"
    assert [s["segment_id"] for s in ticks[0]["new_segments"]] == [1]


@pytest.mark.asyncio
async def test_transcript_tick_suppressed_without_active_meeting():
    """
    given: a bridge with a registered channel client but no active meeting
           (tick_state stays None)
    when:  enough `send_transcript` calls to cross the count trigger arrive
    then:  no `transcript_tick` is broadcast — the bridge cannot describe a
           meeting that has not started, so the proactive path stays silent
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))
    assert bridge._copilot_tick_state is None

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)

    for sid in range(1, 7):
        bridge.send_transcript("Alice", f"t{sid}", "10:00:00", segment_id=sid)
    await asyncio.sleep(0.05)

    assert all(json.loads(m)["type"] != "transcript_tick" for m in channel_client.sent)


@pytest.mark.asyncio
async def test_transcript_tick_suppressed_without_channel_clients():
    """
    given: an active meeting (tick_state populated) but no channel clients
    when:  `send_transcript` calls cross the count trigger
    then:  no tick is constructed and `last_seg_count` stays at 0 — the
           formatter is gated on subscriber presence to avoid building
           snapshots no one will read
    """
    bridge = KTalkBridge()
    bridge._loop = asyncio.get_event_loop()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))
    _activate_tick_state(bridge)
    seeded_emit_ts = bridge._copilot_tick_state["last_emit_ts"]

    assert not bridge._copilot_channel_clients

    for sid in range(1, 7):
        bridge.send_transcript("Alice", f"t{sid}", "10:00:00", segment_id=sid)
    await asyncio.sleep(0.05)

    # Counters stay at the seeded baseline because the formatter exits
    # before mutating them when no channel clients are registered.
    assert bridge._copilot_tick_state["last_seg_count"] == 0
    assert bridge._copilot_tick_state["last_emit_ts"] == seeded_emit_ts


@pytest.mark.asyncio
async def test_meeting_join_resets_tick_state_and_meeting_leave_clears_it():
    """
    given: a bridge with `meeting_storage` ready to accept join/leave
    when:  meeting_join lands then meeting_leave lands
    then:  tick_state is populated on join (with the new meeting_id and
           a zero baseline) and torn down on leave so a stray
           `send_transcript` between meetings cannot emit a tick
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_meeting_storage_mock(meeting_id="m-now", new_meeting_id="m-next"))

    await bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/abc",
        "title": "Daily",
        "platform": "ktalk",
    })
    assert bridge._copilot_tick_state is not None
    assert bridge._copilot_tick_state["meeting_id"] == "m-now"
    assert bridge._copilot_tick_state["seg_count"] == 0
    assert bridge._copilot_tick_state["last_seg_count"] == 0

    await bridge._process_message({
        "type": "meeting_leave",
        "reason": "user_left",
    })
    assert bridge._copilot_tick_state is None


@pytest.mark.asyncio
async def test_meeting_leave_clears_recent_transcript_buffer():
    """
    given: a recent-transcript buffer populated during the active meeting
    when:  meeting_leave lands
    then:  the buffer and its insertion-order deque are emptied so a late
           refine for this meeting cannot rewrite an entry that the next
           meeting would otherwise occupy at the same segment_id
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_meeting_storage_mock(meeting_id="m-prev", new_meeting_id="m-next"))

    bridge.send_transcript("Alice", "первая", "10:00:00", segment_id=1)
    bridge.send_transcript("Bob", "вторая", "10:00:01", segment_id=2)
    assert len(bridge._copilot_transcript_buffer) == 2
    assert list(bridge._copilot_transcript_order) == [1, 2]

    await bridge._process_message({"type": "meeting_leave", "reason": "user_left"})

    assert bridge._copilot_transcript_buffer == {}
    assert list(bridge._copilot_transcript_order) == []


@pytest.mark.asyncio
async def test_meeting_join_path_change_clears_prior_buffer():
    """
    given: a recent-transcript buffer populated for the prior meeting and
           a fresh meeting_join whose `current_meeting_path` differs
    when:  meeting_join lands
    then:  the prior buffer is cleared so the new meeting starts with a
           clean recent-transcript history; without this clear, segment_id
           collisions across meetings (ids restart at 1 per meeting) would
           leave the deque tail pointing at prior-meeting entries
    """
    bridge = KTalkBridge()
    bridge._loop = None
    storage = _meeting_storage_mock(meeting_id="m-current", new_meeting_id="m-next")
    bridge.set_meeting_storage(storage)

    # Populate with prior-meeting bookkeeping.
    bridge._copilot_transcript_buffer[42] = {
        "segment_id": 42,
        "speaker": "Alice",
        "text": "stale",
        "start_ms": time.time() * 1000,
        "end_ms": time.time() * 1000,
        "refined": False,
        "meeting_id": "m-prior",
    }
    bridge._copilot_transcript_order.append(42)

    await bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/new",
        "title": "Sprint review",
        "platform": "ktalk",
    })

    assert bridge._copilot_transcript_buffer == {}
    assert list(bridge._copilot_transcript_order) == []


@pytest.mark.asyncio
async def test_copilot_pending_request_dropped_after_successful_reply():
    """
    given: a pending request_id recorded for the active meeting and a
           non-stale `copilot_channel_reply` arrives carrying that id
    when:  the bridge relays the reply to extension clients
    then:  the bookkeeping entry is popped so the dict cannot grow
           unboundedly across many chat turns
    """
    bridge = KTalkBridge()
    bridge._copilot_pending_requests["req-alive"] = "m-current"

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-alive",
        "kind": "manual",
        "text": "ответ",
    })

    relayed = [json.loads(m) for m in extension_client.sent if json.loads(m)["type"] == "copilot_reply"]
    assert len(relayed) == 1
    assert "req-alive" not in bridge._copilot_pending_requests


@pytest.mark.asyncio
async def test_copilot_pending_request_dropped_after_terminal_status():
    """
    given: a pending request_id recorded for the active meeting
    when:  a terminal `copilot_channel_status` (status="done"/"error") arrives
    then:  the bookkeeping entry is popped — the channel server is finished
           with this request even if no separate reply lands
    """
    bridge = KTalkBridge()
    bridge._copilot_pending_requests["req-done"] = "m-current"
    bridge._copilot_pending_requests["req-err"] = "m-current"

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message({
        "type": "copilot_channel_status",
        "request_id": "req-done",
        "status": "done",
    })
    await bridge._process_message({
        "type": "copilot_channel_status",
        "request_id": "req-err",
        "status": "error",
    })

    assert "req-done" not in bridge._copilot_pending_requests
    assert "req-err" not in bridge._copilot_pending_requests


@pytest.mark.asyncio
async def test_copilot_pending_request_kept_through_non_terminal_status():
    """
    given: a pending request_id recorded for the active meeting
    when:  a non-terminal `copilot_channel_status` (queued/working) arrives
    then:  the entry stays — only a reply or a terminal status pops it
    """
    bridge = KTalkBridge()
    bridge._copilot_pending_requests["req-working"] = "m-current"

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    for status in ("queued", "working"):
        await bridge._process_message({
            "type": "copilot_channel_status",
            "request_id": "req-working",
            "status": status,
        })

    assert bridge._copilot_pending_requests.get("req-working") == "m-current"


def test_send_refine_with_meeting_id_skips_cross_meeting_entries():
    """
    given: a buffer entry tagged with meeting_id="m-current"
    when:  send_refine runs with meeting_id="m-other" carrying an update
           for the same segment_id
    then:  the entry is NOT mutated — the refine belongs to a different
           meeting whose ids merely happen to collide (ids restart at 1
           per meeting), so cross-meeting writes are silently dropped
    """
    bridge = KTalkBridge()
    bridge._loop = None
    bridge.set_meeting_storage(_make_storage_for_meeting("m-current"))

    bridge.send_transcript("Alice", "оригинал", "10:00:00", segment_id=1)
    original_text = bridge._copilot_transcript_buffer[1]["text"]

    bridge.send_refine(
        [{"segment_id": 1, "text": "из другой встречи"}],
        meeting_id="m-other",
    )

    entry = bridge._copilot_transcript_buffer[1]
    assert entry["text"] == original_text
    assert entry["refined"] is False


# --- _end_meeting_on_last_extension_gone: copilot cleanup parity with meeting_leave ---


@pytest.mark.asyncio
async def test_extension_disconnect_cleans_up_copilot_state():
    """
    given: an active ktalk meeting with copilot state populated — a
           pending request_id mapped to the live meeting, an active
           tick-state, a recent-transcript buffer entry, and one
           registered copilot channel client
    when:  the only extension client disconnects (no explicit
           meeting_leave frame ever arrives — a page reload / crash /
           network drop)
    then:  the same Copilot cleanup that meeting_leave performs runs:
           the pending request is marked stale, the tick-state is
           cleared, the transcript buffer is emptied, and a
           `meeting_ended` frame reaches the channel client. Without
           this, a late `copilot_channel_reply` for the prior meeting
           would leak into the next session.
    """
    bridge = KTalkBridge()

    mock_storage = Mock()
    mock_storage.current_meeting_path = Path("/tmp/m-prev.jsonl")
    mock_storage.current_meeting_id = "m-prev"
    mock_storage._load_meta = Mock(return_value={"source": "ktalk"})
    mock_storage.on_ktalk_leave = Mock(return_value="m-next")
    mock_storage.get_current_meeting_id = Mock(return_value="m-prev")
    bridge.set_meeting_storage(mock_storage)

    bridge._copilot_pending_requests["req-alive"] = "m-prev"
    bridge._copilot_tick_state = {
        "meeting_id": "m-prev",
        "meeting_title": "Sprint review",
        "seg_count": 0,
        "last_seg_count": 0,
        "last_emit_ts": time.time(),
    }
    bridge._copilot_transcript_buffer[7] = {
        "segment_id": 7,
        "speaker": "Alice",
        "text": "first",
        "start_ms": time.time() * 1000,
        "end_ms": time.time() * 1000,
        "refined": False,
        "meeting_id": "m-prev",
    }
    bridge._copilot_transcript_order.append(7)

    channel_client = RecordingClient()
    bridge._copilot_channel_clients.add(channel_client)

    fake_ws = FakeWebSocket()
    await bridge._handle_client(fake_ws)

    assert bridge._copilot_pending_requests["req-alive"] == "stale:m-prev"
    assert bridge._copilot_tick_state is None
    assert bridge._copilot_transcript_buffer == {}
    assert list(bridge._copilot_transcript_order) == []

    ended = [
        json.loads(m) for m in channel_client.sent
        if json.loads(m).get("type") == "meeting_ended"
    ]
    assert len(ended) == 1
    assert ended[0]["meeting_id"] == "m-prev"


# --- copilot_channel_reply: cross-meeting filter for proactive replies ---


@pytest.mark.asyncio
async def test_proactive_reply_for_different_meeting_is_dropped():
    """
    given: bridge's current meeting is "m-now" and an extension client is
           connected. Proactive replies use a synthetic
           `proactive_<uuid>` request_id that was never tracked in
           `_copilot_pending_requests`, so the request_id stale check
           cannot drop them — only the meeting_id cross-check can.
    when:  a `copilot_channel_reply` arrives carrying that synthetic
           id and `meeting_id="m-old"` (the meeting that produced the
           originating `transcript_tick` already ended)
    then:  the reply is NOT relayed — the channel server lagged and
           Claude's proactive note belongs to a meeting the user has
           already left
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-now"))

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "proactive_abc123",
        "kind": "proactive",
        "text": "вопрос Славе про CI",
        "meeting_id": "m-old",
    })

    relayed = [
        m for m in extension_client.sent
        if json.loads(m).get("type") == "copilot_reply"
    ]
    assert relayed == []


@pytest.mark.asyncio
async def test_proactive_reply_for_current_meeting_is_relayed():
    """
    given: bridge's current meeting is "m-now"
    when:  a proactive `copilot_channel_reply` arrives carrying a
           synthetic request_id and matching `meeting_id="m-now"`
    then:  the reply IS relayed — the meeting_id filter only fires on
           a mismatch
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-now"))

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "proactive_abc123",
        "kind": "proactive",
        "text": "вопрос Славе про CI",
        "meeting_id": "m-now",
    })

    relayed = [
        json.loads(m) for m in extension_client.sent
        if json.loads(m).get("type") == "copilot_reply"
    ]
    assert len(relayed) == 1
    assert relayed[0]["meeting_id"] == "m-now"
    assert relayed[0]["kind"] == "proactive"


@pytest.mark.asyncio
async def test_reply_without_meeting_id_falls_through_filter():
    """
    given: bridge's current meeting is "m-now"; a reply arrives without
           a `meeting_id` field (older channel server, or pre-meeting
           manual send)
    when:  the bridge processes the reply
    then:  the reply IS relayed — the meeting_id filter is best-effort
           and only drops on a definite mismatch, never on a missing id
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-now"))

    extension_client = RecordingClient()
    bridge._connected_clients.add(extension_client)
    bridge._copilot_pending_requests["req-known"] = "m-now"

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-known",
        "kind": "manual",
        "text": "ответ без meeting_id",
    })

    relayed = [
        json.loads(m) for m in extension_client.sent
        if json.loads(m).get("type") == "copilot_reply"
    ]
    assert len(relayed) == 1
    assert relayed[0]["request_id"] == "req-known"


# --- _end_meeting_on_last_extension_gone: defensive _load_meta error path ---


@pytest.mark.asyncio
async def test_extension_disconnect_swallows_load_meta_error():
    """
    given: an active meeting whose `_load_meta()` raises (transient I/O,
           a JSON-decode error on a torn file, or an OSError) — the same
           failure mode that `_has_active_ktalk_meeting` already catches
           on its `pcm_chunk` guard at line 462
    when:  the only extension client disconnects
    then:  the cleanup must NOT propagate the exception out of the
           handler — without the guard, an asyncio task would crash and
           any later code in the finally-block would be skipped. The
           function returns early, leaving Copilot state untouched.
    """
    bridge = KTalkBridge()

    mock_storage = Mock()
    mock_storage.current_meeting_path = Path("/tmp/m-prev.jsonl")
    mock_storage.current_meeting_id = "m-prev"
    mock_storage._load_meta = Mock(side_effect=OSError("disk gone"))
    bridge.set_meeting_storage(mock_storage)

    bridge._copilot_pending_requests["req-x"] = "m-prev"

    fake_ws = FakeWebSocket()
    # Must not raise — the disconnect cleanup is awaited in the
    # finally-block of `_handle_client` and an unhandled OSError would
    # bubble out and abort the entire connection task.
    await bridge._handle_client(fake_ws)

    # Cleanup did not run past the load_meta gate, so meeting bookkeeping
    # is unchanged. The next meeting_join will overwrite this entry; we
    # only care that the handler did not crash.
    assert bridge._copilot_pending_requests.get("req-x") == "m-prev"


# --- get_recent_transcript_request: snapshot stability vs refine writes ---


@pytest.mark.asyncio
async def test_get_recent_transcript_snapshots_decouple_from_buffer():
    """
    given: a buffered transcript entry. The refine worker runs on a
           separate thread and mutates entries in-place via
           `_apply_copilot_transcript_refine`. The snapshot path must
           build copies under `_lock` so a refine that fires after the
           lock release cannot retro-actively mutate fields the filter
           loop is reading.
    when:  the snapshot is taken, then the buffer entry is mutated
           in-place after the snapshot returns
    then:  the response segments are independent of subsequent buffer
           mutations — proving each segment is a snapshot copy, not a
           live reference into `_copilot_transcript_buffer`.
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-1"))

    now_ms = time.time() * 1000
    bridge._copilot_transcript_buffer[1] = {
        "segment_id": 1,
        "speaker": "Alice",
        "text": "live text",
        "start_ms": now_ms,
        "end_ms": now_ms,
        "refined": False,
        "meeting_id": "m-1",
    }
    bridge._copilot_transcript_order.append(1)

    channel_client = RecordingClient()
    await bridge._process_message(
        {
            "type": "get_recent_transcript_request",
            "request_id": "req-snap",
            "window_seconds": 600,
        },
        websocket=channel_client,
    )

    # Now mutate the buffer entry as a refine worker would. If the
    # snapshot held a reference (the pre-fix behaviour), we'd already
    # have leaked the mutation into `segments` via `dict(entry)` racing
    # mid-loop — but more importantly, callers parsing the JSON later
    # could observe inconsistent state. The post-fix snapshot must be
    # fully decoupled from the live buffer.
    bridge._copilot_transcript_buffer[1]["text"] = "refined text"
    bridge._copilot_transcript_buffer[1]["refined"] = True

    responses = [
        json.loads(m) for m in channel_client.sent
        if json.loads(m).get("type") == "get_recent_transcript_response"
    ]
    assert len(responses) == 1
    segs = responses[0]["segments"]
    assert len(segs) == 1
    assert segs[0]["text"] == "live text"
    assert segs[0]["refined"] is False


# --- Task 4: copilot supervisor — hello / meeting_state / lifecycle / merge ---


async def _serve_bridge(bridge):
    """Serve a real KTalkBridge on an ephemeral port; return (server, url)."""
    server = await websockets.serve(bridge._handle_client, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    return server, f"ws://127.0.0.1:{port}"


async def _recv_type(ws, want, timeout=5.0):
    """Read frames from `ws` until one of type `want` arrives (or timeout)."""
    while True:
        raw = await asyncio.wait_for(ws.recv(), timeout=timeout)
        frame = json.loads(raw)
        if frame.get("type") == want:
            return frame


@pytest.mark.asyncio
async def test_real_bridge_delivers_meeting_started_to_supervisor():
    """
    given: a REAL bridge WS server, a registered supervisor client
           (`supervisor_hello`), and no active meeting at connect time
    when:  an extension client joins a KTalk meeting
    then:  the supervisor receives `meeting_started` for the new meeting_id
           through the real WS fan-out — guards against a fake-bridge unit
           test masking a missing supervisor emit path (meeting_join)
    """
    bridge = KTalkBridge()
    storage = _meeting_storage_mock(meeting_id="m-join")
    storage.current_meeting_path = None  # no active meeting at connect
    bridge.set_meeting_storage(storage)

    server, url = await _serve_bridge(bridge)
    supervisor = extension = None
    try:
        supervisor = await websockets.connect(url)
        await supervisor.send(json.dumps({"type": "supervisor_hello"}))
        state = await _recv_type(supervisor, "meeting_state")
        assert state["active"] is False

        extension = await websockets.connect(url)
        await extension.send(json.dumps({
            "type": "meeting_join",
            "url": "https://ktalk.test/abc",
            "title": "Sprint review",
            "platform": "ktalk",
        }))
        started = await _recv_type(supervisor, "meeting_started")
        assert started["meeting_id"] == "m-join"
        assert started["meeting_title"] == "Sprint review"
    finally:
        if extension is not None:
            await extension.close()
        if supervisor is not None:
            await supervisor.close()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_real_bridge_delivers_meeting_ended_on_leave_to_supervisor():
    """
    given: a REAL bridge, a registered supervisor, and an active meeting
    when:  the extension sends an explicit `meeting_leave`
    then:  the supervisor receives `meeting_ended` for the previous meeting_id
           (second of the three emit paths)
    """
    bridge = KTalkBridge()
    storage = _meeting_storage_mock(meeting_id="m-prev", new_meeting_id="m-next")
    storage.current_meeting_path = None
    bridge.set_meeting_storage(storage)

    server, url = await _serve_bridge(bridge)
    supervisor = extension = None
    try:
        supervisor = await websockets.connect(url)
        await supervisor.send(json.dumps({"type": "supervisor_hello"}))
        await _recv_type(supervisor, "meeting_state")

        extension = await websockets.connect(url)
        await extension.send(json.dumps({
            "type": "meeting_join",
            "url": "https://ktalk.test/abc",
            "title": "Standup",
            "platform": "ktalk",
        }))
        await _recv_type(supervisor, "meeting_started")

        await extension.send(json.dumps({"type": "meeting_leave", "reason": "user_left"}))
        ended = await _recv_type(supervisor, "meeting_ended")
        assert ended["meeting_id"] == "m-prev"
    finally:
        if extension is not None:
            await extension.close()
        if supervisor is not None:
            await supervisor.close()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_real_bridge_delivers_meeting_ended_on_last_extension_gone_to_supervisor():
    """
    given: a REAL bridge, a registered supervisor, and an active meeting
    when:  the only extension client abruptly disconnects (no meeting_leave)
    then:  the supervisor receives `meeting_ended` via the
           last-extension-gone path — the third emit path, and proof that
           `supervisor_hello` isolates the supervisor from the extension
           count (else `no_extension_clients` would be False and no end fires)
    """
    bridge = KTalkBridge()
    storage = _meeting_storage_mock(meeting_id="m-live")
    storage.current_meeting_path = None
    bridge.set_meeting_storage(storage)

    server, url = await _serve_bridge(bridge)
    supervisor = extension = None
    try:
        supervisor = await websockets.connect(url)
        await supervisor.send(json.dumps({"type": "supervisor_hello"}))
        await _recv_type(supervisor, "meeting_state")

        extension = await websockets.connect(url)
        await extension.send(json.dumps({
            "type": "meeting_join",
            "url": "https://ktalk.test/abc",
            "title": "Standup",
            "platform": "ktalk",
        }))
        await _recv_type(supervisor, "meeting_started")

        await extension.close()  # abrupt disconnect — page crash / network drop
        extension = None
        ended = await _recv_type(supervisor, "meeting_ended")
        assert ended["meeting_id"] == "m-live"
    finally:
        if extension is not None:
            await extension.close()
        if supervisor is not None:
            await supervisor.close()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_orphan_reuse_join_emits_meeting_started_to_supervisor():
    """
    given: a REAL bridge, a registered supervisor, and an empty orphan
           meeting current at join time (created by a prior leave);
           on_meeting_join reuses it in place — source flips orphan→ktalk,
           current_meeting_path does NOT change (case 3 in meeting_storage)
    when:  an extension joins a KTalk room
    then:  the supervisor still receives `meeting_started` — the
           path-change guard alone must not swallow the spawn trigger
    """
    bridge = KTalkBridge()
    storage = _meeting_storage_mock(meeting_id="m-orphan-reuse")
    # Case-3 shape: the orphan dir already IS the meeting dir, so the
    # default _on_join side effect sets the path to the same value —
    # path unchanged across the join. Pre-join meta says "orphan".
    storage.current_meeting_path = Path("/tmp/m-orphan-reuse.jsonl")
    storage._load_meta = Mock(return_value={"source": "orphan"})
    bridge.set_meeting_storage(storage)

    server, url = await _serve_bridge(bridge)
    supervisor = extension = None
    try:
        supervisor = await websockets.connect(url)
        await supervisor.send(json.dumps({"type": "supervisor_hello"}))
        await _recv_type(supervisor, "meeting_state")

        extension = await websockets.connect(url)
        await extension.send(json.dumps({
            "type": "meeting_join",
            "url": "https://ktalk.test/room-after-leave",
            "title": "Обед — Толк",
            "platform": "ktalk",
        }))
        started = await _recv_type(supervisor, "meeting_started")
        assert started["meeting_id"] == "m-orphan-reuse"
    finally:
        if extension is not None:
            await extension.close()
        if supervisor is not None:
            await supervisor.close()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_same_path_ktalk_reconnect_does_not_duplicate_meeting_started():
    """
    given: a REAL bridge, a registered supervisor, and an active ktalk
           meeting (source already "ktalk")
    when:  the extension re-sends meeting_join for the same room (no-op
           reconnect: same live_id, current_meeting_path unchanged)
    then:  the supervisor receives NO second `meeting_started` — the
           orphan-reuse fix must not break reconnect dedup
    """
    bridge = KTalkBridge()
    storage = _meeting_storage_mock(meeting_id="m-reconnect")
    # Meeting already active: path is the meeting dir, source is ktalk.
    storage.current_meeting_path = Path("/tmp/m-reconnect.jsonl")
    storage._load_meta = Mock(return_value={"source": "ktalk"})
    bridge.set_meeting_storage(storage)

    server, url = await _serve_bridge(bridge)
    supervisor = extension = None
    try:
        supervisor = await websockets.connect(url)
        await supervisor.send(json.dumps({"type": "supervisor_hello"}))
        await _recv_type(supervisor, "meeting_state")

        extension = await websockets.connect(url)
        await extension.send(json.dumps({
            "type": "meeting_join",
            "url": "https://ktalk.test/same-room",
            "title": "Standup",
            "platform": "ktalk",
        }))
        with pytest.raises(asyncio.TimeoutError):
            await _recv_type(supervisor, "meeting_started", timeout=1.0)
    finally:
        if extension is not None:
            await extension.close()
        if supervisor is not None:
            await supervisor.close()
        server.close()
        await server.wait_closed()


def _ktalk_storage(meeting_id="m-1", title="Standup", source="ktalk"):
    storage = Mock()
    storage.current_meeting_path = Path(f"/tmp/{meeting_id}.jsonl")
    storage.current_meeting_id = meeting_id
    storage._load_meta = Mock(return_value={"source": source, "title": title})
    return storage


def test_meeting_state_active_true_for_ktalk_with_extension():
    """
    given: a ktalk meeting in storage and one connected extension client
    when:  the bridge builds a meeting_state reply
    then:  active is True with the meeting_id and title populated
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1", title="Standup"))
    bridge._connected_clients.add(RecordingClient())

    frame = bridge._compute_meeting_state()

    assert frame["type"] == "meeting_state"
    assert frame["active"] is True
    assert frame["meeting_id"] == "m-1"
    assert frame["meeting_title"] == "Standup"


def test_meeting_state_orphan_after_leave_is_inactive():
    """
    given: an orphan meeting (post-leave: current_meeting_path is never None)
           with a connected extension client
    when:  the bridge builds a meeting_state reply
    then:  active is False and no meeting_id leaks — orphan is not "in a
           meeting", so the supervisor must not treat it as one
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="orphan-1", source="orphan"))
    bridge._connected_clients.add(RecordingClient())

    frame = bridge._compute_meeting_state()

    assert frame["active"] is False
    assert frame["meeting_id"] is None
    assert frame["meeting_title"] is None


def test_meeting_state_ktalk_without_extension_is_inactive():
    """
    given: a ktalk meeting in storage but NO connected extension client
    when:  the bridge builds a meeting_state reply
    then:  active is False — a ktalk meta without a live extension driving it
           is a stale record, not an active meeting
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1"))
    # no extension clients

    frame = bridge._compute_meeting_state()

    assert frame["active"] is False
    assert frame["meeting_id"] is None


# --- conference-source predicate: matrix / vas3k / vkkovalev parity with ktalk ---


def test_is_conference_source_covers_all_four_platforms():
    """
    given: the four extension-driven conference platforms and the two
           non-conference sources (orphan / local)
    when:  is_conference_source is queried
    then:  ktalk + vas3k + vkkovalev + matrix are conferences; orphan / local
           / None are not — this is the single predicate that widens the
           previously ktalk-only lifecycle gates to all conference platforms
    """
    from meeting_storage import is_conference_source

    for source in ("ktalk", "vas3k", "vkkovalev", "matrix"):
        assert is_conference_source(source) is True
    for source in ("orphan", "local", None, "", "unknown"):
        assert is_conference_source(source) is False


@pytest.mark.parametrize("source", ["matrix", "vas3k", "vkkovalev"])
def test_meeting_state_active_true_for_conference_source_with_extension(source):
    """
    given: a meeting opened by a conference platform other than ktalk
           (matrix / vas3k / vkkovalev) and one connected extension client
    when:  the bridge builds a meeting_state reply
    then:  active is True with meeting_id/title populated — the supervisor
           reconcile now adopts these meetings the same way it adopts ktalk
           (previously only source=="ktalk" reported active, so a bridge
           restart mid-matrix-meeting would kill the copilot)
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1", title="Standup", source=source))
    bridge._connected_clients.add(RecordingClient())

    frame = bridge._compute_meeting_state()

    assert frame["active"] is True
    assert frame["meeting_id"] == "m-1"
    assert frame["meeting_title"] == "Standup"


def test_meeting_state_local_source_is_inactive():
    """
    given: a `local` (non-conference) meeting with a connected extension client
    when:  the bridge builds a meeting_state reply
    then:  active is False — widening the gate to conference sources must NOT
           make plain local recordings look like a live conference
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1", source="local"))
    bridge._connected_clients.add(RecordingClient())

    frame = bridge._compute_meeting_state()

    assert frame["active"] is False
    assert frame["meeting_id"] is None


@pytest.mark.asyncio
@pytest.mark.parametrize("source", ["matrix", "vas3k", "vkkovalev"])
async def test_extension_disconnect_ends_conference_meeting_and_generates_increment(source):
    """
    given: an active conference meeting (matrix / vas3k / vkkovalev) with an
           increment generator wired and the only extension client connected
    when:  that last extension client disconnects (page reload / crash — no
           explicit meeting_leave frame arrives)
    then:  the meeting is ended via on_ktalk_leave("disconnected") AND an
           increment is generated for the meeting just left — the same
           end-on-last-extension-gone flow previously reserved for ktalk now
           finishes wiring the half-connected conference platforms
    """
    bridge = KTalkBridge()

    mock_storage = Mock()
    mock_storage.current_meeting_path = Path("/tmp/m-prev.jsonl")
    mock_storage.current_meeting_id = "m-prev"
    mock_storage._load_meta = Mock(return_value={"source": source})
    mock_storage.on_ktalk_leave = Mock(return_value="m-next")
    bridge.set_meeting_storage(mock_storage)

    increment_gen = Mock()
    increment_gen.generate_all = AsyncMock()
    bridge.set_increment_generator(increment_gen)

    fake_ws = FakeWebSocket()
    await bridge._handle_client(fake_ws)
    await bridge._drain_background_tasks()

    mock_storage.on_ktalk_leave.assert_called_once_with("disconnected")
    increment_gen.generate_all.assert_awaited_once_with("m-prev", Path("/tmp/m-prev.jsonl"))


@pytest.mark.asyncio
async def test_local_meeting_not_ended_on_disconnect():
    """
    given: a `local` (non-conference) meeting and one extension client
    when:  the last extension client disconnects
    then:  on_ktalk_leave is NOT called — only conference meetings end on the
           last extension leaving; plain local recordings keep running
    """
    bridge = KTalkBridge()

    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "local"})
    mock_storage.on_ktalk_leave = Mock()
    bridge.set_meeting_storage(mock_storage)

    fake_ws = FakeWebSocket()

    await bridge._handle_client(fake_ws)

    mock_storage.on_ktalk_leave.assert_not_called()


def _bridge_with_active_ktalk():
    """Bridge with an active ktalk meeting + one extension client (recorder)."""
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1"))
    ext = RecordingClient()
    bridge._connected_clients.add(ext)
    return bridge, ext


def _agent_states(client):
    """Extract the sequence of broadcast agent_state values from a client."""
    out = []
    for m in client.sent:
        f = json.loads(m)
        if f.get("type") == "copilot_status" and f.get("status") == "agent_state":
            out.append(f["agent_state"])
    return out


@pytest.mark.asyncio
async def test_merge_working_with_one_session_is_working():
    """
    given: supervisor reports `working` and the daemon reports 1 live session
    when:  the effective agent state is recomputed
    then:  the chip broadcasts `working` (supervisor status passes through)
    """
    bridge, ext = _bridge_with_active_ktalk()
    bridge._supervisor_agent_status = "working"
    bridge._supervisor_status_ts = 50.0
    bridge._set_claude_sessions(1)

    await bridge._recompute_agent_state(now=50.0)

    assert _agent_states(ext) == ["working"]


@pytest.mark.asyncio
async def test_merge_online_with_sessions_zero_sustained_goes_offline():
    """
    given: supervisor stays `online` (fresh heartbeats) but the daemon reports
           zero live sessions during an active meeting
    when:  the zero-session condition holds past the debounce window
    then:  the chip flips online -> offline (TUI up, copilot never connected),
           and stays online while the condition is younger than the debounce
    """
    bridge, ext = _bridge_with_active_ktalk()
    bridge._supervisor_agent_status = "online"
    bridge._supervisor_status_ts = 100.0
    bridge._set_claude_sessions(0)

    await bridge._recompute_agent_state(now=100.0)   # zero just started → online
    bridge._supervisor_status_ts = 105.0             # heartbeat keeps supervisor fresh
    await bridge._recompute_agent_state(now=105.0)   # 5s < debounce → still online
    bridge._supervisor_status_ts = 111.0
    await bridge._recompute_agent_state(now=111.0)   # 11s ≥ debounce → offline

    assert _agent_states(ext) == ["online", "offline"]


@pytest.mark.asyncio
async def test_merge_supervisor_silence_goes_offline():
    """
    given: supervisor reported `online`, then falls silent (no heartbeat)
    when:  more than two heartbeat intervals elapse without a report
    then:  the chip flips online -> offline on the silence threshold
    """
    bridge, ext = _bridge_with_active_ktalk()
    bridge._supervisor_agent_status = "online"
    bridge._supervisor_status_ts = 10.0

    await bridge._recompute_agent_state(now=15.0)   # 5s silence → online
    await bridge._recompute_agent_state(now=25.0)   # 15s silence → offline

    assert _agent_states(ext) == ["online", "offline"]


@pytest.mark.asyncio
async def test_merge_spawning_to_online_with_young_sessions_zero_does_not_flap():
    """
    given: supervisor is `spawning` with zero sessions, then flips to `online`
           while the daemon has not yet reported any session (still 0, young)
    when:  the effective state is recomputed on the transition
    then:  the chip goes spawning -> online WITHOUT a spurious offline —
           the sessions-zero debounce absorbs the startup gap (no flap)
    """
    bridge, ext = _bridge_with_active_ktalk()
    bridge._supervisor_agent_status = "spawning"
    bridge._supervisor_status_ts = 200.0
    bridge._set_claude_sessions(0)

    await bridge._recompute_agent_state(now=200.0)   # spawning (zero branch needs !=spawning)
    bridge._supervisor_agent_status = "online"
    bridge._supervisor_status_ts = 202.0
    await bridge._recompute_agent_state(now=202.0)   # sessions 0 for 2s < debounce → online

    assert _agent_states(ext) == ["spawning", "online"]


@pytest.mark.asyncio
async def test_merge_stale_sessions_zero_anchor_does_not_flap_at_meeting_start():
    """
    given: the always-on daemon reported 0 sessions at its own startup (long
           before any meeting), then a meeting starts and the supervisor goes
           spawning -> online while the daemon has not yet polled the new MCP
           session (count still cached 0)
    when:  the effective state is recomputed across the transition
    then:  the chip goes spawning -> online WITHOUT a spurious offline — the
           debounce is anchored to the online transition, not the stale
           startup-time zero (regression: a startup-anchored clock is already
           past the debounce and would flap the chip offline every meeting)
    """
    bridge, ext = _bridge_with_active_ktalk()
    bridge._set_claude_sessions(0)                 # daemon startup poll, pre-meeting
    bridge._sessions_zero_since = 0.0              # stale anchor from that early 0

    bridge._supervisor_agent_status = "spawning"
    bridge._supervisor_status_ts = 100.0
    await bridge._recompute_agent_state(now=100.0)  # spawning clears the stale anchor

    bridge._supervisor_agent_status = "online"
    bridge._supervisor_status_ts = 103.0
    await bridge._recompute_agent_state(now=103.0)  # anchor re-armed at 103 → online
    bridge._supervisor_status_ts = 108.0
    await bridge._recompute_agent_state(now=108.0)  # 5s < debounce → still online

    assert _agent_states(ext) == ["spawning", "online"]


@pytest.mark.asyncio
async def test_merge_sessions_zero_without_active_meeting_does_not_force_offline():
    """
    given: the supervisor reports `online` and the daemon reports a sustained
           confident 0 live sessions, but no KTalk meeting is active (an orphan
           meeting — the copilot legitimately has no MCP session)
    when:  the effective state is recomputed well past the debounce window
    then:  the chip stays `online` — the sessions==0 -> offline verdict is
           gated on an active meeting and must not fire outside one
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1", source="orphan"))
    ext = RecordingClient()
    bridge._connected_clients.add(ext)
    bridge._supervisor_agent_status = "online"
    bridge._supervisor_status_ts = 500.0
    bridge._set_claude_sessions(0)

    await bridge._recompute_agent_state(now=500.0)
    bridge._supervisor_status_ts = 520.0
    await bridge._recompute_agent_state(now=520.0)   # 20s zero but no meeting → online

    assert _agent_states(ext) == ["online"]


@pytest.mark.asyncio
async def test_merge_unknown_session_count_does_not_force_offline():
    """
    given: supervisor reports `online` and the daemon reports an unknown
           (null) session count during an active meeting
    when:  the effective state is recomputed
    then:  the chip stays `online` — null is not a confident 0 and must not
           force offline (an SDK refactor renaming the private attr)
    """
    bridge, ext = _bridge_with_active_ktalk()
    bridge._supervisor_agent_status = "online"
    bridge._supervisor_status_ts = 300.0
    bridge._set_claude_sessions(None)

    await bridge._recompute_agent_state(now=300.0)

    assert _agent_states(ext) == ["online"]


@pytest.mark.asyncio
async def test_merge_broadcasts_only_on_effective_change():
    """
    given: a stable effective agent state
    when:  the merge is recomputed repeatedly with no change (heartbeat)
    then:  the chip is broadcast exactly once — heartbeats that do not change
           the effective status are not transmitted to extensions
    """
    bridge, ext = _bridge_with_active_ktalk()
    bridge._supervisor_agent_status = "working"
    bridge._supervisor_status_ts = 50.0
    bridge._set_claude_sessions(2)

    await bridge._recompute_agent_state(now=50.0)
    bridge._supervisor_status_ts = 51.0
    await bridge._recompute_agent_state(now=51.0)   # same effective → no broadcast
    bridge._supervisor_status_ts = 52.0
    await bridge._recompute_agent_state(now=52.0)   # still same → no broadcast

    assert _agent_states(ext) == ["working"]


@pytest.mark.asyncio
async def test_copilot_agent_status_frame_updates_and_broadcasts():
    """
    given: a bridge with an active ktalk meeting and an extension client
    when:  a `copilot_agent_status` frame arrives from the supervisor
    then:  the status is stored and an effective agent_state is broadcast
    """
    bridge, ext = _bridge_with_active_ktalk()

    await bridge._process_message({"type": "copilot_agent_status", "status": "online"})

    assert bridge._supervisor_agent_status == "online"
    assert _agent_states(ext) == ["online"]


@pytest.mark.asyncio
async def test_copilot_agent_status_invalid_is_ignored(capsys):
    """
    given: a bridge with an active meeting
    when:  a `copilot_agent_status` frame carries a missing/blank status
    then:  it is ignored (no stored status, no broadcast)
    """
    bridge, ext = _bridge_with_active_ktalk()

    await bridge._process_message({"type": "copilot_agent_status", "status": ""})

    assert bridge._supervisor_agent_status is None
    assert _agent_states(ext) == []


@pytest.mark.asyncio
async def test_claude_sessions_null_count_kept_unknown():
    """
    given: a bridge with an active meeting and supervisor reporting online
    when:  a `claude_sessions` frame carries an explicit null count
    then:  the count is stored as None (unknown) and does not force offline
    """
    bridge, ext = _bridge_with_active_ktalk()
    bridge._supervisor_agent_status = "online"
    bridge._supervisor_status_ts = time.monotonic()

    await bridge._process_message({"type": "claude_sessions", "count": None})

    assert bridge._claude_sessions_count is None
    assert _agent_states(ext) == ["online"]


@pytest.mark.asyncio
async def test_agent_state_primer_sent_to_reconnecting_client():
    """
    given: the effective agent chip is a steady `online` (mid-meeting copilot)
           and an extension tab reloads, opening a fresh WS
    when:  the new client connects (the chip broadcast is change-only, so no
           `agent_state` frame would otherwise be sent for a steady state)
    then:  the bridge primes the new connection with the current effective
           `agent_state` so the reloaded tab shows the live chip immediately
           instead of its default ⚫ offline for the rest of the meeting
    """
    bridge = KTalkBridge()
    mock_storage = Mock()
    mock_storage._load_meta = Mock(return_value={"source": "orphan"})
    mock_storage.on_ktalk_leave = Mock()
    bridge.set_meeting_storage(mock_storage)
    bridge._last_effective_agent_state = "online"

    class RecordingWS(FakeWebSocket):
        def __init__(self):
            super().__init__()
            self.sent: list[str] = []

        async def send(self, message: str):
            self.sent.append(message)

    ws = RecordingWS()
    await bridge._handle_client(ws)

    primers = [
        json.loads(m)
        for m in ws.sent
        if json.loads(m).get("status") == "agent_state"
    ]
    assert primers == [{
        "type": "copilot_status",
        "request_id": "",
        "status": "agent_state",
        "agent_state": "online",
    }]


# --- copilot frame fan-out: extension + subscriber clients (CAP-1) ---
#
# Every copilot broadcast must reach `_subscriber_clients` (the frontend
# `BridgeSubscriber`) in addition to `_connected_clients` (the extension),
# so the webapp `#/live` view can render copilot replies and status.
# `_broadcast_message` itself stays extension-only.


def _copilot_frames(client, frame_type: str, status: str | None = None):
    """Extract decoded frames of `frame_type` (optionally filtered by status)."""
    out = []
    for m in client.sent:
        f = json.loads(m)
        if f.get("type") != frame_type:
            continue
        if status is not None and f.get("status") != status:
            continue
        out.append(f)
    return out


def _bridge_with_extension_and_subscriber():
    """Bridge with one extension client and one subscriber client (recorders)."""
    bridge = KTalkBridge()
    ext = RecordingClient()
    sub = RecordingClient()
    bridge._connected_clients.add(ext)
    bridge._subscriber_clients.add(sub)
    return bridge, ext, sub


@pytest.mark.asyncio
async def test_copilot_reply_fans_out_to_extension_and_subscriber():
    """
    given: a bridge with one extension client and one subscriber client
    when:  the channel server sends `copilot_channel_reply`
    then:  both clients receive the identical `copilot_reply` frame — the
           webapp relay (`BridgeSubscriber`) cannot render replies otherwise
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()

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

    assert len(ext.sent) == 1
    assert ext.sent == sub.sent
    payload = json.loads(sub.sent[0])
    assert payload["type"] == "copilot_reply"
    assert payload["request_id"] == "req-42"
    assert payload["kind"] == "manual"
    assert payload["text"] == "Алиса спросила про метрики."
    assert payload["meeting_id"] == "2026-04-26_test_meeting"
    assert payload["created_at"] == 1700000123456.0


@pytest.mark.asyncio
async def test_copilot_per_request_status_fans_out_to_extension_and_subscriber():
    """
    given: a bridge with one extension client and one subscriber client
    when:  the channel server sends `copilot_channel_status`
    then:  both clients receive the identical per-request `copilot_status`
           frame so the webapp can drive its pending indicator
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()

    await bridge._process_message(
        {
            "type": "copilot_channel_status",
            "request_id": "req-42",
            "status": "working",
            "message": "Claude is thinking",
        }
    )

    assert len(ext.sent) == 1
    assert ext.sent == sub.sent
    payload = json.loads(sub.sent[0])
    assert payload["type"] == "copilot_status"
    assert payload["request_id"] == "req-42"
    assert payload["status"] == "working"
    assert payload["message"] == "Claude is thinking"


@pytest.mark.asyncio
async def test_agent_state_chip_fans_out_to_extension_and_subscriber():
    """
    given: a bridge with an active ktalk meeting, one extension client and
           one subscriber client
    when:  the effective agent state flips (supervisor reports `working`)
    then:  both clients receive the `copilot_status status='agent_state'`
           chip frame — the webapp chip mirrors the extension chip
    """
    bridge, ext = _bridge_with_active_ktalk()
    sub = RecordingClient()
    bridge._subscriber_clients.add(sub)

    bridge._supervisor_agent_status = "working"
    bridge._supervisor_status_ts = 50.0
    bridge._set_claude_sessions(1)

    await bridge._recompute_agent_state(now=50.0)

    assert _agent_states(ext) == ["working"]
    assert _agent_states(sub) == ["working"]
    assert ext.sent == sub.sent


@pytest.mark.asyncio
async def test_channel_connected_fans_out_to_extension_and_subscriber():
    """
    given: a bridge with one extension client and one subscriber client and
           no channel clients yet
    when:  the first channel client sends `copilot_channel_hello` (0→1)
    then:  both clients receive `copilot_status status='channel_connected'`
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()

    channel_ws = ControllableFakeWebSocket()
    channel_ws.add_message(json.dumps({"type": "copilot_channel_hello"}))

    handler_task = asyncio.create_task(bridge._handle_client(channel_ws))
    for _ in range(50):
        if channel_ws in bridge._copilot_channel_clients:
            break
        await asyncio.sleep(0.01)

    try:
        assert channel_ws in bridge._copilot_channel_clients
        assert _copilot_frames(ext, "copilot_status", "channel_connected")
        assert ext.sent == sub.sent
    finally:
        channel_ws.signal_close()
        await handler_task


@pytest.mark.asyncio
async def test_channel_disconnected_fans_out_to_extension_and_subscriber():
    """
    given: a registered channel client, one extension client and one
           subscriber client
    when:  the last channel client's socket closes (1→0)
    then:  both clients receive `copilot_status status='channel_disconnected'`
           — without this the webapp stays blind to channel-daemon death
           mid-session
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()

    channel_ws = ControllableFakeWebSocket()
    channel_ws.add_message(json.dumps({"type": "copilot_channel_hello"}))

    handler_task = asyncio.create_task(bridge._handle_client(channel_ws))
    try:
        for _ in range(50):
            if channel_ws in bridge._copilot_channel_clients:
                break
            await asyncio.sleep(0.01)
        assert channel_ws in bridge._copilot_channel_clients

        # Drop the `channel_connected` frames emitted on hello.
        ext.sent.clear()
        sub.sent.clear()

        channel_ws.signal_close()
        await handler_task

        assert len(_copilot_frames(ext, "copilot_status", "channel_disconnected")) == 1
        assert ext.sent == sub.sent
    finally:
        channel_ws.signal_close()
        if not handler_task.done():
            await handler_task


@pytest.mark.asyncio
async def test_copilot_frame_reaches_subscriber_without_extension_clients():
    """
    given: a bridge with a subscriber client and NO extension clients
           (webapp open, no conference tab — the primary CAP-1 scenario)
    when:  a copilot frame is broadcast
    then:  the subscriber still receives it — the fan-out must not inherit
           `_broadcast_message`'s empty-`_connected_clients` early return
    """
    bridge = KTalkBridge()
    sub = RecordingClient()
    bridge._subscriber_clients.add(sub)
    assert not bridge._connected_clients

    await bridge._process_message(
        {
            "type": "copilot_channel_reply",
            "request_id": "req-9",
            "kind": "manual",
            "text": "ответ без расширения",
        }
    )

    assert len(sub.sent) == 1
    payload = json.loads(sub.sent[0])
    assert payload["type"] == "copilot_reply"
    assert payload["text"] == "ответ без расширения"


@pytest.mark.asyncio
async def test_dead_client_does_not_abort_copilot_fan_out():
    """
    given: a dead client whose `send` raises, sitting in `_connected_clients`,
           plus a live extension recorder and a live subscriber recorder —
           the fan-out iterates extensions before subscribers, so the
           subscriber is guaranteed to be reached AFTER the dead socket
    when:  a copilot frame is broadcast
    then:  no exception escapes the broadcast and BOTH live clients still
           receive the frame — a dead socket must not abort delivery to the
           recipients queued behind it
    """
    class ExplodingClient:
        async def send(self, message: str):
            raise RuntimeError("socket is gone")

    bridge = KTalkBridge()
    ext = RecordingClient()
    sub = RecordingClient()
    bridge._connected_clients.add(ExplodingClient())
    bridge._connected_clients.add(ext)
    bridge._subscriber_clients.add(sub)

    await bridge._process_message(
        {
            "type": "copilot_channel_status",
            "request_id": "req-5",
            "status": "done",
        }
    )

    assert len(_copilot_frames(ext, "copilot_status", "done")) == 1
    assert len(_copilot_frames(sub, "copilot_status", "done")) == 1


@pytest.mark.asyncio
async def test_non_copilot_frame_is_not_fanned_out_to_subscriber():
    """
    given: a bridge with one extension client and one subscriber client
    when:  a non-copilot frame (`transcript_history`) is broadcast
    then:  only the extension receives it — subscribers must not start
           seeing transcript/lifecycle/meeting frames
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()

    await bridge._broadcast_message(
        {"type": "transcript_history", "meeting_id": "m-1", "segments": []}
    )

    assert len(ext.sent) == 1
    assert json.loads(ext.sent[0])["type"] == "transcript_history"
    assert sub.sent == []


# --- manual copilot session: spawn/stop relay + manual state (CAP-2/3/4) ---
#
# The bridge relays `copilot_spawn_request` / `copilot_stop_request`
# (webapp -> frontend -> bridge -> supervisor) and mirrors the supervisor's
# manual-session truth in `_manual_copilot_active`, so a supervisor reconnect
# adopts a live manual session instead of GRACE-killing it. Conference
# meetings take precedence over manual state everywhere the two can conflict.


def _bridge_with_supervisor():
    """Bridge with a registered supervisor client and a subscriber (recorders)."""
    bridge = KTalkBridge()
    sup = RecordingClient()
    sub = RecordingClient()
    bridge._supervisor_clients.add(sup)
    bridge._subscriber_clients.add(sub)
    return bridge, sup, sub


@pytest.mark.asyncio
async def test_real_subscriber_socket_relays_spawn_and_stop_to_supervisor():
    """
    given: a real bridge server with hello-registered subscriber and supervisor
    when:  the subscriber sends manual spawn and stop frames with opaque fields
    then:  the supervisor receives each complete JSON object in order and the
           bridge mirror follows the last delivered operation
    """
    bridge = KTalkBridge()
    server, url = await _serve_bridge(bridge)
    supervisor = subscriber = None
    try:
        supervisor = await websockets.connect(url)
        await supervisor.send(json.dumps({"type": "supervisor_hello"}))
        await _recv_type(supervisor, "meeting_state")

        subscriber = await websockets.connect(url)
        await subscriber.send(json.dumps({"type": "subscriber_hello"}))
        spawn = {
            "type": "copilot_spawn_request",
            "request_id": "req-real-spawn",
            "opaque": {"future_protocol_field": True},
        }
        await subscriber.send(json.dumps(spawn))

        assert await _recv_type(supervisor, "copilot_spawn_request") == spawn
        assert bridge._manual_copilot_active is True

        stop = {
            "type": "copilot_stop_request",
            "request_id": "req-real-stop",
            "opaque": "preserved",
        }
        await subscriber.send(json.dumps(stop))

        assert await _recv_type(supervisor, "copilot_stop_request") == stop
        assert bridge._manual_copilot_active is False
    finally:
        if subscriber is not None:
            await subscriber.close()
        if supervisor is not None:
            await supervisor.close()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_real_subscriber_socket_gets_spawn_rejection_for_its_request():
    """
    given: a real bridge server with a subscriber but no supervisor
    when:  that subscriber sends a manual spawn request
    then:  the same socket receives `supervisor_disconnected` with its request id
    """
    bridge = KTalkBridge()
    server, url = await _serve_bridge(bridge)
    subscriber = None
    try:
        subscriber = await websockets.connect(url)
        await subscriber.send(json.dumps({"type": "subscriber_hello"}))
        await subscriber.send(json.dumps({
            "type": "copilot_spawn_request",
            "request_id": "req-real-reject",
        }))

        rejected = await _recv_type(subscriber, "copilot_status")
        while rejected.get("status") != "supervisor_disconnected":
            rejected = await _recv_type(subscriber, "copilot_status")
        assert rejected["request_id"] == "req-real-reject"
    finally:
        if subscriber is not None:
            await subscriber.close()
        server.close()
        await server.wait_closed()


@pytest.mark.asyncio
async def test_spawn_request_relays_to_supervisor_and_arms_manual_flag():
    """
    given: a registered supervisor client and no conference meeting
    when:  a subscriber socket sends `copilot_spawn_request`
    then:  the frame is forwarded verbatim to the supervisor and
           `_manual_copilot_active` is armed, so a later `supervisor_hello`
           reconciles as "adopt" instead of killing the session
    """
    bridge, sup, sub = _bridge_with_supervisor()
    origin = RecordingClient()

    request = {
        "type": "copilot_spawn_request",
        "request_id": "req-spawn",
        "opaque": {"future_protocol_field": True},
    }
    await bridge._process_message(request, websocket=origin)

    assert _copilot_frames(sup, "copilot_spawn_request") == [request]
    assert bridge._manual_copilot_active is True
    # No error reply on the happy path; the supervisor answers with status.
    assert origin.sent == []


@pytest.mark.asyncio
async def test_spawn_request_during_conference_is_rejected():
    """
    given: an active conference meeting and a registered supervisor
    when:  a `copilot_spawn_request` arrives
    then:  nothing at all reaches the supervisor (it would read the frame as
           `meeting_started("manual")` and kill the conference-bound session),
           the manual flag stays False, and the originator alone gets
           `copilot_status {status: "conference_active"}` echoing request_id
    """
    bridge, sup, sub = _bridge_with_supervisor()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1", title="Standup"))
    bridge._connected_clients.add(RecordingClient())
    origin = RecordingClient()

    await bridge._process_message(
        {"type": "copilot_spawn_request", "request_id": "req-conf"},
        websocket=origin,
    )

    assert sup.sent == []
    assert bridge._manual_copilot_active is False
    rejected = _copilot_frames(origin, "copilot_status", "conference_active")
    assert len(rejected) == 1
    assert rejected[0]["request_id"] == "req-conf"
    # The error binds to the originating click — never fanned out.
    assert sub.sent == []


@pytest.mark.asyncio
async def test_spawn_request_without_supervisor_replies_disconnected():
    """
    given: no supervisor client is registered
    when:  a `copilot_spawn_request` arrives
    then:  the originator gets `supervisor_disconnected` echoing request_id and
           the manual flag stays False — arming it with no relay (and thus no
           tmux session) would make a later hello silently spawn a session the
           user was already told had failed
    """
    bridge = KTalkBridge()
    sub = RecordingClient()
    bridge._subscriber_clients.add(sub)
    origin = RecordingClient()

    await bridge._process_message(
        {"type": "copilot_spawn_request", "request_id": "req-nosup"},
        websocket=origin,
    )

    frames = _copilot_frames(origin, "copilot_status", "supervisor_disconnected")
    assert len(frames) == 1
    assert frames[0]["request_id"] == "req-nosup"
    assert bridge._manual_copilot_active is False
    assert sub.sent == []


@pytest.mark.asyncio
async def test_stop_request_relays_to_supervisor_and_clears_flag():
    """
    given: a live manual session (flag armed) and a registered supervisor
    when:  a `copilot_stop_request` arrives
    then:  the frame is forwarded verbatim and the manual flag is cleared
    """
    bridge, sup, sub = _bridge_with_supervisor()
    bridge._manual_copilot_active = True
    bridge._sessions_zero_since = 1.0
    origin = RecordingClient()

    request = {
        "type": "copilot_stop_request",
        "request_id": "req-stop",
        "opaque": {"future_protocol_field": True},
    }
    await bridge._process_message(request, websocket=origin)

    assert _copilot_frames(sup, "copilot_stop_request") == [request]
    assert bridge._manual_copilot_active is False
    assert bridge._sessions_zero_since is None
    assert origin.sent == []


@pytest.mark.asyncio
async def test_stop_request_during_conference_is_still_relayed():
    """
    given: an active conference meeting and a registered supervisor
    when:  a `copilot_stop_request` arrives
    then:  the exact frame still reaches the supervisor and no
           `conference_active` rejection is returned
    """
    bridge, sup, sub = _bridge_with_supervisor()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1", title="Standup"))
    bridge._connected_clients.add(RecordingClient())
    origin = RecordingClient()
    request = {
        "type": "copilot_stop_request",
        "request_id": "req-stop-conf",
        "opaque": "preserved",
    }

    await bridge._process_message(request, websocket=origin)

    assert _copilot_frames(sup, "copilot_stop_request") == [request]
    assert _copilot_frames(origin, "copilot_status", "conference_active") == []


@pytest.mark.asyncio
async def test_stop_request_without_supervisor_still_clears_flag():
    """
    given: a live manual session (flag armed) and no supervisor registered
    when:  a `copilot_stop_request` arrives
    then:  the originator gets `supervisor_disconnected` echoing request_id and
           the flag is cleared anyway — there is no session to keep alive for
    """
    bridge = KTalkBridge()
    bridge._manual_copilot_active = True
    origin = RecordingClient()

    await bridge._process_message(
        {"type": "copilot_stop_request", "request_id": "req-stop-nosup"},
        websocket=origin,
    )

    frames = _copilot_frames(origin, "copilot_status", "supervisor_disconnected")
    assert len(frames) == 1
    assert frames[0]["request_id"] == "req-stop-nosup"
    assert bridge._manual_copilot_active is False


@pytest.mark.asyncio
async def test_malformed_spawn_and_stop_requests_are_ignored(capsys):
    """
    given: a registered supervisor and an armed manual flag
    when:  spawn/stop frames arrive with a missing, empty or non-string
           request_id
    then:  each is dropped with a log — no relay, no reply, no flag change
           (an un-echoable error could never be bound to a click anyway)
    """
    bridge, sup, sub = _bridge_with_supervisor()
    bridge._manual_copilot_active = True
    origin = RecordingClient()

    malformed = [
        {"type": "copilot_spawn_request"},
        {"type": "copilot_spawn_request", "request_id": ""},
        {"type": "copilot_spawn_request", "request_id": 42},
        {"type": "copilot_stop_request"},
        {"type": "copilot_stop_request", "request_id": ""},
        {"type": "copilot_stop_request", "request_id": None},
    ]
    for frame in malformed:
        await bridge._process_message(frame, websocket=origin)

    assert sup.sent == []
    assert origin.sent == []
    assert sub.sent == []
    assert bridge._manual_copilot_active is True
    err = capsys.readouterr().err
    assert err.count("missing/invalid request_id") == len(malformed)


def test_meeting_state_reports_manual_session():
    """
    given: an armed manual flag and no conference meeting
    when:  the bridge builds a `meeting_state` reply
    then:  it reports the synthetic manual meeting so the supervisor's
           reconcile adopts the live session instead of GRACE-killing it
    """
    bridge = KTalkBridge()
    bridge._manual_copilot_active = True

    frame = bridge._compute_meeting_state()

    assert frame == {
        "type": "meeting_state",
        "active": True,
        "meeting_id": "manual",
        "meeting_title": None,
    }


def test_meeting_state_conference_takes_precedence_over_manual():
    """
    given: an armed manual flag AND an active conference meeting
    when:  the bridge builds a `meeting_state` reply
    then:  the conference meeting is reported — "manual" must never mask a
           real meeting id the supervisor binds its session to
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1", title="Standup"))
    bridge._connected_clients.add(RecordingClient())
    bridge._manual_copilot_active = True

    frame = bridge._compute_meeting_state()

    assert frame["active"] is True
    assert frame["meeting_id"] == "m-1"
    assert frame["meeting_title"] == "Standup"


@pytest.mark.asyncio
async def test_supervisor_hello_rearms_manual_flag_before_reply():
    """
    given: a bridge that lost its manual flag (restart) and no conference
    when:  the supervisor reconnects with `current_meeting_id == "manual"`
    then:  the flag is re-armed BEFORE the reply is computed, so the
           `meeting_state` answer is already `{active: True, "manual"}` and
           the supervisor adopts its live tmux session (D5)
    """
    bridge = KTalkBridge()
    sup = RecordingClient()

    await bridge._process_message(
        {"type": "supervisor_hello", "current_meeting_id": "manual"},
        websocket=sup,
    )

    assert bridge._manual_copilot_active is True
    states = _copilot_frames(sup, "meeting_state")
    assert len(states) == 1
    assert states[0]["active"] is True
    assert states[0]["meeting_id"] == "manual"


@pytest.mark.asyncio
async def test_supervisor_hello_manual_id_does_not_rearm_during_conference():
    """
    given: an active conference meeting
    when:  a supervisor hello arrives with `current_meeting_id == "manual"`
    then:  the manual flag stays False and the reply carries the conference
           meeting — conference precedence outranks the stale manual claim
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_ktalk_storage(meeting_id="m-1", title="Standup"))
    bridge._connected_clients.add(RecordingClient())
    sup = RecordingClient()

    await bridge._process_message(
        {"type": "supervisor_hello", "current_meeting_id": "manual"},
        websocket=sup,
    )

    assert bridge._manual_copilot_active is False
    states = _copilot_frames(sup, "meeting_state")
    assert len(states) == 1
    assert states[0]["active"] is True
    assert states[0]["meeting_id"] == "m-1"


@pytest.mark.asyncio
async def test_conference_meeting_started_preempts_manual_session():
    """
    given: a live manual session and an extension about to join a conference
    when:  `meeting_join` drives the conference `meeting_started` emit path
    then:  the manual flag is cleared (the supervisor rebinds its session to
           the meeting id) and subscribers ALONE get the informational
           `manual_preempted` notice — the extension stream stays untouched
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_meeting_storage_mock(meeting_id="m-conf"))
    ext = RecordingClient()
    sub = RecordingClient()
    bridge._connected_clients.add(ext)
    bridge._subscriber_clients.add(sub)
    bridge._manual_copilot_active = True

    await bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/room",
        "title": "Standup",
        "platform": "ktalk",
    })

    assert bridge._manual_copilot_active is False
    notices = _copilot_frames(sub, "copilot_status", "manual_preempted")
    assert len(notices) == 1
    assert notices[0]["request_id"] == ""
    assert _copilot_frames(ext, "copilot_status", "manual_preempted") == []


@pytest.mark.asyncio
async def test_conference_start_without_manual_session_emits_no_preemption():
    """
    given: no manual copilot session and a subscriber watching bridge status
    when:  an extension starts an ordinary conference meeting
    then:  no `manual_preempted` notice is emitted
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_meeting_storage_mock(meeting_id="m-conf"))
    ext = RecordingClient()
    sub = RecordingClient()
    bridge._connected_clients.add(ext)
    bridge._subscriber_clients.add(sub)

    await bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/room",
        "title": "Standup",
        "platform": "ktalk",
    })

    assert _copilot_frames(sub, "copilot_status", "manual_preempted") == []


@pytest.mark.asyncio
async def test_manual_preemption_delivers_past_a_dead_subscriber():
    """
    given: an active manual mirror, one dead subscriber and one healthy one
    when:  conference preemption broadcasts `manual_preempted`
    then:  the dead socket is logged and the healthy subscriber still receives
           the notice without an exception escaping
    """
    class ExplodingSubscriber:
        async def send(self, message: str):
            raise RuntimeError("socket is gone")

    bridge = KTalkBridge()
    healthy = RecordingClient()
    bridge._subscriber_clients.add(ExplodingSubscriber())
    bridge._subscriber_clients.add(healthy)
    bridge._manual_copilot_active = True

    await bridge._preempt_manual_copilot()

    assert len(_copilot_frames(healthy, "copilot_status", "manual_preempted")) == 1
    assert bridge._manual_copilot_active is False


@pytest.mark.asyncio
async def test_manual_session_with_zero_claude_sessions_goes_offline():
    """
    given: a manual session (no conference), supervisor reporting `online`,
           and the daemon reporting a confident zero live MCP sessions
    when:  the zero-session condition holds past the debounce window
    then:  the effective chip flips to offline — a manual copilot whose Claude
           session never registered must not keep showing a false green chip
    """
    bridge = KTalkBridge()
    sub = RecordingClient()
    bridge._subscriber_clients.add(sub)
    bridge._manual_copilot_active = True
    bridge._supervisor_agent_status = "online"
    bridge._supervisor_status_ts = 100.0
    bridge._set_claude_sessions(0)

    await bridge._recompute_agent_state(now=100.0)   # zero just started → online
    bridge._supervisor_status_ts = 111.0             # heartbeat keeps it fresh
    await bridge._recompute_agent_state(now=111.0)   # 11s ≥ debounce → offline

    assert _agent_states(sub) == ["online", "offline"]


@pytest.mark.asyncio
async def test_manual_chat_message_carries_no_meeting_id():
    """
    given: no conference meeting is active — storage still reports an orphan
           meeting id, as it always does after a leave
    when:  a `copilot_chat_message` is forwarded to the channel client
    then:  the outbound `copilot_user_message` carries meeting_id None and the
           pending entry records "" — a manual request must not inherit
           conference staleness semantics
    """
    bridge = KTalkBridge()
    storage = _ktalk_storage(meeting_id="orphan-1", source="orphan")
    storage.get_current_meeting_id = Mock(return_value="orphan-1")
    bridge.set_meeting_storage(storage)
    channel = RecordingClient()
    bridge._copilot_channel_clients.add(channel)

    await bridge._process_message({
        "type": "copilot_chat_message",
        "request_id": "req-manual",
        "text": "что было сказано?",
    }, websocket=RecordingClient())

    forwarded = _copilot_frames(channel, "copilot_user_message")
    assert len(forwarded) == 1
    assert forwarded[0]["meeting_id"] is None
    assert bridge._copilot_pending_requests["req-manual"] == ""


@pytest.mark.asyncio
async def test_manual_chat_reply_survives_meeting_rotation():
    """
    given: a manual chat request forwarded with no conference active, whose
           meeting stamp the channel server echoes back verbatim
    when:  `_check_silence_split` rotates the orphan meeting mid-flight and the
           reply only then arrives
    then:  the reply is still broadcast — the reply-side staleness cross-check
           has no meeting id to mismatch against
    """
    bridge = KTalkBridge()
    storage = _ktalk_storage(meeting_id="orphan-1", source="orphan")
    storage.get_current_meeting_id = Mock(side_effect=lambda: storage.current_meeting_id)
    bridge.set_meeting_storage(storage)
    channel = RecordingClient()
    bridge._copilot_channel_clients.add(channel)
    sub = RecordingClient()
    bridge._subscriber_clients.add(sub)

    await bridge._process_message({
        "type": "copilot_chat_message",
        "request_id": "req-rot",
        "text": "вопрос",
    }, websocket=RecordingClient())
    echoed_meeting_id = _copilot_frames(channel, "copilot_user_message")[0]["meeting_id"]

    # Orphan silence-split rotates the current meeting while Claude thinks.
    storage.current_meeting_id = "orphan-2"

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-rot",
        "kind": "manual",
        "text": "ответ",
        "meeting_id": echoed_meeting_id,
    })

    replies = _copilot_frames(sub, "copilot_reply")
    assert len(replies) == 1
    assert replies[0]["text"] == "ответ"


@pytest.mark.asyncio
async def test_spawn_request_with_only_a_dead_supervisor_is_not_armed():
    """
    given: the sole registered supervisor is a socket in its closing handshake
           whose `send` raises — `_handle_client`'s finally-block has not
           reaped it from `_supervisor_clients` yet
    when:  a `copilot_spawn_request` arrives
    then:  registration is not delivery: the originator gets
           `supervisor_disconnected` and the manual mirror stays False, so no
           later `supervisor_hello` can reconcile an armed mirror into a
           session the user was already told had failed
    """
    class ExplodingClient:
        async def send(self, message: str):
            raise RuntimeError("socket is gone")

    bridge = KTalkBridge()
    bridge._supervisor_clients.add(ExplodingClient())
    origin = RecordingClient()

    await bridge._process_message(
        {"type": "copilot_spawn_request", "request_id": "req-dead"},
        websocket=origin,
    )

    frames = _copilot_frames(origin, "copilot_status", "supervisor_disconnected")
    assert len(frames) == 1
    assert frames[0]["request_id"] == "req-dead"
    assert bridge._manual_copilot_active is False


@pytest.mark.asyncio
async def test_spawn_request_delivers_past_a_dead_supervisor():
    """
    given: a dead supervisor socket registered alongside a live one
    when:  a `copilot_spawn_request` arrives
    then:  no exception escapes, the live supervisor still receives the frame,
           and the mirror is armed — one broken socket must not deny a spawn
           the machine can actually serve
    """
    class ExplodingClient:
        async def send(self, message: str):
            raise RuntimeError("socket is gone")

    bridge = KTalkBridge()
    live = RecordingClient()
    bridge._supervisor_clients.add(ExplodingClient())
    bridge._supervisor_clients.add(live)
    origin = RecordingClient()

    await bridge._process_message(
        {"type": "copilot_spawn_request", "request_id": "req-mixed"},
        websocket=origin,
    )

    assert _copilot_frames(live, "copilot_spawn_request") == [
        {"type": "copilot_spawn_request", "request_id": "req-mixed"}
    ]
    assert bridge._manual_copilot_active is True
    assert origin.sent == []


@pytest.mark.asyncio
async def test_spawn_request_is_not_armed_when_conference_starts_mid_relay():
    """
    given: no conference at guard time, but a `meeting_join` on another socket
           completes while the relay awaits `client.send` — its
           `_preempt_manual_copilot()` sees the mirror still False and no-ops
    when:  the spawn handler resumes after the relay
    then:  it re-checks and does NOT arm the mirror — otherwise the bridge
           would claim a manual session for the whole conference and past its
           end, with no preemption left to clear it
    """
    bridge = KTalkBridge()
    # Starts as an orphan (no conference — the spawn guard must pass), flips
    # to a real ktalk meeting when `meeting_join` lands mid-relay.
    storage = Mock()
    storage.current_meeting_path = Path("/tmp/orphan-1.jsonl")
    storage.current_meeting_id = "orphan-1"
    storage._load_meta = Mock(return_value={"source": "orphan"})

    def _on_join(url, *args, **kwargs):
        storage.current_meeting_path = Path("/tmp/m-conf.jsonl")
        storage.current_meeting_id = "m-conf"
        storage._load_meta = Mock(return_value={"source": "ktalk", "title": "Standup"})
        return "m-conf"

    storage.on_meeting_join = Mock(side_effect=_on_join)
    storage.get_current_transcript = Mock(return_value=[])
    storage.get_current_meeting_id = Mock(side_effect=lambda: storage.current_meeting_id)
    bridge.set_meeting_storage(storage)
    ext = RecordingClient()
    bridge._connected_clients.add(ext)

    class ConferenceRacingSupervisor(RecordingClient):
        """Supervisor whose send yields to a concurrent conference start."""

        async def send(self, message: str):
            await super().send(message)
            await bridge._process_message({
                "type": "meeting_join",
                "url": "https://ktalk.test/room",
                "title": "Standup",
                "platform": "ktalk",
            })

    sup = ConferenceRacingSupervisor()
    bridge._supervisor_clients.add(sup)
    origin = RecordingClient()

    assert not bridge._is_meeting_active()   # guard passes at entry
    await bridge._process_message(
        {"type": "copilot_spawn_request", "request_id": "req-race"},
        websocket=origin,
    )

    # The frame did reach the supervisor — it rebinds to the conference id on
    # the `meeting_started` it received in the same window.
    assert _copilot_frames(sup, "copilot_spawn_request")
    assert bridge._is_meeting_active()
    assert bridge._manual_copilot_active is False
    rejected = _copilot_frames(origin, "copilot_status", "conference_active")
    assert len(rejected) == 1
    assert rejected[0]["request_id"] == "req-race"


@pytest.mark.asyncio
async def test_no_conference_given_spawn_fanout_when_conference_starts_then_late_send_stops():
    """
    given: two supervisors and no conference when a manual spawn relay starts
    when:  a conference becomes active after the first supervisor receives it
    then:  no later supervisor receives a spawn that could displace the conference
    """
    bridge = KTalkBridge()
    conference_active = False
    bridge._is_meeting_active = Mock(side_effect=lambda: conference_active)

    class ConferenceStartingSupervisor(RecordingClient):
        async def send(self, message: str):
            nonlocal conference_active
            await super().send(message)
            conference_active = True

    first = ConferenceStartingSupervisor()
    late = RecordingClient()
    bridge._supervisor_clients = [first, late]
    origin = RecordingClient()
    request = {
        "type": "copilot_spawn_request",
        "request_id": "req-fanout-race",
    }

    await bridge._process_message(request, websocket=origin)

    assert _copilot_frames(first, "copilot_spawn_request") == [request]
    assert _copilot_frames(late, "copilot_spawn_request") == []
    rejected = _copilot_frames(origin, "copilot_status", "conference_active")
    assert len(rejected) == 1
    assert rejected[0]["request_id"] == "req-fanout-race"
    assert bridge._manual_copilot_active is False


@pytest.mark.asyncio
async def test_stop_waits_for_in_flight_spawn_before_updating_manual_state():
    """
    given: a spawn relay is suspended in `send` and another subscriber sends stop
    when:  the spawn delivery resumes
    then:  the supervisor observes spawn then stop and the bridge finishes
           inactive, matching the last operation the supervisor processed
    """
    class BlockingSpawnSupervisor(RecordingClient):
        def __init__(self):
            super().__init__()
            self.spawn_started = asyncio.Event()
            self.release_spawn = asyncio.Event()

        async def send(self, message: str):
            self.sent.append(message)
            if json.loads(message).get("type") == "copilot_spawn_request":
                self.spawn_started.set()
                await self.release_spawn.wait()

    bridge = KTalkBridge()
    sup = BlockingSpawnSupervisor()
    bridge._supervisor_clients.add(sup)

    spawn_task = asyncio.create_task(bridge._process_message(
        {"type": "copilot_spawn_request", "request_id": "req-spawn-race"},
        websocket=RecordingClient(),
    ))
    await asyncio.wait_for(sup.spawn_started.wait(), timeout=1.0)
    stop_task = asyncio.create_task(bridge._process_message(
        {"type": "copilot_stop_request", "request_id": "req-stop-race"},
        websocket=RecordingClient(),
    ))
    await asyncio.sleep(0)

    assert [json.loads(message)["type"] for message in sup.sent] == [
        "copilot_spawn_request"
    ]

    sup.release_spawn.set()
    await asyncio.gather(spawn_task, stop_task)

    assert [json.loads(message)["type"] for message in sup.sent] == [
        "copilot_spawn_request",
        "copilot_stop_request",
    ]
    assert bridge._manual_copilot_active is False


@pytest.mark.asyncio
async def test_manual_active_given_stop_in_flight_when_hello_rearms_then_stop_wins():
    """
    given: a stop relay is suspended after clearing the manual mirror
    when:  a reconnecting supervisor hello re-arms it before delivery finishes
    then:  the completed stop clears the mirror again and reconciliation is inactive
    """
    class BlockingStopSupervisor(RecordingClient):
        def __init__(self):
            super().__init__()
            self.stop_started = asyncio.Event()
            self.release_stop = asyncio.Event()

        async def send(self, message: str):
            self.sent.append(message)
            if json.loads(message).get("type") == "copilot_stop_request":
                self.stop_started.set()
                await self.release_stop.wait()

    bridge = KTalkBridge()
    bridge._manual_copilot_active = True
    sup = BlockingStopSupervisor()
    bridge._supervisor_clients.add(sup)

    stop_task = asyncio.create_task(bridge._process_message(
        {"type": "copilot_stop_request", "request_id": "req-stop-hello-race"},
        websocket=RecordingClient(),
    ))
    await asyncio.wait_for(sup.stop_started.wait(), timeout=1.0)

    reconnecting_sup = RecordingClient()
    await bridge._process_message(
        {"type": "supervisor_hello", "current_meeting_id": "manual"},
        websocket=reconnecting_sup,
    )
    assert bridge._manual_copilot_active is True

    sup.release_stop.set()
    await stop_task

    assert bridge._manual_copilot_active is False
    assert bridge._compute_meeting_state()["active"] is False


def test_contended_manual_lock_given_bridge_restart_when_reused_then_new_loop_works():
    """
    given: the manual request lock was contended on the bridge thread's event loop
    when:  the same bridge object restarts and creates a new event loop
    then:  concurrent manual requests can contend without an old-loop binding error
    """
    bridge = KTalkBridge()

    async def contend_manual_lock():
        await bridge._manual_copilot_request_lock.acquire()
        waiter = asyncio.create_task(bridge._manual_copilot_request_lock.acquire())
        await asyncio.sleep(0)
        bridge._manual_copilot_request_lock.release()
        await waiter
        bridge._manual_copilot_request_lock.release()

    bridge._run_server = contend_manual_lock

    bridge._thread_main()
    bridge._thread_main()


# --- copilot reply drop guards: terminal echo + manual-preempt invalidation ---
# A tracked reply dropped by either drop branch (stale-prefix / cross-meeting
# mismatch) must terminally resolve the asking UI via a broadcast
# `copilot_status {status: "error"}`; untracked (proactive) drops stay fully
# silent. `_preempt_manual_copilot` stale-marks pending manual entries
# (meeting id "") so a late manual reply cannot leak into the conference chat.

_MSG_MEETING_ENDED = (
    "Ответ не доставлен: встреча, в которой был задан вопрос, уже завершилась."
)
_MSG_MEETING_CHANGED = "Ответ не доставлен: встреча сменилась до получения ответа."
_MSG_MANUAL_PREEMPTED = "Ответ не доставлен: ручная сессия copilot была прервана."


@pytest.mark.asyncio
async def test_stale_drop_emits_one_terminal_error_to_both_audiences(capsys):
    """
    given: a tracked pending entry stale-marked with a meeting scope
           ("stale:m-prev"), one extension and one subscriber client
    when:  the copilot_channel_reply for that request_id arrives
    then:  no copilot_reply is broadcast; extension AND subscriber each
           receive exactly ONE copilot_status {status: "error"} with the
           request_id and the meeting-ended message; the entry is popped
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()
    bridge._copilot_pending_requests["req-late"] = "stale:m-prev"

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-late",
        "kind": "manual",
        "text": "поздняя сводка",
    })

    for client in (ext, sub):
        assert _copilot_frames(client, "copilot_reply") == []
        errors = _copilot_frames(client, "copilot_status", "error")
        assert len(errors) == 1
        assert errors[0]["request_id"] == "req-late"
        assert errors[0]["message"] == _MSG_MEETING_ENDED
    assert "req-late" not in bridge._copilot_pending_requests
    assert (
        "copilot_channel_reply for stale request_id=req-late "
        "(kind=manual) — dropped"
    ) in capsys.readouterr().err


@pytest.mark.asyncio
async def test_cross_meeting_drop_for_tracked_request_emits_terminal_error(capsys):
    """
    given: a tracked non-stale entry ("m-prev"), current meeting "m-new",
           one extension and one subscriber client
    when:  a copilot_channel_reply arrives carrying meeting_id "m-prev"
    then:  no copilot_reply; ONE terminal copilot_status error per client
           with the meeting-changed message; the entry is popped
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-new"))
    bridge._copilot_pending_requests["req-x"] = "m-prev"

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-x",
        "kind": "manual",
        "text": "ответ из прошлой встречи",
        "meeting_id": "m-prev",
    })

    for client in (ext, sub):
        assert _copilot_frames(client, "copilot_reply") == []
        errors = _copilot_frames(client, "copilot_status", "error")
        assert len(errors) == 1
        assert errors[0]["request_id"] == "req-x"
        assert errors[0]["message"] == _MSG_MEETING_CHANGED
    assert "req-x" not in bridge._copilot_pending_requests
    assert (
        "copilot_channel_reply for meeting_id=m-prev but current is "
        "m-new (kind=manual) — dropped"
    ) in capsys.readouterr().err


@pytest.mark.asyncio
async def test_cross_meeting_drop_for_untracked_reply_emits_no_status(capsys):
    """
    given: no pending entry (proactive `proactive_<uuid>` reply), current
           meeting "m-now"
    when:  a copilot_channel_reply arrives carrying mismatching meeting_id
    then:  nothing is broadcast at all — an unmatched per-request error
           frame would render a spurious error bubble in both UIs
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()
    bridge.set_meeting_storage(_make_storage_for_meeting("m-now"))

    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "proactive_abc123",
        "kind": "proactive",
        "text": "заметка из прошлой встречи",
        "meeting_id": "m-old",
    })

    assert ext.sent == []
    assert sub.sent == []
    assert (
        "copilot_channel_reply for meeting_id=m-old but current is "
        "m-now (kind=proactive) — dropped"
    ) in capsys.readouterr().err


@pytest.mark.asyncio
async def test_preempt_manual_copilot_stale_marks_only_manual_entries(capsys):
    """
    given: an armed manual mirror and pending entries of every scope:
           manual (""), conference ("m-cur"), already-stale ("stale:m-old")
    when:  `_preempt_manual_copilot` runs
    then:  only the ""-valued entry becomes "stale:"; the conference and
           already-stale entries are untouched
    """
    bridge = KTalkBridge()
    bridge._manual_copilot_active = True
    bridge._copilot_pending_requests["req-m"] = ""
    bridge._copilot_pending_requests["req-c"] = "m-cur"
    bridge._copilot_pending_requests["req-s"] = "stale:m-old"

    await bridge._preempt_manual_copilot()

    assert bridge._copilot_pending_requests["req-m"] == "stale:"
    assert bridge._copilot_pending_requests["req-c"] == "m-cur"
    assert bridge._copilot_pending_requests["req-s"] == "stale:m-old"
    assert (
        "Marked 1 manual copilot request(s) stale on preemption"
    ) in capsys.readouterr().err


@pytest.mark.asyncio
async def test_preempt_manual_copilot_noop_leaves_pending_untouched():
    """
    given: `_manual_copilot_active` is False and a pending ""-valued entry
    when:  `_preempt_manual_copilot` runs
    then:  the early return fires — the entry is NOT stale-marked
    """
    bridge = KTalkBridge()
    bridge._copilot_pending_requests["req-m"] = ""

    await bridge._preempt_manual_copilot()

    assert bridge._copilot_pending_requests["req-m"] == ""


@pytest.mark.asyncio
async def test_late_manual_reply_after_preemption_gets_terminal_echo():
    """
    given: an armed manual mirror with a pending ""-valued request, one
           extension and one subscriber client
    when:  a conference `meeting_join` triggers `_preempt_manual_copilot`
           and the manual reply arrives afterwards
    then:  the entry was stale-marked ("stale:"); the reply is NOT rendered
           as copilot_reply; both audiences receive ONE terminal
           copilot_status error carrying the MANUAL-session message
           ("прервана") — never the meeting-ended one, because no meeting
           ever existed for the question (end-to-end DW-15 + DW-8)
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_meeting_storage_mock(meeting_id="m-conf"))
    ext = RecordingClient()
    sub = RecordingClient()
    bridge._connected_clients.add(ext)
    bridge._subscriber_clients.add(sub)
    bridge._manual_copilot_active = True
    bridge._copilot_pending_requests["req-manual"] = ""

    await bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/room",
        "title": "Standup",
        "platform": "ktalk",
    })

    assert bridge._copilot_pending_requests["req-manual"] == "stale:"

    ext.sent.clear()
    sub.sent.clear()
    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-manual",
        "kind": "manual",
        "text": "поздний ответ ручной сессии",
        "meeting_id": None,
    })

    for client in (ext, sub):
        assert _copilot_frames(client, "copilot_reply") == []
        errors = _copilot_frames(client, "copilot_status", "error")
        assert len(errors) == 1
        assert errors[0]["request_id"] == "req-manual"
        assert errors[0]["message"] == _MSG_MANUAL_PREEMPTED
        assert errors[0]["message"] != _MSG_MEETING_ENDED
    assert "req-manual" not in bridge._copilot_pending_requests


@pytest.mark.asyncio
async def test_given_lifecycle_send_reenters_when_manual_reply_arrives_then_reply_is_stale():
    """
    given: an armed manual mirror with a pending request and a channel
           lifecycle recipient that re-enters the bridge
    when:  that recipient handles the late manual reply from inside its
           `meeting_started` send
    then:  preemption has already stale-marked the request, so no
           copilot_reply leaks into the conference chat and the terminal
           manual-session error reaches both UI audiences
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()
    bridge.set_meeting_storage(_meeting_storage_mock(meeting_id="m-conf"))
    bridge._manual_copilot_active = True
    bridge._copilot_pending_requests["req-reentrant"] = ""

    class ReplyingLifecycleClient(RecordingClient):
        def __init__(self):
            super().__init__()
            self.replied = False

        async def send(self, message: str):
            await super().send(message)
            frame = json.loads(message)
            if frame.get("type") == "meeting_started" and not self.replied:
                self.replied = True
                await bridge._process_message({
                    "type": "copilot_channel_reply",
                    "request_id": "req-reentrant",
                    "kind": "manual",
                    "text": "слишком поздний ответ",
                    "meeting_id": None,
                })

    channel = ReplyingLifecycleClient()
    bridge._copilot_channel_clients.add(channel)

    await bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/room",
        "title": "Standup",
        "platform": "ktalk",
    })

    assert channel.replied is True
    assert len(_copilot_frames(channel, "meeting_started")) == 1
    for client in (ext, sub):
        assert _copilot_frames(client, "copilot_reply") == []
        errors = _copilot_frames(client, "copilot_status", "error")
        assert len(errors) == 1
        assert errors[0]["request_id"] == "req-reentrant"
        assert errors[0]["message"] == _MSG_MANUAL_PREEMPTED
    assert "req-reentrant" not in bridge._copilot_pending_requests


@pytest.mark.asyncio
async def test_given_channel_send_reenters_when_conference_joins_then_pending_stays_stale():
    """
    given: a manual chat relay with no active conference and a channel client
           that starts a conference while receiving `copilot_user_message`
    when:  the chat relay resumes and its late reply later arrives
    then:  pending bookkeeping was visible before the send await, becomes
           "stale:" during preemption, is not overwritten on resume, and only
           the terminal manual-session error reaches both UI audiences
    """
    bridge, ext, sub = _bridge_with_extension_and_subscriber()
    storage = Mock()
    storage.current_meeting_path = Path("/tmp/orphan.jsonl")
    storage.current_meeting_id = "orphan"
    source = {"value": "orphan"}
    storage._load_meta = Mock(
        side_effect=lambda: {"source": source["value"], "title": "Standup"}
    )

    def _on_join(url, *args, **kwargs):
        source["value"] = "ktalk"
        storage.current_meeting_path = Path("/tmp/m-conf.jsonl")
        storage.current_meeting_id = "m-conf"
        return "m-conf"

    storage.on_meeting_join = Mock(side_effect=_on_join)
    storage.get_current_transcript = Mock(return_value=[])
    storage.get_current_meeting_id = Mock(
        side_effect=lambda: storage.current_meeting_id
    )
    bridge.set_meeting_storage(storage)
    bridge._manual_copilot_active = True

    class JoiningChannelClient(RecordingClient):
        def __init__(self):
            super().__init__()
            self.pending_before_join = None
            self.pending_after_join = None
            self.joined = False

        async def send(self, message: str):
            await super().send(message)
            frame = json.loads(message)
            if frame.get("type") == "copilot_user_message" and not self.joined:
                self.joined = True
                self.pending_before_join = bridge._copilot_pending_requests.get(
                    "req-race"
                )
                await bridge._process_message({
                    "type": "meeting_join",
                    "url": "https://ktalk.test/room",
                    "title": "Standup",
                    "platform": "ktalk",
                })
                self.pending_after_join = bridge._copilot_pending_requests.get(
                    "req-race"
                )

    channel = JoiningChannelClient()
    bridge._copilot_channel_clients.add(channel)

    await bridge._process_message(
        {
            "type": "copilot_chat_message",
            "request_id": "req-race",
            "text": "ручной вопрос",
        },
        websocket=sub,
    )

    assert channel.pending_before_join == ""
    assert channel.pending_after_join == "stale:"
    assert bridge._copilot_pending_requests["req-race"] == "stale:"
    forwarded = _copilot_frames(channel, "copilot_user_message")
    assert len(forwarded) == 1
    assert forwarded[0]["meeting_id"] is None

    ext.sent.clear()
    sub.sent.clear()
    await bridge._process_message({
        "type": "copilot_channel_reply",
        "request_id": "req-race",
        "kind": "manual",
        "text": "поздний ответ",
        "meeting_id": None,
    })

    for client in (ext, sub):
        assert _copilot_frames(client, "copilot_reply") == []
        errors = _copilot_frames(client, "copilot_status", "error")
        assert len(errors) == 1
        assert errors[0]["request_id"] == "req-race"
        assert errors[0]["message"] == _MSG_MANUAL_PREEMPTED
    assert "req-race" not in bridge._copilot_pending_requests


@pytest.mark.asyncio
async def test_given_blocking_preemption_notice_when_join_runs_then_lifecycle_arrives_first():
    """
    given: an armed manual mirror and a subscriber that blocks while receiving
           the `manual_preempted` notice
    when:  a conference meeting joins and reaches that blocked send
    then:  channel, extension, and supervisor recipients already received
           `meeting_started`; subscriber I/O is not on the lifecycle path
    """
    bridge = KTalkBridge()
    bridge.set_meeting_storage(_meeting_storage_mock(meeting_id="m-conf"))
    channel = RecordingClient()
    extension = RecordingClient()
    supervisor = RecordingClient()
    bridge._copilot_channel_clients.add(channel)
    bridge._connected_clients.add(extension)
    bridge._supervisor_clients.add(supervisor)
    bridge._manual_copilot_active = True
    bridge._copilot_pending_requests["req-manual"] = ""

    class BlockingSubscriber(RecordingClient):
        def __init__(self):
            super().__init__()
            self.notice_started = asyncio.Event()
            self.release_notice = asyncio.Event()

        async def send(self, message: str):
            await super().send(message)
            frame = json.loads(message)
            if frame.get("status") == "manual_preempted":
                self.notice_started.set()
                await self.release_notice.wait()

    subscriber = BlockingSubscriber()
    bridge._subscriber_clients.add(subscriber)

    join_task = asyncio.create_task(bridge._process_message({
        "type": "meeting_join",
        "url": "https://ktalk.test/room",
        "title": "Standup",
        "platform": "ktalk",
    }))
    await asyncio.wait_for(subscriber.notice_started.wait(), timeout=1.0)
    try:
        for recipient in (channel, extension, supervisor):
            started = _copilot_frames(recipient, "meeting_started")
            assert len(started) == 1
            assert started[0]["meeting_id"] == "m-conf"
        assert bridge._manual_copilot_active is False
        assert bridge._copilot_pending_requests["req-manual"] == "stale:"
        assert join_task.done() is False
    finally:
        subscriber.release_notice.set()
        await asyncio.wait_for(join_task, timeout=1.0)
