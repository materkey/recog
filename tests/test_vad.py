"""Unit tests for recog.vad.pcm_int16_has_speech."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from recog import vad
from recog.config import PCM_VAD_THRESHOLD


@pytest.fixture(autouse=True)
def reset_model_singleton():
    vad.reset()
    yield
    vad.reset()


def test_silence_is_not_speech():
    pcm = np.zeros(9600, dtype=np.int16)
    assert vad.pcm_int16_has_speech(pcm, PCM_VAD_THRESHOLD) is False


def test_empty_chunk_is_not_speech():
    pcm = np.array([], dtype=np.int16)
    assert vad.pcm_int16_has_speech(pcm, PCM_VAD_THRESHOLD) is False


def test_chunk_below_frame_size_is_not_speech():
    # 511 samples — short of the 512-sample Silero frame.
    pcm = np.zeros(511, dtype=np.int16)
    assert vad.pcm_int16_has_speech(pcm, PCM_VAD_THRESHOLD) is False


def test_real_speech_fixture_is_speech():
    import soundfile as sf
    fixture = Path(__file__).parent / "e2e" / "fixtures" / "speech_reference.wav"
    if not fixture.exists():
        pytest.skip("speech_reference.wav not available")
    data, sr = sf.read(fixture, dtype="int16")
    if sr != 16000 or data.ndim != 1:
        pytest.skip("fixture is not 16 kHz mono int16")
    assert vad.pcm_int16_has_speech(data[:16000], PCM_VAD_THRESHOLD) is True


def test_pure_noise_below_threshold_is_not_speech():
    rng = np.random.default_rng(42)
    pcm_f32 = rng.normal(0, 0.05, 16000).astype(np.float32)
    pcm_i16 = (pcm_f32 * 32767).astype(np.int16)
    assert vad.pcm_int16_has_speech(pcm_i16, PCM_VAD_THRESHOLD) is False


def test_fail_open_when_model_unavailable(monkeypatch):
    monkeypatch.setattr(vad, "_get_model", lambda: None)
    pcm = np.zeros(9600, dtype=np.int16)
    assert vad.pcm_int16_has_speech(pcm, PCM_VAD_THRESHOLD) is True
