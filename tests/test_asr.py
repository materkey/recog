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
    asr._model_lock = threading.RLock()
    asr._mps_inference_count = 0

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


def test_transcribe_simple_unpacks_decode_tuple(monkeypatch, tmp_path):
    """
    given: model.decoding.decode returns List[Tuple[str, List[int], List[int]]]
          (post-GigaAM commit a755fec tuple shape)
    when: transcribe_simple runs one chunk
    then: the first-tuple-element text is extracted into Segment.text

    Regression guard for recog/asr.py:134 — a revert to single-index `[0]`
    would assign a tuple to `text` and crash on `.strip()`.
    """
    import types

    asr, model = _make_mocked_asr(decoded_texts=["hello world"])

    # Stub gigaam.preprocess to avoid the heavy upstream import chain; the
    # audio returned here just needs to satisfy transcribe_simple's shape
    # expectations (1-D at SAMPLE_RATE, >= 0.5 s).
    fake_preprocess = types.ModuleType("gigaam.preprocess")
    fake_preprocess.load_audio = lambda _path: torch.zeros(16000)
    fake_preprocess.SAMPLE_RATE = 16000
    monkeypatch.setitem(sys.modules, "gigaam.preprocess", fake_preprocess)

    segments = asr.transcribe_simple(
        str(tmp_path / "dummy.wav"),
        chunk_duration=1.0,
        overlap=0.0,
        quiet=True,
    )

    assert len(segments) == 1
    assert segments[0].text == "hello world"
    model.decoding.decode.assert_called_once()


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
    asr._model_lock = threading.RLock()
    asr._mps_inference_count = 0

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


def test_asrmodule_explicit_cpu_overrides_mps_autodetect(monkeypatch):
    """
    given: an explicit device="cpu" kwarg and MPS is available
    when: ASRModule is instantiated
    then: self.device stays "cpu" — auto-detect path is bypassed

    Without the monkeypatch, this test would be tautological on CPU-only
    hosts: both the explicit and auto-detect branches yield "cpu".
    """
    monkeypatch.setattr(torch.backends.mps, "is_available", lambda: True)
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


# -----------------------------------------------------------------------------
# _mps_autoreleasepool helper + _maybe_empty_mps_cache throttle
# -----------------------------------------------------------------------------


def _make_pool_mock() -> MagicMock:
    """Build a MagicMock that behaves like a context manager and tracks
    enter/exit. Used to verify `with objc.autorelease_pool(): ...` semantics."""
    cm = MagicMock()
    cm.__enter__ = MagicMock(return_value=None)
    cm.__exit__ = MagicMock(return_value=False)  # don't suppress exceptions
    pool_factory = MagicMock(return_value=cm)
    return pool_factory


def _install_pool_mock(monkeypatch) -> MagicMock:
    """Install a MagicMock factory for `recog.asr.objc.autorelease_pool`.

    Forces `_HAS_AUTORELEASE = True` so the wrapping branch is actually
    exercised regardless of whether PyObjC is importable on the host.
    Returns the pool_factory mock (call it like `pool_factory()` to get cm).
    """
    from recog import asr as asr_module

    pool_factory = _make_pool_mock()
    fake_objc = MagicMock()
    fake_objc.autorelease_pool = pool_factory
    monkeypatch.setattr(asr_module, "objc", fake_objc)
    monkeypatch.setattr(asr_module, "_HAS_AUTORELEASE", True)
    return pool_factory


def test_mps_autoreleasepool_mps_device_with_pyobjc_enters_and_exits(monkeypatch):
    """
    given: device == "mps" and PyObjC is available
    when: _mps_autoreleasepool("mps") is used as a context manager
    then: objc.autorelease_pool().__enter__/__exit__ are each called once
    """
    from recog.asr import _mps_autoreleasepool

    pool_factory = _install_pool_mock(monkeypatch)

    with _mps_autoreleasepool("mps"):
        pass

    pool_factory.assert_called_once_with()
    cm = pool_factory.return_value
    cm.__enter__.assert_called_once()
    cm.__exit__.assert_called_once()


def test_mps_autoreleasepool_cpu_device_does_not_call_objc(monkeypatch):
    """
    given: device == "cpu"
    when: _mps_autoreleasepool("cpu") is used as a context manager
    then: objc.autorelease_pool is not called at all
    """
    from recog.asr import _mps_autoreleasepool

    pool_factory = _install_pool_mock(monkeypatch)

    with _mps_autoreleasepool("cpu"):
        pass

    pool_factory.assert_not_called()


def test_mps_autoreleasepool_mps_without_pyobjc_is_noop(monkeypatch):
    """
    given: device == "mps" but _HAS_AUTORELEASE is False (no PyObjC)
    when: _mps_autoreleasepool("mps") is used as a context manager
    then: no crash and objc is not invoked
    """
    from recog import asr as asr_module
    from recog.asr import _mps_autoreleasepool

    pool_factory = _install_pool_mock(monkeypatch)
    monkeypatch.setattr(asr_module, "_HAS_AUTORELEASE", False)

    with _mps_autoreleasepool("mps"):
        pass

    pool_factory.assert_not_called()


def test_mps_autoreleasepool_drains_on_exception_inside_with_block(monkeypatch):
    """
    given: device == "mps", PyObjC available, body raises
    when: exception is raised inside the with-block
    then: __exit__ is invoked with non-None exc_info (pool drains) and the
          exception propagates to the caller
    """
    from recog.asr import _mps_autoreleasepool

    pool_factory = _install_pool_mock(monkeypatch)

    class _Boom(Exception):
        pass

    with pytest.raises(_Boom):
        with _mps_autoreleasepool("mps"):
            raise _Boom("inner")

    cm = pool_factory.return_value
    cm.__enter__.assert_called_once()
    cm.__exit__.assert_called_once()
    exit_args, _exit_kwargs = cm.__exit__.call_args
    # contextlib's contextmanager forwards (exc_type, exc_value, traceback)
    assert exit_args[0] is _Boom
    assert isinstance(exit_args[1], _Boom)
    assert exit_args[2] is not None


def test_maybe_empty_mps_cache_99_calls_no_trigger_100th_triggers_and_resets(monkeypatch):
    """
    given: ASRModule with device == "mps" and a fresh _mps_inference_count
    when: _maybe_empty_mps_cache is invoked 100 times
    then: the first 99 do NOT call torch.mps.empty_cache, the 100th does, and
          the counter resets to 0 after firing
    """
    asr, _ = _make_mocked_asr(decoded_texts=[])
    asr.device = "mps"
    asr._mps_inference_count = 0

    empty_cache = MagicMock()
    monkeypatch.setattr(torch.mps, "empty_cache", empty_cache)

    for _ in range(ASRModule._MPS_EMPTY_CACHE_EVERY - 1):
        asr._maybe_empty_mps_cache()
    empty_cache.assert_not_called()
    assert asr._mps_inference_count == ASRModule._MPS_EMPTY_CACHE_EVERY - 1

    asr._maybe_empty_mps_cache()  # 100th call
    empty_cache.assert_called_once_with()
    assert asr._mps_inference_count == 0


def test_maybe_empty_mps_cache_cpu_device_is_noop_even_after_many_calls(monkeypatch):
    """
    given: ASRModule with device == "cpu"
    when: _maybe_empty_mps_cache is invoked many times
    then: torch.mps.empty_cache is never called and counter stays at 0
    """
    asr, _ = _make_mocked_asr(decoded_texts=[])
    asr.device = "cpu"
    asr._mps_inference_count = 0

    empty_cache = MagicMock()
    monkeypatch.setattr(torch.mps, "empty_cache", empty_cache)

    for _ in range(ASRModule._MPS_EMPTY_CACHE_EVERY * 2):
        asr._maybe_empty_mps_cache()

    empty_cache.assert_not_called()
    assert asr._mps_inference_count == 0


# -----------------------------------------------------------------------------
# Hot-path wrapping: transcribe_batch / transcribe_with_timestamps /
# transcribe_simple wrap each inference in _mps_autoreleasepool and call
# _maybe_empty_mps_cache when device == "mps". Also asserts the symmetric
# negative case (device == "cpu") to guard against silently dropped wrapping.
# -----------------------------------------------------------------------------


def test_transcribe_batch_on_mps_wraps_inference_in_pool_and_calls_cache_helper(monkeypatch):
    """
    given: ASRModule with device == "mps", PyObjC patched in
    when: transcribe_batch runs a single batch
    then: objc.autorelease_pool is entered and exited exactly once and
          _maybe_empty_mps_cache is called exactly once after decode
    """
    pool_factory = _install_pool_mock(monkeypatch)

    asr, model = _make_mocked_asr(decoded_texts=["hello"])
    asr.device = "mps"
    asr._mps_inference_count = 0

    cache_calls = []
    monkeypatch.setattr(
        ASRModule,
        "_maybe_empty_mps_cache",
        lambda self: cache_calls.append(1),
    )

    asr.transcribe_batch([np.full(16000, 100, dtype=np.int16)])

    pool_factory.assert_called_once_with()
    cm = pool_factory.return_value
    cm.__enter__.assert_called_once()
    cm.__exit__.assert_called_once()
    assert len(cache_calls) == 1, "expected one cache-helper call after decode"
    model.forward.assert_called_once()
    model.decoding.decode.assert_called_once()


def test_transcribe_batch_on_cpu_does_not_enter_pool(monkeypatch):
    """
    given: ASRModule with device == "cpu", PyObjC patched in
    when: transcribe_batch runs
    then: objc.autorelease_pool is NEVER called (negative guard)

    Symmetric negative test — protects against silently dropped wrapping where
    the helper is no-op'd on cpu and the test would pass without exercising
    the actual MPS wrapping path.
    """
    pool_factory = _install_pool_mock(monkeypatch)

    asr, _ = _make_mocked_asr(decoded_texts=["hello"])
    asr.device = "cpu"
    asr._mps_inference_count = 0

    asr.transcribe_batch([np.full(16000, 100, dtype=np.int16)])

    pool_factory.assert_not_called()


def test_transcribe_with_timestamps_str_path_on_mps_wraps_in_pool_and_calls_cache_helper(monkeypatch, tmp_path):
    """
    given: ASRModule with device == "mps", str audio path
    when: transcribe_with_timestamps runs
    then: objc.autorelease_pool is entered/exited once and
          _maybe_empty_mps_cache is called once
    """
    pool_factory = _install_pool_mock(monkeypatch)

    asr, model = _make_timestamps_asr(_FakeResult(text="ok", words=[]))
    asr.device = "mps"
    asr._mps_inference_count = 0

    wav_path = tmp_path / "input.wav"
    sf.write(str(wav_path), np.zeros(16000, dtype=np.float32), 16000, subtype="FLOAT")

    cache_calls = []
    monkeypatch.setattr(
        ASRModule,
        "_maybe_empty_mps_cache",
        lambda self: cache_calls.append(1),
    )

    asr.transcribe_with_timestamps(str(wav_path))

    pool_factory.assert_called_once_with()
    cm = pool_factory.return_value
    cm.__enter__.assert_called_once()
    cm.__exit__.assert_called_once()
    assert len(cache_calls) == 1
    model.transcribe.assert_called_once()


def test_transcribe_with_timestamps_str_path_on_cpu_does_not_enter_pool(monkeypatch, tmp_path):
    """
    given: ASRModule with device == "cpu", str audio path
    when: transcribe_with_timestamps runs
    then: objc.autorelease_pool is NEVER called (negative guard)
    """
    pool_factory = _install_pool_mock(monkeypatch)

    asr, _ = _make_timestamps_asr(_FakeResult(text="ok", words=[]))
    asr.device = "cpu"
    asr._mps_inference_count = 0

    wav_path = tmp_path / "input.wav"
    sf.write(str(wav_path), np.zeros(16000, dtype=np.float32), 16000, subtype="FLOAT")

    asr.transcribe_with_timestamps(str(wav_path))

    pool_factory.assert_not_called()


def test_transcribe_with_timestamps_ndarray_on_mps_wraps_in_pool_and_calls_cache_helper(monkeypatch):
    """
    given: ASRModule with device == "mps", numpy audio array
    when: transcribe_with_timestamps runs (writes temp WAV, then calls model)
    then: objc.autorelease_pool is entered/exited once around model.transcribe
          and _maybe_empty_mps_cache is called once
    """
    pool_factory = _install_pool_mock(monkeypatch)

    asr, model = _make_timestamps_asr(_FakeResult(text="ok", words=[]))
    asr.device = "mps"
    asr._mps_inference_count = 0

    cache_calls = []
    monkeypatch.setattr(
        ASRModule,
        "_maybe_empty_mps_cache",
        lambda self: cache_calls.append(1),
    )

    asr.transcribe_with_timestamps(np.zeros(16000, dtype=np.float32))

    pool_factory.assert_called_once_with()
    cm = pool_factory.return_value
    cm.__enter__.assert_called_once()
    cm.__exit__.assert_called_once()
    assert len(cache_calls) == 1
    model.transcribe.assert_called_once()


def test_transcribe_with_timestamps_ndarray_on_cpu_does_not_enter_pool(monkeypatch):
    """
    given: ASRModule with device == "cpu", numpy audio array
    when: transcribe_with_timestamps runs
    then: objc.autorelease_pool is NEVER called (negative guard)
    """
    pool_factory = _install_pool_mock(monkeypatch)

    asr, _ = _make_timestamps_asr(_FakeResult(text="ok", words=[]))
    asr.device = "cpu"
    asr._mps_inference_count = 0

    asr.transcribe_with_timestamps(np.zeros(16000, dtype=np.float32))

    pool_factory.assert_not_called()


def test_transcribe_simple_on_mps_one_pool_entry_per_chunk(monkeypatch, tmp_path):
    """
    given: ASRModule with device == "mps" and audio long enough for 3 chunks
    when: transcribe_simple is invoked
    then: objc.autorelease_pool is entered N times (one per chunk) and
          _maybe_empty_mps_cache is called N times
    """
    import types

    pool_factory = _install_pool_mock(monkeypatch)

    asr, model = _make_mocked_asr(decoded_texts=["hello"])
    asr.device = "mps"
    asr._mps_inference_count = 0

    # 3 seconds of audio at 16 kHz with chunk_duration=1.0/overlap=0.0
    # => 3 chunks (start_sample steps 0, 16000, 32000).
    fake_preprocess = types.ModuleType("gigaam.preprocess")
    fake_preprocess.load_audio = lambda _path: torch.zeros(48000)
    fake_preprocess.SAMPLE_RATE = 16000
    monkeypatch.setitem(sys.modules, "gigaam.preprocess", fake_preprocess)

    cache_calls = []
    monkeypatch.setattr(
        ASRModule,
        "_maybe_empty_mps_cache",
        lambda self: cache_calls.append(1),
    )

    asr.transcribe_simple(
        str(tmp_path / "dummy.wav"),
        chunk_duration=1.0,
        overlap=0.0,
        quiet=True,
    )

    expected_chunks = 3
    assert pool_factory.call_count == expected_chunks, (
        f"expected pool entered once per chunk ({expected_chunks}); got {pool_factory.call_count}"
    )
    cm = pool_factory.return_value
    assert cm.__enter__.call_count == expected_chunks
    assert cm.__exit__.call_count == expected_chunks
    assert len(cache_calls) == expected_chunks
    assert model.forward.call_count == expected_chunks


def test_transcribe_simple_on_cpu_does_not_enter_pool(monkeypatch, tmp_path):
    """
    given: ASRModule with device == "cpu" and 3-chunk audio
    when: transcribe_simple is invoked
    then: objc.autorelease_pool is NEVER called regardless of chunk count
    """
    import types

    pool_factory = _install_pool_mock(monkeypatch)

    asr, _ = _make_mocked_asr(decoded_texts=["hello"])
    asr.device = "cpu"
    asr._mps_inference_count = 0

    fake_preprocess = types.ModuleType("gigaam.preprocess")
    fake_preprocess.load_audio = lambda _path: torch.zeros(48000)
    fake_preprocess.SAMPLE_RATE = 16000
    monkeypatch.setitem(sys.modules, "gigaam.preprocess", fake_preprocess)

    asr.transcribe_simple(
        str(tmp_path / "dummy.wav"),
        chunk_duration=1.0,
        overlap=0.0,
        quiet=True,
    )

    pool_factory.assert_not_called()


def _make_live_transcriber_for_direct(asr: ASRModule):
    """Build a minimal `LiveTranscriber`-shaped object for `_transcribe_direct`.

    Uses `__new__` to bypass the heavy `__init__` (which constructs capture
    devices, ASR module, etc.). Sets only the attributes `_transcribe_direct`
    touches: `asr`, `_model_device`, `_model_dtype`, `_refine_worker`.
    """
    from recog.live import LiveTranscriber

    transcriber = LiveTranscriber.__new__(LiveTranscriber)
    transcriber.asr = asr
    transcriber._model_device = None
    transcriber._model_dtype = None
    transcriber._refine_worker = None
    return transcriber


def test_transcribe_direct_on_mps_wraps_inference_in_pool_and_calls_cache_helper(monkeypatch):
    """
    given: LiveTranscriber whose ASR is on device == "mps", PyObjC patched in
    when: _transcribe_direct runs on a 1 s float32 chunk
    then: objc.autorelease_pool is entered/exited once and
          asr._maybe_empty_mps_cache is called once

    Mirrors the live mic / fallback-mixed path that bypasses transcribe_batch
    and goes straight to model.forward + decode under self.asr._model_lock.
    """
    pool_factory = _install_pool_mock(monkeypatch)

    asr, model = _make_mocked_asr(decoded_texts=["live text"])
    asr.device = "mps"
    asr._mps_inference_count = 0

    cache_calls = []
    monkeypatch.setattr(
        ASRModule,
        "_maybe_empty_mps_cache",
        lambda self: cache_calls.append(1),
    )

    transcriber = _make_live_transcriber_for_direct(asr)
    audio = np.zeros(16000, dtype=np.float32)
    text = transcriber._transcribe_direct(audio)

    assert text == "live text"
    pool_factory.assert_called_once_with()
    cm = pool_factory.return_value
    cm.__enter__.assert_called_once()
    cm.__exit__.assert_called_once()
    assert len(cache_calls) == 1
    model.forward.assert_called_once()
    model.decoding.decode.assert_called_once()


def test_transcribe_direct_on_cpu_does_not_enter_pool(monkeypatch):
    """
    given: LiveTranscriber whose ASR is on device == "cpu"
    when: _transcribe_direct runs
    then: objc.autorelease_pool is NEVER called (negative guard)
    """
    pool_factory = _install_pool_mock(monkeypatch)

    asr, _ = _make_mocked_asr(decoded_texts=["live text"])
    asr.device = "cpu"
    asr._mps_inference_count = 0

    transcriber = _make_live_transcriber_for_direct(asr)
    audio = np.zeros(16000, dtype=np.float32)
    transcriber._transcribe_direct(audio)

    pool_factory.assert_not_called()
