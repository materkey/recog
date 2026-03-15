#!/usr/bin/env python3
"""Generate noise WAV fixtures for speaker-noise detection research.

Creates WAV files that simulate non-speech sounds (rustling, keyboard clicks)
alongside real speech extracted from existing fixtures. All files are 20s at
the same sample rate for consistent comparison.

Usage:
    python tests/e2e/generate_noise_fixtures.py
"""
import numpy as np
import soundfile as sf
from pathlib import Path

FIXTURES_DIR = Path(__file__).parent / "fixtures"
SR = 48000  # Match KTalk/WebRTC sample rate
DURATION = 30.0  # seconds


def pink_noise(n_samples: int) -> np.ndarray:
    """Generate pink noise (1/f spectrum) using Voss-McCartney algorithm."""
    n_rows = 16
    array = np.empty((n_samples, n_rows))
    array.fill(np.nan)
    array[0, :] = np.random.random(n_rows)
    array[:, 0] = np.random.random(n_samples)

    cols = np.random.geometric(0.5, n_samples)
    cols[cols >= n_rows] = 0
    rows = np.random.randint(n_samples, size=n_samples)
    array[rows, cols] = np.random.random(n_samples)

    # Forward-fill NaN values
    for i in range(1, n_samples):
        mask = np.isnan(array[i])
        array[i, mask] = array[i - 1, mask]

    total = np.nansum(array, axis=1)
    total -= np.mean(total)
    peak = np.max(np.abs(total))
    if peak > 0:
        total = total / peak * 0.6  # normalize to 0.6 peak
    return total.astype(np.float32)


def keyboard_clicks(n_samples: int, sr: int) -> np.ndarray:
    """Generate simulated keyboard click sounds — short broadband transients."""
    out = np.zeros(n_samples, dtype=np.float32)
    # Random click intervals: ~3-8 clicks per second
    click_interval_samples = sr // 5  # ~5 clicks/sec average
    pos = 0
    while pos < n_samples:
        # Each click: very short burst of broadband noise (2-5ms)
        click_len = int(sr * np.random.uniform(0.002, 0.005))
        click = np.random.randn(click_len).astype(np.float32) * 0.7
        # Apply sharp envelope
        envelope = np.exp(-np.linspace(0, 8, click_len))
        click *= envelope
        end = min(pos + click_len, n_samples)
        out[pos:end] = click[:end - pos]
        # Random gap between clicks
        gap = int(np.random.uniform(0.08, 0.35) * sr)
        pos += click_len + gap
    return out


def rustling(n_samples: int, sr: int) -> np.ndarray:
    """Generate paper rustling — filtered broadband noise with amplitude modulation."""
    # Broadband noise base
    noise = np.random.randn(n_samples).astype(np.float32)
    # Amplitude modulation (rustling comes in bursts)
    mod_freq = np.random.uniform(2, 6)  # Hz
    t = np.linspace(0, n_samples / sr, n_samples)
    modulation = 0.5 + 0.5 * np.sin(2 * np.pi * mod_freq * t + np.random.uniform(0, 2 * np.pi))
    # Add random amplitude variations
    for _ in range(5):
        f = np.random.uniform(0.5, 3)
        modulation += 0.3 * np.sin(2 * np.pi * f * t + np.random.uniform(0, 2 * np.pi))
    modulation = np.clip(modulation / np.max(modulation), 0, 1)
    out = noise * modulation * 0.5
    return out.astype(np.float32)


def main():
    FIXTURES_DIR.mkdir(parents=True, exist_ok=True)
    n_samples = int(DURATION * SR)

    # --- Pink noise (general broadband noise, simulates rustling/fan) ---
    pn = pink_noise(n_samples)
    sf.write(str(FIXTURES_DIR / "noise_pink.wav"), pn, SR)
    print(f"  noise_pink.wav: {DURATION:.0f}s pink noise")

    # --- Keyboard clicks ---
    clicks = keyboard_clicks(n_samples, SR)
    sf.write(str(FIXTURES_DIR / "noise_clicks.wav"), clicks, SR)
    print(f"  noise_clicks.wav: {DURATION:.0f}s keyboard clicks")

    # --- Paper rustling ---
    rust = rustling(n_samples, SR)
    sf.write(str(FIXTURES_DIR / "noise_rustling.wav"), rust, SR)
    print(f"  noise_rustling.wav: {DURATION:.0f}s paper rustling")

    # --- Speech reference (copy from existing fixture, padded to DURATION) ---
    speech_src = FIXTURES_DIR / "speaker_a.wav"
    if speech_src.exists():
        data, sr = sf.read(str(speech_src))
        padded = np.zeros(int(DURATION * sr), dtype=np.float32)
        copy_len = min(len(data), len(padded))
        padded[:copy_len] = data[:copy_len]
        sf.write(str(FIXTURES_DIR / "speech_reference.wav"), padded, sr)
        print(f"  speech_reference.wav: {DURATION:.0f}s (speech from speaker_a)")
    else:
        print(f"  WARNING: {speech_src} not found, skipping speech_reference.wav")

    print(f"\nNoise fixtures saved to {FIXTURES_DIR}")


if __name__ == "__main__":
    main()
