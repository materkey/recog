"""Silero VAD v6 wrapper for the per-speaker PCM pipeline.

Applied in `LiveTranscriber._on_pcm_batch` between the PerSpeaker ingest
batch and `ASRModule.transcribe_batch` to drop near-silence chunks before
they reach GigaAM.  GigaAM is an RNN-T seq2seq model that hallucinates
short filler words ("Да", "Угу", "Вот", "Ок") on low-energy PCM; the mic
path already runs an RMS VAD in `LiveTranscriber._process_chunk`, but the
PCM path reached ASR unfiltered.

The model is the upstream `silero_vad` ONNX build (MIT).  It is loaded
lazily on first use so services can start without paying the ONNX
import cost when `RECOG_PCM_VAD=0`.
"""

from __future__ import annotations

import sys
import threading
from typing import Optional

import numpy as np


_model = None
_model_lock = threading.Lock()
_torch = None


def _get_model():
    """Lazy-load the Silero ONNX model.  Returns None on import failure.

    `get_speech_timestamps` calls `model.reset_states()` internally, so the
    same shared instance is safe across pids without per-pid bookkeeping.
    """
    global _model, _torch
    if _model is not None:
        return _model
    with _model_lock:
        if _model is not None:
            return _model
        try:
            import torch  # type: ignore
            from silero_vad import load_silero_vad  # type: ignore
            _torch = torch
            _model = load_silero_vad(onnx=True)
        except Exception as exc:  # pragma: no cover — defensive
            print(f"[VAD] silero load failed: {exc}; VAD disabled", file=sys.stderr)
            _model = None
    return _model


def pcm_int16_has_speech(
    pcm_i16: np.ndarray,
    sample_rate: int = 16000,
    threshold: float = 0.3,
) -> bool:
    """True iff Silero detects at least one speech span in this PCM window.

    `pcm_i16` must be 1-D int16 @ `sample_rate`.  Returns True on any
    loader / inference error (fail-open: never drop a chunk because VAD
    itself failed — GigaAM hallucinations are preferable to lost speech).
    """
    if pcm_i16.size == 0:
        return False
    model = _get_model()
    if model is None or _torch is None:
        return True  # fail-open
    try:
        from silero_vad import get_speech_timestamps  # type: ignore
        pcm_f32 = pcm_i16.astype(np.float32) / 32768.0
        segs = get_speech_timestamps(
            _torch.from_numpy(pcm_f32),
            model,
            sampling_rate=sample_rate,
            threshold=threshold,
        )
        return bool(segs)
    except Exception as exc:  # pragma: no cover
        print(f"[VAD] inference error: {exc}; keeping chunk", file=sys.stderr)
        return True


def reset() -> None:
    """Force-reload the model on next call.  For tests only."""
    global _model
    with _model_lock:
        _model = None
