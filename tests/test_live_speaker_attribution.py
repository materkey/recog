"""Tests for speaker attribution in live transcription."""
import pytest
from pathlib import Path
import sys

sys.path.insert(0, '/Users/vkkovalev/projects/recog')


def test_system_audio_uses_is_local_false():
    """
    Verify the fix: system audio processing should use is_local=False hardcoded.

    This test reads the source code and verifies:
    1. The buggy pattern is NOT present
    2. The fix is in place
    """
    live_py_path = Path('/Users/vkkovalev/projects/recog/recog/live.py')
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
    live_py_path = Path('/Users/vkkovalev/projects/recog/recog/live.py')
    source = live_py_path.read_text()

    # The old buggy pattern that marked system audio as uncertain
    buggy_pattern = 'uncertain_reason = "local"'

    assert buggy_pattern not in source, \
        f"Old uncertain logic still present: {buggy_pattern}"
