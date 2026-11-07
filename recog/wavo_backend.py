"""ASR backend "wavo": GigaAM v3 e2e_rnnt on wavo, a Rust runtime, on the GPU.

The model is the one ``ASRModule`` runs on PyTorch, quantized to Q8_0; its WER is
within 0.06 points of PyTorch's and it is 6-12 times faster on the Mac
(docs/experiments/2026-11-07-rust-asr-runtimes.md). The Rust side is the module
``wavo_asr`` built from rust/wavo-asr-py; this adapter gives it ASRModule's shape.

What differs from ASRModule:

- no batch: ``transcribe_batch`` runs the clips one after another;
- no abort of a decode under way: a refine (60-100 ms on the Mac) runs to its end,
  so ``LiveTranscriber._refine_transcribe_fn`` only checks before starting;
- no torch, so no device choice: wavo picks the GPU (Metal on macOS).
"""

from __future__ import annotations

import sys
import threading
from dataclasses import dataclass, field
from typing import Any, List, Optional

import numpy as np

from .log_utils import log_rate_limited

SAMPLE_RATE = 16000
# GigaAM's encoder frame: wavo gives each token's start as its frame times 40 ms.
FRAME_MS = 40


@dataclass
class Word:
    """A word with ``start``/``end`` in ms, as ``ASRModule.transcribe_with_timestamps`` gives."""

    text: str
    start: int
    end: int


@dataclass
class Result:
    text: str
    words: List[Word] = field(default_factory=list)


def words_from_tokens(tokens) -> List[Word]:
    """Words from ``(piece, start_ms)`` tokens the way GigaAM's ``frames_to_words`` makes them.

    A piece starting with "▁" starts a word; a word runs from its first token's frame
    to the end of its last token's frame.
    """
    words: List[Word] = []
    pieces: List[str] = []
    starts: List[int] = []

    def commit():
        text = "".join(pieces).strip()
        if text:
            words.append(Word(text, starts[0], starts[-1] + FRAME_MS))
        pieces.clear()
        starts.clear()

    for piece, start in tokens:
        if piece.startswith("▁"):
            commit()
            piece = piece[1:]
        pieces.append(piece)
        starts.append(int(start))
    commit()
    return words


def _pcm_bytes(audio: Any, sample_rate: int) -> bytes:
    if sample_rate != SAMPLE_RATE:
        raise ValueError(f"wavo takes {SAMPLE_RATE} Hz audio, got {sample_rate}")
    arr = np.asarray(audio)
    if arr.ndim > 1:
        arr = np.squeeze(arr)
    if arr.ndim != 1:
        raise ValueError(f"audio must be 1-D mono; got shape {arr.shape}")
    if arr.dtype == np.int16:
        arr = arr.astype(np.float32) / 32768.0
    return arr.astype("<f4", copy=False).tobytes()


class WavoASR:
    """GigaAM v3 e2e_rnnt on wavo with the methods recog's live and refine paths call."""

    available = True

    def __init__(self):
        self.model = None
        self.device = "wavo"
        # The same role as ASRModule's: RefineWorker and _refine_transcribe_fn hold it
        # around a refine, and live calls take it, so live waits for at most one refine.
        self._model_lock = threading.RLock()

    def load_model(self, quiet: bool = False):
        if self.model is None:
            with self._model_lock:
                if self.model is None:
                    import wavo_asr

                    if not quiet:
                        print("Загрузка модели gigaam-v3 (wavo)...", file=sys.stderr)
                    model = wavo_asr.Model()
                    self.device = f"wavo {model.device}"
                    self.model = model
        return self.model

    def _transcribe(self, audio: Any, sample_rate: int = SAMPLE_RATE):
        pcm = _pcm_bytes(audio, sample_rate)
        model = self.load_model(quiet=True)
        with self._model_lock:
            return model.transcribe(pcm)

    def transcribe_chunk(self, audio: np.ndarray, sample_rate: int = SAMPLE_RATE) -> Optional[str]:
        """The live mic/mixed chunk's text, None when empty or on any error."""
        try:
            text, _tokens = self._transcribe(audio, sample_rate)
            return text.strip() or None
        except Exception as e:
            log_rate_limited(
                "asr-transcribe-chunk",
                f"[WavoASR] transcribe_chunk failed: {type(e).__name__}: {e}",
            )
            return None

    def transcribe_batch(self, pcm_list: List[np.ndarray]) -> List[str]:
        """One text per per-speaker clip, in order."""
        return [self._transcribe(pcm)[0] for pcm in pcm_list]

    def transcribe_with_timestamps(
        self, audio: Any, sample_rate: int = SAMPLE_RATE, quiet: bool = False
    ) -> Result:
        """The text and its words with ``start``/``end`` in ms, for refine."""
        text, tokens = self._transcribe(audio, sample_rate)
        return Result(text, words_from_tokens(tokens))
