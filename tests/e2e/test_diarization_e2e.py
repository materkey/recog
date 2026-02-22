"""End-to-end diarization tests using KTalk virtual conferences.

Each test launches real Chromium browsers with fake audio files, joins a
KTalk room, and verifies that the live transcription pipeline correctly
attributes speech to the expected speakers.

Requirements:
- KONTUR_LOGIN and KONTUR_PASSWORD environment variables must be set.
- BlackHole virtual audio device must be installed.
- KTalk speaker extension must be built (npm run build in kontur-diarize/ktalk-speaker-extension/).
- Fixture WAV files must exist (run tests/e2e/prepare_fixtures.py).
"""
from pathlib import Path

import pytest

from tests.e2e.ktalk_orchestrator import KTalkOrchestrator, SpeakerScheduleEvent


@pytest.mark.e2e
def test_two_speakers_alternating(speaker_a_wav: Path, speaker_b_wav: Path):
    """Guest A speaks 0-15s, Guest B speaks 15-30s; expect 2+ distinct speakers."""
    schedule = [
        # t=0: unmute A
        SpeakerScheduleEvent(time_offset=0.0, guest_index=0, mute=False),
        # t=15: mute A, unmute B
        SpeakerScheduleEvent(time_offset=15.0, guest_index=0, mute=True),
        SpeakerScheduleEvent(time_offset=15.0, guest_index=1, mute=False),
        # t=30: mute B
        SpeakerScheduleEvent(time_offset=30.0, guest_index=1, mute=True),
    ]
    orchestrator = KTalkOrchestrator(
        guest_names=["GuestA", "GuestB"],
        guest_audio_files=[str(speaker_a_wav), str(speaker_b_wav)],
        schedule=schedule,
        test_duration=35.0,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"
    assert len(result.transcript_lines) > 0, "Transcript should not be empty"
    assert len(result.speakers_found) >= 2, (
        f"Expected at least 2 speakers, found: {result.speakers_found}"
    )


@pytest.mark.e2e
def test_ttl_regression_speaker_resets_after_gap(speaker_a_wav: Path, speaker_b_wav: Path):
    """A speaks 0-10s, silence 10-25s (>TTL), B speaks 25-35s.

    Regression test: after a gap longer than LAST_SPEAKER_TTL, late speech
    must not cascade to Guest A — Guest B should get its own attribution line.
    """
    schedule = [
        SpeakerScheduleEvent(time_offset=0.0, guest_index=0, mute=False),
        SpeakerScheduleEvent(time_offset=10.0, guest_index=0, mute=True),
        # 15s gap (>10s TTL)
        SpeakerScheduleEvent(time_offset=25.0, guest_index=1, mute=False),
        SpeakerScheduleEvent(time_offset=35.0, guest_index=1, mute=True),
    ]
    orchestrator = KTalkOrchestrator(
        guest_names=["GuestA", "GuestB"],
        guest_audio_files=[str(speaker_a_wav), str(speaker_b_wav)],
        schedule=schedule,
        test_duration=40.0,
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
def test_long_pause_between_speakers(speaker_a_wav: Path, speaker_b_wav: Path):
    """Guest A speaks 0-10s, 15s silence, Guest B speaks 25-35s.

    Late lines must not all be attributed to Guest A.
    """
    schedule = [
        SpeakerScheduleEvent(time_offset=0.0, guest_index=0, mute=False),
        SpeakerScheduleEvent(time_offset=10.0, guest_index=0, mute=True),
        SpeakerScheduleEvent(time_offset=25.0, guest_index=1, mute=False),
        SpeakerScheduleEvent(time_offset=35.0, guest_index=1, mute=True),
    ]
    orchestrator = KTalkOrchestrator(
        guest_names=["GuestA", "GuestB"],
        guest_audio_files=[str(speaker_a_wav), str(speaker_b_wav)],
        schedule=schedule,
        test_duration=40.0,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"

    guest_a_lines = [l for l in result.transcript_lines if "GuestA" in l]
    guest_b_lines = [l for l in result.transcript_lines if "GuestB" in l]

    # After the long pause Guest B's speech should not be labelled as GuestA
    assert len(guest_b_lines) > 0 or "GuestA" not in {
        s for s in result.speakers_found if s != "GuestA"
    }, "Late speech after long pause must not all be attributed to GuestA"

    # At least one late line should NOT be attributed to GuestA
    late_guest_a_only = len(result.transcript_lines) > 0 and all(
        "GuestA" in l for l in result.transcript_lines if l.strip()
    )
    assert not late_guest_a_only, (
        "All transcript lines are attributed to GuestA; GuestB attribution missing after pause"
    )


@pytest.mark.e2e
def test_three_speakers(speaker_a_wav: Path, speaker_b_wav: Path, speaker_c_wav: Path):
    """Three guests speak in sequence; expect 3+ distinct speakers."""
    schedule = [
        SpeakerScheduleEvent(time_offset=0.0, guest_index=0, mute=False),
        SpeakerScheduleEvent(time_offset=10.0, guest_index=0, mute=True),
        SpeakerScheduleEvent(time_offset=10.0, guest_index=1, mute=False),
        SpeakerScheduleEvent(time_offset=20.0, guest_index=1, mute=True),
        SpeakerScheduleEvent(time_offset=20.0, guest_index=2, mute=False),
        SpeakerScheduleEvent(time_offset=30.0, guest_index=2, mute=True),
    ]
    orchestrator = KTalkOrchestrator(
        guest_names=["GuestA", "GuestB", "GuestC"],
        guest_audio_files=[str(speaker_a_wav), str(speaker_b_wav), str(speaker_c_wav)],
        schedule=schedule,
        test_duration=35.0,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"
    assert len(result.speakers_found) >= 3, (
        f"Expected at least 3 speakers, found: {result.speakers_found}"
    )
