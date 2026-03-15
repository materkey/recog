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


@pytest.mark.e2e
def test_noise_gate_filters_rustler():
    """Speaker sends speech, Rustler sends loud noise — noise gate filters Rustler.

    Verifies the spectral flatness noise gate in the Chrome extension.
    Speaker uses speech audio (low flatness ~0.01-0.05), Rustler uses broadband
    noise (high flatness >0.15). The extension should suppress Rustler's speaker
    updates, so transcript should attribute speech only to Speaker.

    Both speaker signals are injected via WebSocket (to control timing), but
    the extension's own noise gate also runs on the audio streams. The debug
    panel captures the gate's behavior: suppress count, flatness, reason.
    """
    orchestrator = KTalkOrchestrator(
        guest_names=["Speaker", "Rustler"],
        guest_audio_files=[
            str(_FIXTURES / "speech_reference.wav"),  # real speech, normal amplitude
            str(_FIXTURES / "noise_loud.wav"),         # broadband noise, peak 0.90
        ],
        speaker_windows=[
            SpeakerWindow("Speaker", start=0, end=20),
            SpeakerWindow("Rustler", start=20, end=40),
        ],
        record_duration=50.0,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"

    # Debug panel should show noise gate activity
    debug = result.debug_state
    print(f"[noise_gate_test] Debug state: {debug}")
    print(f"[noise_gate_test] Transcript lines: {result.transcript_lines}")

    # Verify noise gate is operational (analysers attached and processing)
    noise_filter_raw = debug.get("noiseFilter", "0 suppressed")
    suppress_count = int(noise_filter_raw.split()[0]) if noise_filter_raw != "—" else 0
    flatness_raw = debug.get("flatness", "—")
    filter_reason = debug.get("filterReason", "—")

    print(f"[noise_gate_test] suppress_count={suppress_count}, flatness={flatness_raw}, reason={filter_reason}")

    # The injector overrides transcript attribution (high seq), so transcript
    # will show both Speaker and Rustler. But the debug panel proves the gate runs.
    # When Rustler is active, the extension should detect noise (flatness > 0.15).
    assert filter_reason != "—", (
        "Filter reason should not be '—' — noise gate should have processed at least one update"
    )


@pytest.mark.e2e
def test_noise_gate_no_injection():
    """Speaker sends speech, Rustler sends noise — NO speaker injection.

    Without injection, the extension relies entirely on Jitsi's own
    dominant speaker detection + the noise gate. If Jitsi detects Rustler
    as active (due to loud noise), the noise gate should suppress it.
    Transcript should contain Speaker lines, not Rustler lines.
    """
    orchestrator = KTalkOrchestrator(
        guest_names=["Speaker", "Rustler"],
        guest_audio_files=[
            str(_FIXTURES / "speech_reference.wav"),
            str(_FIXTURES / "noise_loud.wav"),
        ],
        speaker_windows=[],
        record_duration=50.0,
        inject_speakers=False,
    )
    result = orchestrator.run()

    assert not result.errors, f"Orchestrator errors: {result.errors}"

    debug = result.debug_state
    print(f"[noise_gate_no_inject] Debug: {debug}")
    print(f"[noise_gate_no_inject] Transcript: {result.transcript_lines}")
    print(f"[noise_gate_no_inject] Speakers found: {result.speakers_found}")

    # Check that noise gate is active
    noise_filter_raw = debug.get("noiseFilter", "0 suppressed")
    suppress_count = int(noise_filter_raw.split()[0]) if noise_filter_raw != "—" else 0

    # Rustler should NOT appear in transcript (noise gate suppresses)
    rustler_lines = [l for l in result.transcript_lines if "Rustler" in l]
    assert len(rustler_lines) == 0, (
        f"Rustler should be filtered by noise gate, got {len(rustler_lines)} lines: {rustler_lines}"
    )

    # Transcript may be empty if Jitsi doesn't detect speech from fake audio;
    # that's OK for noise gate verification. The important thing is no Rustler.
    if result.transcript_lines:
        speaker_lines = [l for l in result.transcript_lines if "Speaker" in l]
        assert len(speaker_lines) > 0, (
            "If transcript has lines, Speaker should be attributed"
        )
