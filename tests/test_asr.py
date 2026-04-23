"""Unit tests for recog/asr.py.

Covers the batched forward pass `ASRModule.transcribe_batch` used by the
per-speaker PCM pipeline, and the word-timestamp wrapper
`ASRModule.transcribe_with_timestamps` used by the refine contour.  The
gigaam model itself is heavy (~1 GB) and not suitable for unit tests, so
the model is stubbed out: we only verify that the wrapper forwards the
right args, cleans up temp files, and normalizes word offsets to
milliseconds.
"""
from __future__ import annotations

import os
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional
from unittest.mock import MagicMock

import numpy as np
import pytest
import soundfile as sf
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.asr import ASRModule


@dataclass
class _FakeWord:
    """Stand-in for gigaam.types.Word — plain attrs, mutable start/end."""
    text: str
    start: float
    end: float


@dataclass
class _FakeResult:
    """Stand-in for gigaam.types.TranscriptionResult."""
    text: str
    words: Optional[list] = None


def _make_mocked_asr(decoded_texts: list[str]) -> tuple[ASRModule, MagicMock]:
    """Create ASRModule with `model` replaced by a MagicMock.

    The mock's `forward` returns a sentinel (encoded, encoded_len) pair; its
    `decoding.decode` returns one `(text, token_ids, token_frames)` tuple per
    input text — matching upstream GigaAM's post-`a755fec` decode signature.
    `load_model` is overridden to skip the real gigaam import.
    """
    asr = ASRModule.__new__(ASRModule)  # bypass __init__ (which imports gigaam)
    asr.SAMPLE_RATE = 16000
    asr.model_name = "stub"
    asr.device = None
    asr._model_lock = threading.Lock()

    model = MagicMock()
    model._device = torch.device("cpu")
    model._dtype = torch.float32
    model.forward = MagicMock(return_value=("ENCODED", "ENCODED_LEN"))
    model.head = "HEAD"
    model.decoding = MagicMock()
    decoded_tuples = [(text, [], []) for text in decoded_texts]
    model.decoding.decode = MagicMock(return_value=decoded_tuples)

    asr.model = model
    asr.load_model = lambda quiet=False: model  # type: ignore[assignment]

    return asr, model


def test_transcribe_batch_returns_per_sample_texts():
    """
    given: 2 PCM arrays of different lengths
    when: transcribe_batch is called
    then: forward receives a padded batch tensor and decode output order
          is preserved per-sample
    """
    asr, model = _make_mocked_asr(decoded_texts=["hello", "world"])

    pcm1 = np.full(16000, 100, dtype=np.int16)  # 1 s of int16 audio
    pcm2 = np.full(24000, 200, dtype=np.int16)  # 1.5 s of int16 audio

    texts = asr.transcribe_batch([pcm1, pcm2])

    assert texts == ["hello", "world"]

    # forward was called once with a (2, max_len) batch tensor.
    model.forward.assert_called_once()
    wav_batch, length_batch = model.forward.call_args[0]
    assert wav_batch.shape == (2, 24000)  # padded to max length
    assert wav_batch.dtype == torch.float32
    assert length_batch.tolist() == [16000, 24000]

    # First sample is zero-padded beyond its length.
    assert torch.all(wav_batch[0, 16000:] == 0).item()

    # Int16 → float normalization: 100 / 32768 ≈ 0.00305…
    expected_value = 100 / 32768.0
    assert wav_batch[0, 0].item() == pytest.approx(expected_value)

    # decode was called with forward output.
    model.decoding.decode.assert_called_once_with("HEAD", "ENCODED", "ENCODED_LEN")


def test_transcribe_batch_empty_list_returns_empty():
    """
    given: empty input list
    when: transcribe_batch is called
    then: returns [] without invoking model.forward
    """
    asr, model = _make_mocked_asr(decoded_texts=[])
    texts = asr.transcribe_batch([])
    assert texts == []
    model.forward.assert_not_called()


def test_transcribe_batch_accepts_float32_inputs():
    """
    given: float32 input PCM already in [-1, 1]
    when: transcribe_batch is called
    then: values pass through without re-normalization
    """
    asr, model = _make_mocked_asr(decoded_texts=["t1"])
    pcm = np.full(8000, 0.5, dtype=np.float32)
    asr.transcribe_batch([pcm])

    wav_batch, _ = model.forward.call_args[0]
    assert wav_batch.shape == (1, 8000)
    # 0.5 stays 0.5 (no /32768 applied).
    assert wav_batch[0, 0].item() == pytest.approx(0.5)


def test_transcribe_batch_preserves_input_order_across_varying_lengths():
    """
    given: 3 samples with lengths (8000, 24000, 16000)
    when: transcribe_batch is called
    then: returned list matches the decoder's output positions 1:1 with input
    """
    asr, model = _make_mocked_asr(decoded_texts=["a", "b", "c"])

    samples = [
        np.zeros(8000, dtype=np.int16),
        np.zeros(24000, dtype=np.int16),
        np.zeros(16000, dtype=np.int16),
    ]
    texts = asr.transcribe_batch(samples)
    assert texts == ["a", "b", "c"]

    _, length_batch = model.forward.call_args[0]
    assert length_batch.tolist() == [8000, 24000, 16000]


def test_load_model_is_single_flight_across_threads():
    """
    given: several threads race to lazily load ASR
    when: load_model is called concurrently
    then: GigaAM is loaded once and all callers receive the same model object
    """
    asr = ASRModule.__new__(ASRModule)  # bypass __init__ (which imports gigaam)
    asr.model_name = "stub"
    asr.device = None
    asr.model = None
    asr._model_lock = threading.Lock()
    asr.gigaam = MagicMock()

    loaded_model = object()

    def slow_load_model(_model_name: str, device=None):
        time.sleep(0.02)
        return loaded_model

    asr.gigaam.load_model = MagicMock(side_effect=slow_load_model)

    thread_count = 8
    barrier = threading.Barrier(thread_count)
    results = []
    errors = []

    def load_from_thread():
        try:
            barrier.wait(timeout=1)
            results.append(asr.load_model(quiet=True))
        except Exception as e:
            errors.append(e)

    threads = [threading.Thread(target=load_from_thread) for _ in range(thread_count)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=1)

    assert errors == []
    assert results == [loaded_model] * thread_count
    asr.gigaam.load_model.assert_called_once_with("stub", device=None)


def _make_timestamps_asr(result: _FakeResult) -> tuple[ASRModule, MagicMock]:
    """Create ASRModule where `model.transcribe(...)` returns `result`."""
    asr = ASRModule.__new__(ASRModule)
    asr.SAMPLE_RATE = 16000
    asr.model_name = "stub"
    asr.device = None
    asr._model_lock = threading.Lock()

    model = MagicMock()
    model.transcribe = MagicMock(return_value=result)
    asr.model = model
    asr.load_model = lambda quiet=False: model  # type: ignore[assignment]

    return asr, model


def test_transcribe_with_timestamps_silence_ndarray_returns_str_and_list():
    """
    given: 1 second of silence (int16 zeros) as np.ndarray
    when: transcribe_with_timestamps is called
    then: result.text is a str and result.words is a list (possibly empty),
          and the temp WAV passed to model.transcribe is cleaned up afterwards
    """
    asr, model = _make_timestamps_asr(_FakeResult(text="", words=[]))

    silence = np.zeros(16000, dtype=np.int16)
    result = asr.transcribe_with_timestamps(silence)

    assert isinstance(result.text, str)
    assert isinstance(result.words, list)

    # model.transcribe called exactly once, word_timestamps=True, with a
    # filesystem path that has since been unlinked (tempfile cleanup).
    model.transcribe.assert_called_once()
    call_args, call_kwargs = model.transcribe.call_args
    tmp_path = call_args[0]
    assert isinstance(tmp_path, str) and tmp_path.endswith(".wav")
    assert call_kwargs == {"word_timestamps": True}
    assert not os.path.exists(tmp_path), "temp WAV must be removed after use"


def test_transcribe_with_timestamps_normalizes_word_offsets_to_ms():
    """
    given: upstream returns Words with float-second start/end
    when: transcribe_with_timestamps is called
    then: each word's start/end is mutated to integer milliseconds
    """
    words = [
        _FakeWord(text="раз", start=0.8, end=1.12),
        _FakeWord(text="два", start=1.164, end=1.36),  # 1.164 → 1164 ms
    ]
    asr, _ = _make_timestamps_asr(_FakeResult(text="раз два", words=words))

    result = asr.transcribe_with_timestamps(np.zeros(8000, dtype=np.float32))

    assert result.words[0].start == 800
    assert result.words[0].end == 1120
    assert result.words[1].start == 1164
    assert result.words[1].end == 1360
    for w in result.words:
        assert isinstance(w.start, int)
        assert isinstance(w.end, int)


def test_transcribe_with_timestamps_empty_words_does_not_raise():
    """
    given: upstream returns words=None (silent clip)
    when: transcribe_with_timestamps is called
    then: result is returned unchanged — no AttributeError while normalizing
    """
    asr, _ = _make_timestamps_asr(_FakeResult(text="", words=None))
    result = asr.transcribe_with_timestamps(np.zeros(1600, dtype=np.float32))
    assert result.words is None
    assert result.text == ""


def test_transcribe_with_timestamps_str_path_passthrough(tmp_path):
    """
    given: an existing WAV file on disk
    when: transcribe_with_timestamps is called with the path (str)
    then: model.transcribe receives that exact path and the file is NOT
          deleted afterwards (it wasn't ours to clean up)
    """
    wav_path = tmp_path / "input.wav"
    sf.write(str(wav_path), np.zeros(16000, dtype=np.float32), 16000, subtype="FLOAT")

    asr, model = _make_timestamps_asr(_FakeResult(text="ok", words=[]))
    asr.transcribe_with_timestamps(str(wav_path))

    model.transcribe.assert_called_once_with(str(wav_path), word_timestamps=True)
    assert wav_path.exists(), "caller-owned file must not be deleted"


def test_transcribe_with_timestamps_rejects_multichannel():
    """
    given: a 2-D (stereo) audio array
    when: transcribe_with_timestamps is called
    then: ValueError is raised with a shape-specific message
    """
    asr, _ = _make_timestamps_asr(_FakeResult(text="", words=[]))
    stereo = np.zeros((2, 8000), dtype=np.float32)  # 2 channels, cannot squeeze
    with pytest.raises(ValueError, match="1-D mono"):
        asr.transcribe_with_timestamps(stereo)


def test_transcribe_with_timestamps_accepts_torch_tensor():
    """
    given: a 1-D torch.Tensor of float32 audio
    when: transcribe_with_timestamps is called
    then: tensor is converted to numpy, written to tmp WAV, and forwarded
    """
    asr, model = _make_timestamps_asr(_FakeResult(text="", words=[]))
    audio = torch.zeros(16000, dtype=torch.float32)
    asr.transcribe_with_timestamps(audio)

    model.transcribe.assert_called_once()
    tmp_path = model.transcribe.call_args[0][0]
    assert tmp_path.endswith(".wav")
    assert not os.path.exists(tmp_path)


@pytest.mark.skipif(
    not torch.backends.mps.is_available(),
    reason="MPS backend not available on this machine",
)
def test_asrmodule_autodetects_mps_device_when_device_none():
    """
    given: a host with torch.backends.mps.is_available() == True
    when: ASRModule is instantiated with the default device=None
    then: self.device is auto-resolved to "mps"
    """
    asr = ASRModule(device=None)
    assert asr.device == "mps"


def test_asrmodule_explicit_cpu_overrides_mps_autodetect():
    """
    given: an explicit device="cpu" kwarg
    when: ASRModule is instantiated
    then: self.device stays "cpu" regardless of MPS availability on the host
    """
    asr = ASRModule(device="cpu")
    assert asr.device == "cpu"


def test_asrmodule_falls_back_to_cpu_when_mps_unavailable(monkeypatch):
    """
    given: torch.backends.mps.is_available() returns False
    when: ASRModule is instantiated with device=None
    then: self.device falls back to "cpu"
    """
    monkeypatch.setattr(torch.backends.mps, "is_available", lambda: False)
    asr = ASRModule(device=None)
    assert asr.device == "cpu"
