#!/usr/bin/env python3
"""Prepare audio fixtures for e2e diarization tests.

Extracts segments from example_5min.wav and creates per-speaker WAV files.
Chromium --use-file-for-fake-audio-capture loops the file continuously.

Usage:
    python tests/e2e/prepare_fixtures.py
"""
import numpy as np
import soundfile as sf
from pathlib import Path

FIXTURES_DIR = Path(__file__).parent / "fixtures"
SOURCE = Path(__file__).parent.parent.parent / "example_5min.wav"

SEGMENTS = [
    (5.0,  15.0, "speaker_a.wav"),
    (60.0, 15.0, "speaker_b.wav"),
    (120.0, 15.0, "speaker_c.wav"),
]

SAMPLE_RATE = 16000


def main():
    FIXTURES_DIR.mkdir(parents=True, exist_ok=True)

    if not SOURCE.exists():
        print(f"ERROR: {SOURCE} not found. Place example_5min.wav in project root.")
        return

    data, sr = sf.read(str(SOURCE))
    print(f"Source: {SOURCE} ({len(data)/sr:.1f}s, {sr}Hz)")

    for start, duration, name in SEGMENTS:
        start_sample = int(start * sr)
        end_sample = int((start + duration) * sr)
        segment = data[start_sample:end_sample]

        peak = np.max(np.abs(segment))
        if peak > 0:
            segment = segment * (0.8 / peak)

        out_path = FIXTURES_DIR / name
        sf.write(str(out_path), segment, sr)
        print(f"  {name}: {duration:.0f}s ({start:.0f}-{start+duration:.0f}s)")

    silence = np.zeros(int(5.0 * sr), dtype=np.float32)
    sf.write(str(FIXTURES_DIR / "silence.wav"), silence, sr)
    print(f"  silence.wav: 5.0s")

    print(f"\nFixtures saved to {FIXTURES_DIR}")


if __name__ == "__main__":
    main()
