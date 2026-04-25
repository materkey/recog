"""Unit tests for recog.vad.pcm_int16_has_speech.

The Silero ONNX model is loaded on first call; tests use synthetic PCM
so they do not depend on a fixtures wav.  Latency budget per call at
600 ms PCM is ~2-5 ms on Apple Silicon — fast enough for a pytest.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import numpy as np
import pytest

from recog import vad


@pytest.fixture(autouse=True)
def reset_model_singleton():
    vad.reset()
    yield
    vad.reset()


def test_silence_is_not_speech():
    """Pure zero PCM → get_speech_timestamps yields no span → False."""
    pcm = np.zeros(9600, dtype=np.int16)
    assert vad.pcm_int16_has_speech(pcm, threshold=0.3) is False


def test_empty_chunk_is_not_speech():
    pcm = np.array([], dtype=np.int16)
    assert vad.pcm_int16_has_speech(pcm, threshold=0.3) is False


def test_real_speech_fixture_is_speech():
    """Real recorded speech from e2e fixtures → at least one Silero span."""
    import soundfile as sf
    fixture = Path(__file__).parent / "e2e" / "fixtures" / "speech_reference.wav"
    if not fixture.exists():
        pytest.skip("speech_reference.wav not available")
    data, sr = sf.read(fixture, dtype="int16")
    if sr != 16000 or data.ndim != 1:
        pytest.skip("fixture is not 16 kHz mono int16")
    pcm = data[: int(sr * 1.0)]  # first 1 second is enough
    assert vad.pcm_int16_has_speech(pcm, threshold=0.3) is True


def test_pure_noise_below_threshold_is_not_speech():
    """White noise amplitude 0.05 (quiet hiss) — Silero should reject."""
    rng = np.random.default_rng(42)
    pcm_f32 = rng.normal(0, 0.05, 16000).astype(np.float32)
    pcm_i16 = (pcm_f32 * 32767).astype(np.int16)
    assert vad.pcm_int16_has_speech(pcm_i16, threshold=0.3) is False


def test_fail_open_when_model_unavailable(monkeypatch):
    """If silero-vad is not importable, VAD returns True (keep chunk)."""
    monkeypatch.setattr(vad, "_get_model", lambda: None)
    pcm = np.zeros(9600, dtype=np.int16)
    assert vad.pcm_int16_has_speech(pcm, threshold=0.3) is True


