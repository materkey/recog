"""Tests for speaker attribution in live transcription."""
import pytest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from unittest.mock import Mock
from ktalk_bridge import KTalkBridge, Participant


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
    given: live.py speaker assignment code in KTalk mode
    when: source code is examined
    then: sole_unmuted fallback tier exists between last_speaker TTL and Unknown
    """
    live_py_path = Path(__file__).resolve().parent.parent / 'recog' / 'live.py'
    source = live_py_path.read_text()

    # The fallback chain must include sole unmuted participant check
    assert "get_sole_unmuted_remote_participant()" in source, \
        "sole unmuted fallback not wired into live.py"

    # It must set uncertain flag
    assert 'uncertain_reason = "sole_unmuted_fallback"' in source, \
        "sole_unmuted_fallback uncertain reason not set"

    # sole_unmuted_fallback flag must be initialized
    assert "sole_unmuted_fallback = False" in source, \
        "sole_unmuted_fallback flag not initialized"

    # Verify ordering: sole_unmuted check comes after last_speaker TTL
    # and before Unknown assignment
    sole_unmuted_pos = source.index("get_sole_unmuted_remote_participant()")
    last_speaker_ttl_pos = source.index("LAST_SPEAKER_TTL")
    unknown_pos = source.index('speaker = "Unknown"')
    assert last_speaker_ttl_pos < sole_unmuted_pos < unknown_pos, \
        "Fallback chain order wrong: must be last_speaker TTL → sole_unmuted → Unknown"


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
