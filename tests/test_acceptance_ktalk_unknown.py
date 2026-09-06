"""
Acceptance tests for the Unknown speaker fix in KTalk meetings.

Verifies end-to-end scenarios:
1. Speaker switch with mute → sole unmuted fallback prevents Unknown
2. Multi-speaker scenario: 2+ unmuted → Unknown preserved (no false attribution)
3. Retroactive correction works end-to-end
4. Orphan meetings (no extension) behavior unchanged
5. Mic/local speaker unaffected
"""
import json
import time
import pytest
from pathlib import Path
from collections import deque
from unittest.mock import Mock, MagicMock, patch
import threading

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.ktalk_bridge import KTalkBridge, Participant
from recog.live import (LiveTranscriber, SourceState, RecentSegment,
                        RETROACTIVE_MAX_SEGMENTS, RETROACTIVE_MAX_SECONDS)
from recog.meeting_storage import MeetingStorage


# --- Scenario 1: Speaker switch with mute → sole unmuted fallback ---


def test_speaker_switch_with_mute_sole_unmuted_fallback():
    """
    given: KTalk meeting, Speaker A was talking, then mutes, Speaker B unmutes (sole unmuted)
           dominant speaker detection lags (returns None)
    when: audio chunk arrives during the lag
    then: sole unmuted fallback returns Speaker B (no Unknown)

    This simulates the root cause chain from the investigation.
    """
    bridge = KTalkBridge()
    # State after A mutes and B unmutes but Jitsi hasn't switched dominant speaker
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=True),  # just muted
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),   # just unmuted, speaking
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=True),
    }

    # Simulate the fallback chain from live.py
    # 1. Dominant speaker: None (Jitsi lagging)
    ktalk_speaker = bridge.get_dominant_speaker_for_period(time.time() - 3, time.time())
    assert ktalk_speaker is None  # No events in history

    # 2. Current speaker: None
    current = bridge.get_current_speaker()
    # May or may not be None depending on state, but if it is:
    if not ktalk_speaker:
        ktalk_speaker = current

    # 3. Simulate TTL expiry (last_speaker was Alice but she's muted)
    is_muted = bridge.is_speaker_muted("Alice")
    assert is_muted  # Alice is muted, so TTL fallback blocked

    # 4. Sole unmuted fallback
    sole_unmuted = bridge.get_sole_unmuted_remote_participant()
    assert sole_unmuted == "Bob"

    # End result: speaker = Bob, not Unknown
    speaker = ktalk_speaker or sole_unmuted or "Unknown"
    assert speaker == "Bob"


def test_speaker_switch_three_participants_one_unmuted():
    """
    given: 5 remote participants, only 1 has mic unmuted after a switch
    when: all fallbacks fail
    then: sole unmuted returns the single unmuted participant
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=True),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=True),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=False),  # sole unmuted
        "p4": Participant(id="p4", name="Dave", is_local=False, mic_muted=True),
        "p5": Participant(id="p5", name="Eve", is_local=False, mic_muted=True),
    }

    sole = bridge.get_sole_unmuted_remote_participant()
    assert sole == "Carol"


# --- Scenario 2: Multi-speaker → Unknown preserved ---


def test_multi_speaker_two_unmuted_preserves_unknown():
    """
    given: 2 remote participants unmuted, dominant speaker returns None
    when: fallback chain runs
    then: speaker is "Unknown" — no false attribution
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=False),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=True),
    }

    # All fallbacks fail
    ktalk_speaker = None
    last_speaker_valid = False
    sole_unmuted = bridge.get_sole_unmuted_remote_participant()
    assert sole_unmuted is None

    # Result: Unknown
    if ktalk_speaker:
        speaker = ktalk_speaker
    elif last_speaker_valid:
        speaker = "last"
    elif sole_unmuted:
        speaker = sole_unmuted
    else:
        speaker = "Unknown"

    assert speaker == "Unknown"


def test_multi_speaker_all_unmuted_preserves_unknown():
    """
    given: all 3 remote participants unmuted
    when: sole unmuted check
    then: returns None (ambiguous)
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=False),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=False),
    }

    assert bridge.get_sole_unmuted_remote_participant() is None


# --- Scenario 3: Retroactive correction end-to-end ---


def test_retroactive_correction_end_to_end(tmp_path):
    """
    given: meeting with segments Alice → Unknown → Unknown → Unknown → Bob
    when: Bob is identified (triggers retroactive correction)
    then: all 3 Unknown segments corrected to Bob in storage, transcript, and extension
    """
    # Setup real MeetingStorage
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(ktalk=True)

    # Create segments
    seg_ids = []
    speakers = ["Alice", "Unknown", "Unknown", "Unknown", "Bob"]
    for s in speakers:
        seg_id = storage.append_segment(
            speaker=s, speaker_id=s, text=f"text by {s}",
            is_local=False, uncertain=(s == "Unknown"),
            uncertain_reason="no_speaker" if s == "Unknown" else ""
        )
        seg_ids.append(seg_id)

    # Setup mock transcriber with real data
    transcriber = Mock()
    transcriber._state_lock = threading.Lock()
    transcriber.transcript_lines = [
        f"\n**{s}** *[12:00:{i*3:02d}]*: text by {s}\n"
        for i, s in enumerate(speakers)
    ]

    now = time.time()
    transcriber._recent_system_segments = deque([
        RecentSegment(
            speaker=s, chunk_time=now - (len(speakers) - 1 - i) * 3,
            line_idx=i, meeting_segment_id=seg_ids[i],
            is_local=False, timestamp_display=f"12:00:{i*3:02d}",
            text=f"text by {s}"
        )
        for i, s in enumerate(speakers)
    ], maxlen=RETROACTIVE_MAX_SEGMENTS)

    transcriber._meeting_storage = storage
    mock_bridge = Mock()
    transcriber._ktalk_bridge = mock_bridge

    # Trigger retroactive correction
    from recog.live import LiveTranscriber
    LiveTranscriber._retroactive_correct_unknown_run(transcriber, "Bob", now)

    # Verify transcript_lines corrected
    for i in [1, 2, 3]:
        assert "**Bob**" in transcriber.transcript_lines[i]
        assert "**Unknown**" not in transcriber.transcript_lines[i]

    # Alice's line unchanged
    assert "**Alice**" in transcriber.transcript_lines[0]

    # Verify JSONL storage corrected
    transcript_path = storage.current_meeting_path / "transcript.jsonl"
    lines = transcript_path.read_text(encoding="utf-8").strip().split("\n")
    segments = [json.loads(line) for line in lines]

    assert segments[0]["speaker"] == "Alice"
    for i in [1, 2, 3]:
        assert segments[i]["speaker"] == "Bob"
        assert segments[i]["uncertain"] is False
    assert segments[4]["speaker"] == "Bob"

    # Verify extension got corrections
    assert mock_bridge.send_transcript.call_count == 3
    for c in mock_bridge.send_transcript.call_args_list:
        assert c[0][0] == "Bob"
        assert c[1]["correction"] is True


# --- Scenario 4: Orphan meetings unchanged ---


def test_orphan_meeting_no_sole_unmuted_check():
    """
    given: non-KTalk mode (orphan meeting, no extension)
    when: system audio is transcribed
    then: speech is saved immediately with a generic speaker and its audio offset
    """
    transcriber = Mock()
    transcriber._meeting_storage = Mock()
    transcriber._meeting_generation = 0
    transcriber._state_lock = threading.Lock()
    transcriber.transcript_lines = []

    state = SourceState(audio_offset=1250)
    now = time.time()

    LiveTranscriber._handle_unattributed_chunk(
        transcriber, "hello", "12:00:00", now, 0, state)

    assert "**speaker**" in transcriber.transcript_lines[0]
    transcriber._meeting_storage.append_segment.assert_called_once_with(
        speaker="speaker", speaker_id="speaker", text="hello",
        is_local=False, is_continuation=False, uncertain=True,
        uncertain_reason="no_conference_identity", audio_offset_ms=1250,
    )


def test_orphan_meeting_no_retroactive_correction():
    """
    given: non-KTalk mode (orphan meeting)
    when: unattributed system speech is saved
    then: retroactive correction is not triggered (only available in KTalk handler)
    """
    transcriber = Mock()
    transcriber._meeting_storage = None
    transcriber._meeting_generation = 0
    transcriber._state_lock = threading.Lock()
    transcriber.transcript_lines = []

    state = SourceState()
    now = time.time()

    LiveTranscriber._handle_unattributed_chunk(
        transcriber, "hello", "12:00:00", now, 0, state)

    # Retroactive correction is NOT called (only in _handle_ktalk_chunk)
    transcriber._retroactive_correct_unknown_run.assert_not_called()


# --- Scenario 5: Mic/local speaker unaffected ---


def test_mic_speaker_path_unchanged():
    """
    given: mic audio (is_mic=True)
    when: mic chunk handler processes audio
    then: speaker is always mic_speaker_name, is_local=True, no sole_unmuted or retroactive
    """
    transcriber = Mock()
    transcriber.mic_speaker_name = "TestUser"
    transcriber._ktalk_bridge = Mock()
    transcriber._meeting_storage = Mock()
    transcriber._meeting_generation = 0
    transcriber.transcript_lines = []

    state = SourceState()
    now = time.time()

    LiveTranscriber._handle_mic_chunk(
        transcriber, "hello", "12:00:00", now, 0, state)

    # Speaker is mic_speaker_name
    assert "**TestUser**" in transcriber.transcript_lines[0]

    # is_local=True in bridge call
    bridge_call = transcriber._ktalk_bridge.send_transcript.call_args
    assert bridge_call[1]['is_local'] is True

    # is_local=True and correct speaker in storage call
    storage_call = transcriber._meeting_storage.append_segment.call_args
    assert storage_call[1]['is_local'] is True
    assert storage_call[1]['speaker'] == "TestUser"
    assert storage_call[1]['uncertain'] is False

    # No sole_unmuted or retroactive correction
    transcriber._ktalk_bridge.get_sole_unmuted_remote_participant.assert_not_called()
    transcriber._retroactive_correct_unknown_run.assert_not_called()


def test_local_participant_excluded_from_sole_unmuted():
    """
    given: only the local participant has mic unmuted, 2 remote participants muted
    when: sole unmuted check runs
    then: returns None (local participant excluded)
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p_local": Participant(id="p_local", name="Me", is_local=True, mic_muted=False),
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=True),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=True),
    }

    assert bridge.get_sole_unmuted_remote_participant() is None


def test_correction_flag_in_send_transcript():
    """
    given: KTalkBridge with a connected client
    when: send_transcript is called with correction=True
    then: the actual JSON message sent to the client includes correction: true
    """
    import asyncio

    bridge = KTalkBridge()
    loop = Mock()
    bridge._loop = loop
    mock_client = Mock()
    bridge._connected_clients = {mock_client}

    bridge.send_transcript(
        "Alice", "hello", "12:00:00",
        is_continuation=False, is_local=False,
        uncertain=False, uncertain_reason="",
        correction=True
    )

    # Capture the actual message that was passed to asyncio.run_coroutine_threadsafe
    assert loop.call_count == 0  # loop itself isn't called, run_coroutine_threadsafe is

    # The bridge calls asyncio.run_coroutine_threadsafe(client.send(message), loop)
    # Since we mocked the loop, the coroutine was created but not awaited.
    # Verify by checking that client.send was prepared with the correct message.
    # asyncio.run_coroutine_threadsafe is a module-level function, so let's
    # verify the message directly by inspecting what json.dumps produced
    # via the bridge's internal logic.
    #
    # Re-invoke with a patched json.dumps to capture the actual message:
    captured_messages = []
    original_dumps = json.dumps
    def capturing_dumps(obj, **kwargs):
        captured_messages.append(obj)
        return original_dumps(obj, **kwargs)

    with patch('recog.ktalk_bridge.json.dumps', side_effect=capturing_dumps):
        bridge.send_transcript(
            "Alice", "hello", "12:00:00",
            is_continuation=False, is_local=False,
            uncertain=False, uncertain_reason="",
            correction=True
        )

    assert len(captured_messages) == 1
    msg = captured_messages[0]
    assert msg["type"] == "transcript"
    assert msg["speaker"] == "Alice"
    assert msg["correction"] is True


def test_fallback_chain_order_behavioral():
    """
    given: KTalk mode with different fallback scenarios
    when: _handle_ktalk_chunk processes audio
    then: chain follows priority: dominant > current > TTL > sole_unmuted > Unknown
    """
    def make_transcriber():
        t = Mock()
        t._ktalk_bridge = Mock()
        t._ktalk_bridge.are_all_mics_muted.return_value = False
        t._meeting_storage = None
        t._state_lock = threading.Lock()
        t._meeting_generation = 0
        t.transcript_lines = []
        t._recent_system_segments = deque(maxlen=RETROACTIVE_MAX_SEGMENTS)
        t._ktalk_current_speaker = None
        return t

    now = time.time()

    # Tier 1: dominant speaker available -> used
    t = make_transcriber()
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = "Alice"
    state = SourceState()
    LiveTranscriber._handle_ktalk_chunk(t, "hello", "12:00:00", now-3, now, now, 0, state)
    assert "**Alice**" in t.transcript_lines[0]
    # Didn't need tier 2
    t._ktalk_bridge.get_current_speaker.assert_not_called()

    # Tier 2: no dominant, current available -> used
    t = make_transcriber()
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = None
    t._ktalk_bridge.get_current_speaker.return_value = "Bob"
    state = SourceState()
    LiveTranscriber._handle_ktalk_chunk(t, "hello", "12:00:00", now-3, now, now, 0, state)
    assert "**Bob**" in t.transcript_lines[0]

    # Tier 3: no dominant/current, TTL valid -> reuses last speaker
    t = make_transcriber()
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = None
    t._ktalk_bridge.get_current_speaker.return_value = None
    t._ktalk_bridge.is_speaker_muted.return_value = False
    state = SourceState()
    state.last_speaker = "Carol"
    state.last_speech_time = now - 1  # Within TTL
    LiveTranscriber._handle_ktalk_chunk(t, "hello", "12:00:00", now-3, now, now, 0, state)
    assert "**Carol**" in t.transcript_lines[0]
    # Didn't need tier 4
    t._ktalk_bridge.get_sole_unmuted_remote_participant.assert_not_called()

    # Tier 4: all above fail, sole unmuted -> used
    t = make_transcriber()
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = None
    t._ktalk_bridge.get_current_speaker.return_value = None
    t._ktalk_bridge.get_sole_unmuted_remote_participant.return_value = "Dave"
    state = SourceState()
    state.last_speaker = None
    LiveTranscriber._handle_ktalk_chunk(t, "hello", "12:00:00", now-3, now, now, 0, state)
    assert "**Dave**" in t.transcript_lines[0]

    # Tier 5: everything fails -> Unknown
    t = make_transcriber()
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = None
    t._ktalk_bridge.get_current_speaker.return_value = None
    t._ktalk_bridge.get_sole_unmuted_remote_participant.return_value = None
    state = SourceState()
    state.last_speaker = None
    LiveTranscriber._handle_ktalk_chunk(t, "hello", "12:00:00", now-3, now, now, 0, state)
    assert "**Unknown**" in t.transcript_lines[0]


def test_meeting_boundary_clears_recent_segments():
    """
    given: live.py source code
    when: meeting changes (_meeting_generation incremented)
    then: _recent_system_segments is cleared (prevents cross-meeting correction)
    """
    live_py_path = Path(__file__).resolve().parent.parent / 'recog' / 'live.py'
    source = live_py_path.read_text()

    # Both must be present
    assert "_recent_system_segments.clear()" in source
    assert "_meeting_generation += 1" in source

    # They must be near each other
    gen_pos = source.index("_meeting_generation += 1")
    clear_pos = source.index("_recent_system_segments.clear()")
    assert abs(clear_pos - gen_pos) < 200
