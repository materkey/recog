"""End-to-end diarization tests using KTalk virtual conferences.

Each test launches real Chromium browsers with timed fake audio files, joins a
KTalk room, and verifies that the live transcription pipeline correctly
attributes speech to the expected speakers.

Speaker timing is baked into the WAV files (speech + silence at scheduled
offsets) because KTalk mute/unmute does NOT stop Chromium's fake audio stream.
Speaker identity is injected via WebSocket to the bridge because Jitsi's
dominant speaker detection doesn't work with fake audio.

Requirements:
- SSO_LOGIN, SSO_PASSWORD environment variables.
- BlackHole virtual audio device installed.
- KTalk speaker extension built (npm run build in kontur-diarize/ktalk-speaker-extension/).
- Timed fixture WAV files (run tests/e2e/prepare_fixtures.py).
"""
from pathlib import Path

import pytest

from tests.e2e.ktalk_orchestrator import KTalkOrchestrator, SpeakerWindow

_FIXTURES = Path(__file__).parent / "fixtures"


@pytest.mark.e2e
def test_two_speakers_alternating():
    """Guest A speaks 0-20s, Guest B speaks 20-40s; expect 2+ distinct speakers."""
    orchestrator = KTalkOrchestrator(
        guest_names=["GuestA", "GuestB"],
        guest_audio_files=[
            str(_FIXTURES / "timed_a_first.wav"),   # speech 0-20s, then silence
            str(_FIXTURES / "timed_b_second.wav"),   # silence 0-20s, then speech 20-40s
        ],
        speaker_windows=[
            SpeakerWindow("GuestA", start=0, end=20),
            SpeakerWindow("GuestB", start=20, end=40),
        ],
        record_duration=50.0,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"
    assert len(result.transcript_lines) > 0, "Transcript should not be empty"
    assert len(result.speakers_found) >= 2, (
        f"Expected at least 2 speakers, found: {result.speakers_found}"
    )


@pytest.mark.e2e
def test_ttl_regression_speaker_resets_after_gap():
    """A speaks 0-15s, silence 15-30s (>TTL), B speaks 30-50s.

    Regression test: after a gap longer than LAST_SPEAKER_TTL (10s), late speech
    must not cascade to Guest A — Guest B should get its own attribution line.
    """
    orchestrator = KTalkOrchestrator(
        guest_names=["GuestA", "GuestB"],
        guest_audio_files=[
            str(_FIXTURES / "timed_a_early.wav"),  # speech 0-15s, then silence
            str(_FIXTURES / "timed_b_late.wav"),    # silence 0-30s, then speech 30-50s
        ],
        speaker_windows=[
            SpeakerWindow("GuestA", start=0, end=15),
            # 15s gap (>10s TTL)
            SpeakerWindow("GuestB", start=30, end=50),
        ],
        record_duration=55.0,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"

    guest_a_lines = [l for l in result.transcript_lines if "GuestA" in l]
    guest_b_lines = [l for l in result.transcript_lines if "GuestB" in l]

    assert len(guest_a_lines) > 0, "GuestA should have transcript lines"
    assert len(guest_b_lines) > 0, (
        "GuestB should have its own lines; all speech must not cascade to GuestA after TTL gap"
    )


@pytest.mark.e2e
def test_long_pause_between_speakers():
    """Guest A speaks 0-15s, 15s silence, Guest B speaks 30-50s.

    Late lines must not all be attributed to Guest A.
    """
    orchestrator = KTalkOrchestrator(
        guest_names=["GuestA", "GuestB"],
        guest_audio_files=[
            str(_FIXTURES / "timed_a_early.wav"),  # speech 0-15s
            str(_FIXTURES / "timed_b_late.wav"),    # speech 30-50s
        ],
        speaker_windows=[
            SpeakerWindow("GuestA", start=0, end=15),
            SpeakerWindow("GuestB", start=30, end=50),
        ],
        record_duration=55.0,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"

    # At least one late line should NOT be attributed to GuestA
    late_guest_a_only = len(result.transcript_lines) > 0 and all(
        "GuestA" in l for l in result.transcript_lines if l.strip()
    )
    assert not late_guest_a_only, (
        "All transcript lines are attributed to GuestA; GuestB attribution missing after pause"
    )


@pytest.mark.e2e
def test_three_speakers():
    """Three guests speak in sequence; expect 3+ distinct speakers."""
    orchestrator = KTalkOrchestrator(
        guest_names=["GuestA", "GuestB", "GuestC"],
        guest_audio_files=[
            str(_FIXTURES / "timed_a_seq.wav"),  # speech 0-15s
            str(_FIXTURES / "timed_b_seq.wav"),  # speech 15-30s
            str(_FIXTURES / "timed_c_seq.wav"),  # speech 30-45s
        ],
        speaker_windows=[
            SpeakerWindow("GuestA", start=0, end=15),
            SpeakerWindow("GuestB", start=15, end=30),
            SpeakerWindow("GuestC", start=30, end=45),
        ],
        record_duration=55.0,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"
    assert len(result.speakers_found) >= 3, (
        f"Expected at least 3 speakers, found: {result.speakers_found}"
    )
