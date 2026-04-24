"""Per-speaker refine contour: worker + tasks.

When `RECOG_PCM_REFINE=1` (gated on `RECOG_PCM_TRANSPORT=1`), `RefineWorker`
reruns `ASRModule.transcribe_with_timestamps` on a trailing per-pid PCM
window and feeds the aligned result back into `LiveTranscriber` to replace
live-segment text with `refined: true`.

Threading model (design.md D11):
    - The worker owns a daemon thread that pops `RefineTask`s from an
      internal queue, acquires a shared `model_lock` (serialises with the
      live batched-forward path), and runs `transcribe_fn`.
    - In `cooperative` mode the worker passes a `should_abort=` closure
      (reading `_live_pending`) into `transcribe_fn`; the upstream decoder
      monkey-patch (see `experiments/2026-04-24-refine-abort-spike.md`)
      polls it between time-steps and raises `RefineAborted` on request.
      The worker catches the exception, releases the lock, and re-enqueues
      the task so the next live batch can run without waiting for the
      refine pass to finish.
    - In `coarse` mode the worker runs to completion; live waits at most
      one refine-window for the shared model.
"""

from __future__ import annotations

import queue
import sys
import threading
import time
import traceback
from dataclasses import dataclass
from typing import Any, Callable, List, Literal, Optional

import numpy as np

from gigaam.types import TranscriptionResult, Word


AbortMode = Literal["cooperative", "coarse"]


class RefineAborted(Exception):
    """Raised by the cooperative-mode decoder monkey-patch when
    `should_abort()` returns True mid-inference. The worker catches this,
    releases `model_lock`, and re-enqueues the originating `RefineTask`.
    """


def make_abortable_decode(decoding: Any, should_abort: Callable[[], bool]) -> Callable:
    """Return a drop-in replacement for `RNNTGreedyDecoding.decode` that polls
    `should_abort()` between encoder time steps and raises `RefineAborted`
    when True.

    The body mirrors upstream `gigaam/decoding.py::RNNTGreedyDecoding.decode`
    (commit `6e4b027`) verbatim except for the added
    `if should_abort(): raise RefineAborted()` at the top of the outer
    `for t in range(T):` loop. See
    `experiments/2026-04-24-refine-abort-spike.md` for the spike that
    validated this hook — worst-case abort latency is the encoder forward
    pass (~187 ms on MPS for 30 s audio), saving ~770 ms per aborted refine.

    Install the returned callable by assigning it to `model.decoding.decode`
    for the duration of one `transcribe` call (wrap in `try/finally` to
    restore the original).

    Args:
        decoding: The `model.decoding` instance. Used to capture tokenizer,
            blank_id, max_symbols, and the `_cat_states`/`_split_state`
            helpers into the closure.
        should_abort: No-arg callable returning True when inference should
            be cut short. The worker typically passes
            `self._live_pending.is_set` here.

    Returns:
        Callable with the shape `decode(head, encoded, enc_len)` matching
        the upstream decoder; suitable for drop-in replacement via
        `model.decoding.decode = make_abortable_decode(...)`.
    """
    import torch

    tokenizer = decoding.tokenizer
    blank_id = decoding.blank_id
    max_symbols = decoding.max_symbols

    cat_states = decoding._cat_states
    split_state = decoding._split_state

    @torch.inference_mode()
    def decode(head, encoded, enc_len):
        x = encoded.transpose(1, 2)
        B, T, _ = x.shape
        device = x.device

        hyps: list[list[int]] = [[] for _ in range(B)]
        token_frames: list[list[int]] = [[] for _ in range(B)]
        last_label: list = [None] * B
        dec_state: list = [None] * B

        def emit_batch(batch_idx, t, fresh):
            idx = torch.tensor(batch_idx, device=device, dtype=torch.long)
            f = x[idx, t:t + 1, :]
            if fresh:
                g, hidden = head.decoder.predict(None, None, batch_size=len(batch_idx))
            else:
                labels = torch.cat([last_label[i] for i in batch_idx], dim=0)
                state = cat_states([dec_state[i] for i in batch_idx])
                g, hidden = head.decoder.predict(labels, state, batch_size=len(batch_idx))

            k = head.joint.joint(f, g)[:, 0, 0, :].argmax(dim=-1)
            emit = k.ne(blank_id)
            if not emit.any():
                return []

            hidden_parts = split_state(hidden)
            out = []
            for p in emit.nonzero(as_tuple=False).squeeze(1).tolist():
                bi = batch_idx[p]
                tok = int(k[p])
                hyps[bi].append(tok)
                token_frames[bi].append(t)
                last_label[bi] = k[p:p + 1].view(1, 1)
                dec_state[bi] = hidden_parts[p]
                out.append(bi)
            return out

        enc_len_cpu = enc_len.cpu()
        for t in range(T):
            if should_abort():
                raise RefineAborted(f"abort@t={t}/{T}")
            active = (t < enc_len_cpu).nonzero(as_tuple=False).squeeze(1).tolist()
            if not active:
                break
            for _ in range(max_symbols):
                if not active:
                    break
                fresh = [i for i in active if dec_state[i] is None]
                stateful = [i for i in active if dec_state[i] is not None]

                next_active = []
                if fresh:
                    next_active.extend(emit_batch(fresh, t, fresh=True))
                if stateful:
                    next_active.extend(emit_batch(stateful, t, fresh=False))

                if not next_active:
                    break
                active = next_active

        return [(tokenizer.decode(h), h, tf) for h, tf in zip(hyps, token_frames)]

    return decode


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
        meeting_generation: LiveTranscriber's `_meeting_generation` snapshot
            at submit time. `_apply_refine` drops the result if the current
            generation no longer matches (meeting switched between dispatch
            and apply — same pattern as `_on_pcm_batch`, design.md D11).
    """

    pid: str
    pcm_samples: np.ndarray
    offset_base_ms: float
    submitted_at: float
    meeting_generation: int = 0


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


# In cooperative mode the worker calls
# `transcribe_fn(pcm, should_abort=<bool-getter>)`; in coarse mode it calls
# `transcribe_fn(pcm)`. Typed as `Callable[..., TranscriptionResult]` to
# accommodate both shapes without forcing callers to accept kwargs.
TranscribeFn = Callable[..., TranscriptionResult]
ApplyFn = Callable[["RefineResult"], None]


class RefineWorker:
    """Serialised refine worker.

    Owns a daemon thread that pops `RefineTask`s, runs `transcribe_fn`
    under `model_lock` (shared with the live batched-forward path), and
    hands results to `apply_fn` outside the lock.

    Lifecycle:
        worker = RefineWorker(lock, transcribe_fn, apply_fn)
        worker.start()           # spawn daemon thread
        worker.submit(task)      # enqueue a RefineTask
        worker.pause() / resume()
        worker.stop()            # signal + join

    Priority signalling (for the live path):
        worker.set_live_pending()    # live is about to acquire model_lock
        worker.clear_live_pending()  # live has released model_lock

    In `cooperative` mode the worker polls `_live_pending` via the
    `should_abort=` closure passed into `transcribe_fn`. When the live path
    needs the model, in-flight refine aborts early (see the decoder
    monkey-patch in `experiments/2026-04-24-refine-abort-spike.md`) and
    the task is re-enqueued for a later attempt.

    Args:
        model_lock: Shared lock around the GigaAM model (serialises live
            and refine forward passes).
        transcribe_fn: Callable of shape `(pcm, *, should_abort=<getter>)`
            returning `TranscriptionResult`. In coarse mode the worker
            omits `should_abort`.
        apply_fn: Called with `RefineResult` OUTSIDE `model_lock`; maps to
            `LiveTranscriber._apply_refine`.
        abort_mode: `"cooperative"` polls `should_abort` per decoder step
            (default — see `experiments/2026-04-24-refine-abort-spike.md`);
            `"coarse"` always runs to completion.
    """

    # queue.get poll interval — small enough to notice `_stopped` promptly,
    # large enough not to dominate CPU when idle.
    _POLL_S: float = 0.2
    # `_live_pending` poll interval used when waiting for the live path to
    # release the model lock before re-contending after a cooperative abort.
    # Python Lock is not fair — without this wait, the worker thread can
    # reacquire `_model_lock` ahead of the waiting live thread, paying the
    # encoder forward cost (~187 ms on MPS per the 2026-04-24 spike) on each
    # immediately-aborted retry. 50 ms polls keep wasted wake-ups low while
    # still reacting quickly once live completes its batch (~200-500 ms).
    _LIVE_POLL_S: float = 0.05

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

        self._queue: "queue.Queue[Optional[RefineTask]]" = queue.Queue()
        # `_paused` and `_queue` must be consulted atomically in `submit`
        # vs `pause`, otherwise a concurrent submit can slip a task in
        # between the pause-flag flip and the drain.
        self._paused: bool = False
        self._paused_lock = threading.Lock()
        self._live_pending = threading.Event()
        self._stopped = threading.Event()
        self._thread: Optional[threading.Thread] = None

    # ----- public API -----------------------------------------------------

    @property
    def abort_mode(self) -> AbortMode:
        return self._abort_mode

    def start(self) -> None:
        """Spawn the daemon worker thread. No-op if already running."""
        if self._thread is not None and self._thread.is_alive():
            return
        self._stopped.clear()
        self._thread = threading.Thread(
            target=self._run,
            name="RefineWorker",
            daemon=True,
        )
        self._thread.start()

    def submit(self, task: RefineTask) -> None:
        """Enqueue a `RefineTask`. No-op if paused or stopped."""
        if self._stopped.is_set():
            return
        with self._paused_lock:
            if self._paused:
                return
            self._queue.put(task)

    def pause(self) -> None:
        """Pause processing and drop any tasks already queued.

        Dropped tasks are NOT rescheduled — the next gap/monolog trigger on
        the owning pid will re-submit with a fresh window.
        """
        with self._paused_lock:
            self._paused = True
            while True:
                try:
                    self._queue.get_nowait()
                except queue.Empty:
                    break

    def resume(self) -> None:
        """Clear the pause flag; subsequent `submit` calls enqueue again."""
        with self._paused_lock:
            self._paused = False

    def stop(self, timeout: float = 2.0) -> None:
        """Signal the worker to exit and wait for the thread to join.

        Drains any residual queue entries (including the wake-up sentinel)
        so `submit` after `stop` leaves the queue empty.
        """
        self._stopped.set()
        # Nudge a blocked `queue.get` so the loop can notice `_stopped`
        # without waiting for the poll timeout.
        try:
            self._queue.put_nowait(None)
        except Exception:
            pass
        if self._thread is not None:
            self._thread.join(timeout=timeout)
            self._thread = None
        while True:
            try:
                self._queue.get_nowait()
            except queue.Empty:
                break

    def set_live_pending(self) -> None:
        """Signal that the live path needs `model_lock`; cooperative mode
        will abort in-flight refine at the next decoder step-boundary."""
        self._live_pending.set()

    def clear_live_pending(self) -> None:
        """Signal that live has released `model_lock`; refine may resume."""
        self._live_pending.clear()

    # ----- internal -------------------------------------------------------

    def _wait_for_live_released(self) -> None:
        """Block until `_live_pending` is cleared (or the worker stops).

        Called from the pre-lock gate in `_process_task` before every
        `_model_lock` acquisition. Live sets the event before acquiring
        `_model_lock` and clears it after `transcribe_batch` returns;
        waiting here lets live win the lock without the worker paying a
        doomed encoder forward pass on the unfair `threading.Lock` race
        (see class docstring and the `_LIVE_POLL_S` comment).
        """
        while self._live_pending.is_set():
            if self._stopped.is_set():
                return
            time.sleep(self._LIVE_POLL_S)

    def _run(self) -> None:
        while not self._stopped.is_set():
            try:
                task = self._queue.get(timeout=self._POLL_S)
            except queue.Empty:
                continue
            if task is None or self._stopped.is_set():
                break
            with self._paused_lock:
                if self._paused:
                    # Drop — pause-drain may have missed this one if it
                    # raced in between pause() and get_nowait.
                    continue
            try:
                self._process_task(task)
            except Exception as e:
                # Swallow + log; never kill the worker thread.
                print(
                    f"[RefineWorker] task processing raised "
                    f"{type(e).__name__}: {e}",
                    file=sys.stderr,
                )
                traceback.print_exc()

    def _process_task(self, task: RefineTask) -> None:
        aborted = False
        result: Optional[TranscriptionResult] = None

        # Cooperative pre-lock gate: if live has already signalled intent to
        # acquire `_model_lock`, wait for it to clear before contending. The
        # decoder monkey-patch polls `_live_pending` inside the time-step
        # loop, but the preceding encoder forward pass (~187 ms on MPS per
        # the 2026-04-24 spike) runs unconditionally. Without this gate a
        # queued refine task can pop right as live signals, win the unfair
        # `threading.Lock` race, pay the encoder cost, and only abort
        # afterwards in decode — leaving live waiting through wasted work.
        # The gate also subsumes the post-abort retry case: when
        # `RefineAborted` bounces the task back onto the queue, the next
        # `_process_task` call lands here first.
        #
        # Coarse mode opts out: design.md D6 states "refine holds the lock
        # for one full 20-s inference ... and live waits." There is no
        # abort path in coarse mode, so the encoder cost is never wasted —
        # deferring at task boundary would just reshuffle an already
        # accepted wait. Keep coarse semantics unchanged.
        if self._abort_mode == "cooperative":
            self._wait_for_live_released()

        with self._model_lock:
            if self._stopped.is_set():
                return
            # Post-lock re-check narrows the TOCTOU window between
            # `_wait_for_live_released()` returning and `_model_lock`
            # acquisition: live can call `set_live_pending()` in that gap,
            # win the race to the queued `with self._model_lock` contention,
            # and still end up waiting through our encoder forward pass
            # (~187 ms on MPS per the 2026-04-24 spike) before the decoder
            # hook aborts. Checking once here — after we hold the lock but
            # before the encoder runs — lets the worker release and requeue
            # the task without paying that cost. Coarse mode has no abort
            # path and accepts the full-window wait by design (D6).
            if (
                self._abort_mode == "cooperative"
                and self._live_pending.is_set()
            ):
                aborted = True
            else:
                try:
                    if self._abort_mode == "cooperative":
                        result = self._transcribe_fn(
                            task.pcm_samples,
                            should_abort=self._live_pending.is_set,
                        )
                    else:
                        result = self._transcribe_fn(task.pcm_samples)
                except RefineAborted:
                    aborted = True

        if aborted:
            # Re-enqueue; the pre-lock gate on the next iteration will wait
            # for live to release `_model_lock` before contending again.
            self._queue.put(task)
            return

        if result is None:
            return

        text = getattr(result, "text", "") or ""
        words_attr = getattr(result, "words", None)
        words: List[Word] = list(words_attr) if words_attr else []
        refine_result = RefineResult(task=task, text=text, words=words)

        # apply_fn runs OUTSIDE `model_lock` — it performs JSONL I/O under
        # its own lock and must not hold the model.
        try:
            self._apply_fn(refine_result)
        except Exception as e:
            print(
                f"[RefineWorker] apply_fn raised "
                f"{type(e).__name__}: {e}",
                file=sys.stderr,
            )
            traceback.print_exc()
