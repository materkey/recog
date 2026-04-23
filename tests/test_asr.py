"""Unit tests for recog/asr.py.

Covers the batched forward pass `ASRModule.transcribe_batch` used by the
per-speaker PCM pipeline.  The gigaam model itself is heavy (~1 GB) and not
suitable for unit tests, so the model is stubbed out: we only verify that
the batch is padded, stacked, and forwarded correctly, and that decoded
texts preserve per-sample order.
"""
from __future__ import annotations

import sys
import threading
import time
from pathlib import Path
from unittest.mock import MagicMock

import numpy as np
import pytest
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.asr import ASRModule


def _make_mocked_asr(decoded_texts: list[str]) -> tuple[ASRModule, MagicMock]:
    """Create ASRModule with `model` replaced by a MagicMock.

    The mock's `forward` returns a sentinel (encoded, encoded_len) pair; its
    `decoding.decode` returns `decoded_texts` verbatim.  `load_model` is
    overridden to skip real gigaam import.
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
    model.decoding.decode = MagicMock(return_value=decoded_texts)

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
