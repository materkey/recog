"""Per-speaker refine contour: scaffold.

When `RECOG_PCM_REFINE=1` (gated on `RECOG_PCM_TRANSPORT=1`), the refine
worker reruns `ASRModule.transcribe_with_timestamps` on a trailing per-pid
PCM window and feeds the aligned result back into `LiveTranscriber` to
replace live-segment text with `refined: true`.

This module contains the immutable task/result records and the worker
shell. The worker loop, priority-abort logic, and live-pending gate are
fleshed out in Task 4; see `docs/plans/2026-04-24-per-speaker-refine-backend.md`.
"""

from __future__ import annotations

import threading
from dataclasses import dataclass
from typing import Callable, List, Literal

import numpy as np

from gigaam.types import TranscriptionResult, Word


AbortMode = Literal["cooperative", "coarse"]


@dataclass(frozen=True, eq=False)
class RefineTask:
    """Trailing PCM window submitted to the refine worker for one pid.

    Immutable payload — the worker must not mutate it. Equality is object
    identity (`eq=False`) to avoid numpy's broadcasting `__eq__`.

    Attributes:
        pid: Participant id (KTalk extension's stable per-speaker key).
        pcm_samples: Int16 mono PCM at ASR sample rate.
        offset_base_ms: Per-pid audio-clock offset of `pcm_samples[0]`; used
            to rebase upstream word timestamps back into the pid frame.
        submitted_at: `time.time()` at `PerSpeakerIngest.get_refine_candidate`,
            for queue-age logging/metrics.
    """

    pid: str
    pcm_samples: np.ndarray
    offset_base_ms: float
    submitted_at: float


@dataclass(frozen=True, eq=False)
class RefineResult:
    """Output of one `transcribe_with_timestamps` run for a `RefineTask`.

    Attributes:
        task: The originating task (preserved for alignment back into
            live segments).
        text: Refined concatenated text.
        words: Per-word timestamps (start/end already in milliseconds,
            normalised by `ASRModule.transcribe_with_timestamps`).
    """

    task: RefineTask
    text: str
    words: List[Word]


TranscribeFn = Callable[[np.ndarray], TranscriptionResult]
ApplyFn = Callable[[RefineResult], None]


class RefineWorker:
    """Serialised refine worker — scaffold.

    Owns a daemon thread that pops `RefineTask`s, runs `transcribe_fn`
    under `model_lock` (shared with the live batched-forward path), and
    hands results to `apply_fn`. Full behaviour is implemented in Task 4;
    right now only the constructor is live — `submit/pause/resume/stop`
    raise `NotImplementedError`.

    Args:
        model_lock: Shared lock around the GigaAM model (serialises live
            and refine forward passes).
        transcribe_fn: `ASRModule.transcribe_with_timestamps`-shaped
            callable. Task 4 wires in `should_abort` for cooperative mode.
        apply_fn: Called with `RefineResult` OUTSIDE `model_lock`; maps to
            `LiveTranscriber._apply_refine`.
        abort_mode: `"cooperative"` polls `should_abort` per decoder step
            (default — see `experiments/2026-04-24-refine-abort-spike.md`);
            `"coarse"` always runs to completion.
    """

    def __init__(
        self,
        model_lock: threading.Lock,
        transcribe_fn: TranscribeFn,
        apply_fn: ApplyFn,
        abort_mode: AbortMode = "cooperative",
    ):
        if abort_mode not in ("cooperative", "coarse"):
            raise ValueError(
                f"abort_mode must be 'cooperative' or 'coarse', got {abort_mode!r}"
            )
        self._model_lock = model_lock
        self._transcribe_fn = transcribe_fn
        self._apply_fn = apply_fn
        self._abort_mode: AbortMode = abort_mode

    @property
    def abort_mode(self) -> AbortMode:
        return self._abort_mode

    def submit(self, task: RefineTask) -> None:
        raise NotImplementedError("RefineWorker.submit — implemented in Task 4")

    def pause(self) -> None:
        raise NotImplementedError("RefineWorker.pause — implemented in Task 4")

    def resume(self) -> None:
        raise NotImplementedError("RefineWorker.resume — implemented in Task 4")

    def stop(self) -> None:
        raise NotImplementedError("RefineWorker.stop — implemented in Task 4")
