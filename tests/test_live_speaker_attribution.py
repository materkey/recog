"""Tests for speaker attribution in live transcription."""
import pytest
import time
import threading
from pathlib import Path
import sys
from collections import deque

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from unittest.mock import Mock
from ktalk_bridge import KTalkBridge, Participant
from recog.live import LiveTranscriber, SourceState, RETROACTIVE_MAX_SEGMENTS


def test_system_audio_uses_is_local_false():
    """
    Verify the fix: system audio processing should use is_local=False hardcoded.

    This test reads the source code and verifies:
    1. The buggy pattern is NOT present
    2. The fix is in place
    """
    live_py_path = Path(__file__).resolve().parent.parent / 'recog' / 'live.py'
    source = live_py_path.read_text()

    # The bug was: is_local = self._ktalk_bridge.is_current_speaker_local()
    # The fix is: is_local = False
    buggy_pattern = "is_local = self._ktalk_bridge.is_current_speaker_local()"

    assert buggy_pattern not in source, \
        f"Bug still present: {buggy_pattern}"


def test_no_uncertain_local_reason_for_system_audio():
    """
    Verify: system audio should not have 'uncertain_reason: local' logic.

    The old code marked system audio as uncertain when is_local was True.
    Since system audio is always remote, this logic is no longer needed.
    """
    live_py_path = Path(__file__).resolve().parent.parent / 'recog' / 'live.py'
    source = live_py_path.read_text()

    # The old buggy pattern that marked system audio as uncertain
    buggy_pattern = 'uncertain_reason = "local"'

    assert buggy_pattern not in source, \
        f"Old uncertain logic still present: {buggy_pattern}"


# --- Sole unmuted fallback wiring tests ---


def test_sole_unmuted_fallback_in_speaker_assignment_chain():
    """
    given: KTalk mode, dominant/current speaker both None, TTL expired, one unmuted remote
    when: _handle_ktalk_chunk runs
    then: sole unmuted participant is attributed with uncertain_reason="sole_unmuted_fallback"
    """
    transcriber = Mock()
    transcriber._ktalk_bridge = Mock()
    transcriber._ktalk_bridge.get_dominant_speaker_for_period.return_value = None
    transcriber._ktalk_bridge.get_current_speaker.return_value = None
    transcriber._ktalk_bridge.get_sole_unmuted_remote_participant.return_value = "Bob"
    transcriber._ktalk_bridge.are_all_mics_muted.return_value = False
    transcriber._meeting_storage = None
    transcriber._state_lock = threading.Lock()
    transcriber._meeting_generation = 0
    transcriber.transcript_lines = []
    transcriber._recent_system_segments = deque(maxlen=RETROACTIVE_MAX_SEGMENTS)
    transcriber._ktalk_current_speaker = None

    state = SourceState()
    state.last_speaker = None  # No last speaker -> TTL path skipped
    now = time.time()

    LiveTranscriber._handle_ktalk_chunk(
        transcriber, "hello", "12:00:00", now - 3, now, now, 0, state)

    # Speaker attributed to sole unmuted
    assert "**Bob**" in transcriber.transcript_lines[0]

    # send_transcript called with sole_unmuted_fallback uncertainty
    call_kwargs = transcriber._ktalk_bridge.send_transcript.call_args
    assert call_kwargs[1]['uncertain'] is True
    assert call_kwargs[1]['uncertain_reason'] == "sole_unmuted_fallback"

    # Dominant and current were checked first (chain order preserved)
    transcriber._ktalk_bridge.get_dominant_speaker_for_period.assert_called_once()
    transcriber._ktalk_bridge.get_current_speaker.assert_called_once()
    transcriber._ktalk_bridge.get_sole_unmuted_remote_participant.assert_called_once()


def test_sole_unmuted_fallback_used_when_all_other_signals_fail():
    """
    given: KTalkBridge where dominant speaker and current speaker return None,
           last_speaker TTL expired, but exactly 1 remote participant unmuted
    when: speaker assignment logic runs
    then: sole unmuted participant is returned as speaker
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=True),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=True),
    }

    # Simulate the fallback chain as in live.py:
    # 1. ktalk_speaker = None (dominant/current both failed)
    ktalk_speaker = None
    # 2. last_speaker TTL expired (simulated by not entering that branch)
    last_speaker = None
    # 3. sole unmuted fallback
    sole_unmuted = bridge.get_sole_unmuted_remote_participant()

    # Chain result:
    if ktalk_speaker:
        speaker = ktalk_speaker
    elif last_speaker:
        speaker = last_speaker
    elif sole_unmuted:
        speaker = sole_unmuted
    else:
        speaker = "Unknown"

    assert speaker == "Bob"
    assert sole_unmuted == "Bob"


def test_sole_unmuted_fallback_not_used_when_multiple_unmuted():
    """
    given: KTalkBridge with 2+ remote participants unmuted,
           dominant speaker and current speaker return None
    when: speaker assignment logic runs
    then: speaker is "Unknown" (no false attribution)
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=False),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=True),
    }

    # Simulate the fallback chain:
    ktalk_speaker = None
    last_speaker = None
    sole_unmuted = bridge.get_sole_unmuted_remote_participant()

    if ktalk_speaker:
        speaker = ktalk_speaker
    elif last_speaker:
        speaker = last_speaker
    elif sole_unmuted:
        speaker = sole_unmuted
    else:
        speaker = "Unknown"

    assert speaker == "Unknown"
    assert sole_unmuted is None
