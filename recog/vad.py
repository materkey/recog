"""Silero VAD v6 ONNX wrapper for the per-speaker PCM path."""

from __future__ import annotations

import sys
import threading

import numpy as np


_FRAME_SAMPLES = 512
_SAMPLE_RATE = 16000

_model = None
_model_lock = threading.Lock()
_torch = None


def _get_model():
    global _model, _torch
    if _model is not None:
        return _model
    with _model_lock:
        if _model is not None:
            return _model
        try:
            import torch
            from silero_vad import load_silero_vad
            _torch = torch
            _model = load_silero_vad(onnx=True)
        except (ImportError, RuntimeError, OSError) as exc:
            print(f"[VAD] silero load failed: {exc}; VAD disabled", file=sys.stderr)
            _model = None
    return _model


def pcm_int16_has_speech(pcm_i16: np.ndarray, threshold: float) -> bool:
    """True iff any 512-sample window in `pcm_i16` crosses `threshold`.

    Early-exits on the first speech window — on speech-full chunks this
    is ~19× faster than `get_speech_timestamps` because we avoid scanning
    the rest of the window and building the span list. Fails open on any
    model / inference error: GigaAM hallucinations are preferable to
    lost speech.
    """
    if pcm_i16.size < _FRAME_SAMPLES:
        return False
    model = _get_model()
    if model is None or _torch is None:
        return True
    try:
        model.reset_states()
        pcm_f32 = pcm_i16.astype(np.float32) / 32768.0
        t = _torch.from_numpy(pcm_f32)
        for start in range(0, pcm_f32.size - _FRAME_SAMPLES + 1, _FRAME_SAMPLES):
            if model(t[start:start + _FRAME_SAMPLES], _SAMPLE_RATE).item() >= threshold:
                return True
        return False
    except (RuntimeError, ValueError) as exc:
        print(f"[VAD] inference error: {exc}; keeping chunk", file=sys.stderr)
        return True


def pcm_int16_vad_details(pcm_i16: np.ndarray) -> tuple[float, int, int]:
    """Diagnostic scan: return (max_prob, total_frames, frames_>=_0.3_bucket).

    Scans every 512-sample window without early-exit, so callers can
    report precisely WHY a chunk was kept or dropped.  Used only on the
    debug path (RECOG_PCM_DEBUG=1) — the hot path still uses
    `pcm_int16_has_speech` with early-exit.  Returns (0.0, 0, 0) on
    load/inference failure so debug lines stay well-formed.
    """
    if pcm_i16.size < _FRAME_SAMPLES:
        return 0.0, 0, 0
    model = _get_model()
    if model is None or _torch is None:
        return 0.0, 0, 0
    try:
        model.reset_states()
        pcm_f32 = pcm_i16.astype(np.float32) / 32768.0
        t = _torch.from_numpy(pcm_f32)
        max_prob = 0.0
        total = 0
        above = 0
        for start in range(0, pcm_f32.size - _FRAME_SAMPLES + 1, _FRAME_SAMPLES):
            prob = model(t[start:start + _FRAME_SAMPLES], _SAMPLE_RATE).item()
            total += 1
            if prob > max_prob:
                max_prob = prob
            if prob >= 0.3:
                above += 1
        return max_prob, total, above
    except (RuntimeError, ValueError) as exc:
        print(f"[VAD] debug inference error: {exc}", file=sys.stderr)
        return 0.0, 0, 0


def reset() -> None:
    """Force-reload the model on next call. For tests only."""
    global _model
    with _model_lock:
        _model = None
