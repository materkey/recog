#!/usr/bin/env python3
"""Prepare audio fixtures for e2e diarization tests.

Extracts segments from example_5min.wav and creates per-speaker WAV files.
Chromium --use-file-for-fake-audio-capture loops the file continuously.

Since KTalk mute/unmute does NOT stop fake audio streams, we bake the timing
directly into the WAV files: each file contains silence where the guest should
be "quiet" and speech where they should be "talking".

Usage:
    python tests/e2e/prepare_fixtures.py
"""
import numpy as np
import soundfile as sf
from pathlib import Path

FIXTURES_DIR = Path(__file__).parent / "fixtures"
SOURCE = Path(__file__).parent.parent.parent / "example_5min.wav"

# Raw speech segments extracted from example_5min.wav
SEGMENTS = [
    (5.0,  20.0, "speaker_a.wav"),
    (60.0, 20.0, "speaker_b.wav"),
    (120.0, 20.0, "speaker_c.wav"),
]

# Total duration for timed files — Chromium loops the file, so this sets
# the loop period.  Must be >= longest schedule offset + some margin.
TIMED_DURATION = 60.0


def _normalize(segment: np.ndarray, target_peak: float = 0.8) -> np.ndarray:
    peak = np.max(np.abs(segment))
    if peak > 0:
        return segment * (target_peak / peak)
    return segment


def main():
    FIXTURES_DIR.mkdir(parents=True, exist_ok=True)

    if not SOURCE.exists():
        print(f"ERROR: {SOURCE} not found. Place example_5min.wav in project root.")
        return

    data, sr = sf.read(str(SOURCE))
    print(f"Source: {SOURCE} ({len(data)/sr:.1f}s, {sr}Hz)")

    # --- Raw speech segments (for manual/other use) ---
    raw_segments = {}
    for start, duration, name in SEGMENTS:
        start_sample = int(start * sr)
        end_sample = int((start + duration) * sr)
        segment = _normalize(data[start_sample:end_sample])

        out_path = FIXTURES_DIR / name
        sf.write(str(out_path), segment, sr)
        raw_segments[name] = segment
        print(f"  {name}: {duration:.0f}s ({start:.0f}-{start+duration:.0f}s)")

    # --- Silence ---
    silence = np.zeros(int(5.0 * sr), dtype=np.float32)
    sf.write(str(FIXTURES_DIR / "silence.wav"), silence, sr)
    print(f"  silence.wav: 5.0s")

    # --- Timed audio files (speech at scheduled offsets, silence elsewhere) ---
    # These are used with --use-file-for-fake-audio-capture because KTalk mute
    # does NOT stop the fake audio stream.
    total_samples = int(TIMED_DURATION * sr)

    def _make_timed(speech: np.ndarray, speak_start: float, name: str):
        """Create a WAV file with silence + speech at the given offset."""
        out = np.zeros(total_samples, dtype=np.float32)
        s = int(speak_start * sr)
        e = min(s + len(speech), total_samples)
        out[s:e] = speech[:e - s]
        sf.write(str(FIXTURES_DIR / name), out, sr)
        duration = len(speech) / sr
        print(f"  {name}: {TIMED_DURATION:.0f}s total, speech at {speak_start:.0f}-{speak_start + duration:.0f}s")

    print("\nTimed fixtures (speech + silence):")

    # test_two_speakers_alternating: A speaks 0-20s, B speaks 20-40s
    _make_timed(raw_segments["speaker_a.wav"], 0.0, "timed_a_first.wav")
    _make_timed(raw_segments["speaker_b.wav"], 20.0, "timed_b_second.wav")

    # test_ttl / test_long_pause: A speaks 0-15s, gap, B speaks 30-50s
    _make_timed(raw_segments["speaker_a.wav"][:int(15 * sr)], 0.0, "timed_a_early.wav")
    _make_timed(raw_segments["speaker_b.wav"][:int(20 * sr)], 30.0, "timed_b_late.wav")

    # test_three_speakers: A 0-15s, B 15-30s, C 30-45s
    _make_timed(raw_segments["speaker_a.wav"][:int(15 * sr)], 0.0, "timed_a_seq.wav")
    _make_timed(raw_segments["speaker_b.wav"][:int(15 * sr)], 15.0, "timed_b_seq.wav")
    _make_timed(raw_segments["speaker_c.wav"][:int(15 * sr)], 30.0, "timed_c_seq.wav")

    # test_per_speaker_overlap (Task 5.1): Alice speaks 0-20s, Bob 5-25s
    # (15 s overlap). Different source segments guarantee non-overlapping
    # word sets so the test can detect cross-contamination.
    _make_timed(raw_segments["speaker_a.wav"], 0.0, "timed_alice_overlap.wav")
    _make_timed(raw_segments["speaker_b.wav"], 5.0, "timed_bob_overlap.wav")

    # Alt speech reference distinct from speech_reference.wav (derived from
    # speaker_a). Used by overlap tests as Bob's "what-should-not-leak"
    # reference transcript source. Generated from speaker_b so the voice is
    # different from speech_reference; if vlad_sample.wav is available in the
    # project root, prefer that (longer and unambiguously a different speaker).
    vlad_src = Path(__file__).parent.parent.parent / "vlad_sample.wav"
    if vlad_src.exists():
        vlad_data, vlad_sr = sf.read(str(vlad_src))
        clip = vlad_data[:int(20 * vlad_sr)]
        clip = _normalize(clip)
        sf.write(str(FIXTURES_DIR / "speech_reference_alt.wav"), clip, vlad_sr)
        print(f"  speech_reference_alt.wav: 20s (from vlad_sample.wav)")
    else:
        sf.write(str(FIXTURES_DIR / "speech_reference_alt.wav"),
                 raw_segments["speaker_b.wav"], sr)
        print(f"  speech_reference_alt.wav: 20s (fallback from speaker_b.wav — "
              f"vlad_sample.wav not found)")

    print(f"\nAll fixtures saved to {FIXTURES_DIR}")


if __name__ == "__main__":
    main()
