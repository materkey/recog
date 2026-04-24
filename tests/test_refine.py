"""Unit tests for recog/refine.py.

Task 3 scaffolded `RefineTask`/`RefineResult` and the worker shell; Task 4
fleshed out the worker (submit/pause/resume/stop + cooperative abort).
Task 5 adds the lifecycle + abort-coverage tests:

* dataclass construction and immutability,
* constructor validation of `abort_mode`,
* submit → transcribe → apply ordering,
* pause drains queued tasks,
* cooperative abort re-enqueues, coarse mode runs to completion,
* stop joins within timeout.
"""
from __future__ import annotations

import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import List

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from gigaam.types import Word

from recog.refine import RefineAborted, RefineResult, RefineTask, RefineWorker


def _make_task(pid: str = "p1") -> RefineTask:
    return RefineTask(
        pid=pid,
        pcm_samples=np.zeros(16, dtype=np.int16),
        offset_base_ms=0.0,
        submitted_at=time.time(),
    )


@dataclass
class _FakeResult:
    text: str
    words: List[Word]


class TestRefineTask:
    def test_constructs_with_all_fields(self):
        pcm = np.zeros(16000, dtype=np.int16)
        task = RefineTask(
            pid="p1",
            pcm_samples=pcm,
            offset_base_ms=1234.5,
            submitted_at=time.time(),
        )
        assert task.pid == "p1"
        assert task.pcm_samples is pcm
        assert task.offset_base_ms == 1234.5

    def test_is_immutable(self):
        task = _make_task()
        with pytest.raises(Exception):
            task.pid = "p2"  # type: ignore[misc]


class TestRefineResult:
    def test_constructs_with_task_and_words(self):
        task = _make_task()
        words = [Word(text="hi", start=0, end=500), Word(text="world", start=500, end=1200)]
        result = RefineResult(task=task, text="hi world", words=words)
        assert result.task is task
        assert result.text == "hi world"
        assert len(result.words) == 2
        assert result.words[0].text == "hi"

    def test_is_immutable(self):
        task = _make_task()
        result = RefineResult(task=task, text="x", words=[])
        with pytest.raises(Exception):
            result.text = "y"  # type: ignore[misc]


class TestRefineWorkerConstruction:
    def _make_worker(self, abort_mode="cooperative") -> RefineWorker:
        return RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=lambda _pcm, **_kw: _FakeResult("", []),
            apply_fn=lambda _result: None,
            abort_mode=abort_mode,
        )

    def test_constructs_with_default_abort_mode_cooperative(self):
        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=lambda _pcm, **_kw: _FakeResult("", []),
            apply_fn=lambda _result: None,
        )
        assert worker.abort_mode == "cooperative"

    def test_constructs_with_coarse_mode(self):
        worker = self._make_worker(abort_mode="coarse")
        assert worker.abort_mode == "coarse"

    def test_rejects_invalid_abort_mode(self):
        with pytest.raises(ValueError, match="abort_mode"):
            RefineWorker(
                model_lock=threading.Lock(),
                transcribe_fn=lambda _pcm, **_kw: _FakeResult("", []),
                apply_fn=lambda _result: None,
                abort_mode="bogus",  # type: ignore[arg-type]
            )


class TestRefineWorkerLifecycle:
    def test_submit_runs_transcribe_then_apply(self):
        events: List = []
        done = threading.Event()

        def transcribe_fn(pcm, **_kw):
            events.append("transcribe")
            return _FakeResult("hello", [Word(text="hello", start=0, end=100)])

        def apply_fn(result: RefineResult) -> None:
            events.append(("apply", result.text, result.task.pid))
            done.set()

        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=transcribe_fn,
            apply_fn=apply_fn,
        )
        worker.start()
        try:
            worker.submit(_make_task("p1"))
            assert done.wait(timeout=2.0), "apply_fn was not invoked"
        finally:
            worker.stop(timeout=1.0)

        assert events == ["transcribe", ("apply", "hello", "p1")]

    def test_pause_drops_queued_tasks(self):
        transcribe_calls: List = []
        apply_calls: List = []

        def transcribe_fn(pcm, **_kw):
            transcribe_calls.append(pcm)
            return _FakeResult("x", [])

        def apply_fn(_result: RefineResult) -> None:
            apply_calls.append(_result)

        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=transcribe_fn,
            apply_fn=apply_fn,
        )
        # Submit before starting so tasks sit in the queue.
        worker.submit(_make_task("p1"))
        worker.submit(_make_task("p2"))
        worker.pause()

        # Start and wait longer than `_POLL_S` — nothing should be picked up.
        worker.start()
        try:
            time.sleep(0.4)
            assert transcribe_calls == []
            assert apply_calls == []

            # Submits while paused are also dropped.
            worker.submit(_make_task("p3"))
            time.sleep(0.3)
            assert transcribe_calls == []
        finally:
            worker.stop(timeout=1.0)

    def test_pause_rejects_new_submissions(self):
        """Plan 2026-04-24 §1.4: submit() while paused must be a no-op,
        not just queue-and-drop. The check lives inside `submit`, under
        `_paused_lock`, so the task never reaches the queue.
        """
        transcribe_calls: List = []

        def transcribe_fn(pcm, **_kw):
            transcribe_calls.append(pcm)
            return _FakeResult("x", [])

        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=transcribe_fn,
            apply_fn=lambda _r: None,
        )
        worker.start()
        try:
            worker.pause()
            worker.submit(_make_task("p1"))
            worker.submit(_make_task("p2"))
            time.sleep(0.4)  # > _POLL_S
            assert transcribe_calls == [], (
                "submit-while-paused should drop silently"
            )
            # Internal invariant: queue stays empty, not just masked.
            assert worker._queue.empty()
        finally:
            worker.stop(timeout=1.0)

    def test_resume_accepts_submissions(self):
        """Plan 2026-04-24 §1.4: after resume(), submit() must enqueue
        tasks again. Mirrors the watchdog pause→resume cycle that Task 1
        wires into `_emit_pcm_transition_marker`.
        """
        done = threading.Event()

        def transcribe_fn(pcm, **_kw):
            return _FakeResult("ok", [])

        def apply_fn(_result):
            done.set()

        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=transcribe_fn,
            apply_fn=apply_fn,
        )
        worker.start()
        try:
            worker.pause()
            worker.submit(_make_task("p1"))
            time.sleep(0.2)
            assert not done.is_set()

            worker.resume()
            worker.submit(_make_task("p2"))
            assert done.wait(timeout=2.0), (
                "apply_fn not invoked after resume()"
            )
        finally:
            worker.stop(timeout=1.0)

    def test_live_pending_aborts_cooperative(self):
        """Cooperative abort: live signals `_live_pending` mid-inference;
        the decoder monkey-patch (simulated here by transcribe_fn) raises
        `RefineAborted`; the worker re-enqueues and the retry, gated by
        the pre-lock wait, sees the flag cleared.
        """
        calls: List[bool] = []
        done = threading.Event()
        worker_ref: List[RefineWorker] = []

        def transcribe_fn(pcm, should_abort=None):
            assert should_abort is not None, (
                "cooperative mode must pass should_abort"
            )
            if len(calls) == 0:
                # First call: simulate live arriving mid-inference — set the
                # flag as another thread would, observe it via `should_abort`,
                # and raise `RefineAborted` the way the decoder monkey-patch
                # does on the next step boundary.
                worker_ref[0].set_live_pending()
                calls.append(should_abort())
                raise RefineAborted("mid-inference abort")
            # Retry: the pre-lock gate holds off acquisition until
            # `_live_pending` is cleared, so the flag must be False here.
            calls.append(should_abort())
            return _FakeResult("ok", [])

        def apply_fn(_result: RefineResult) -> None:
            done.set()

        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=transcribe_fn,
            apply_fn=apply_fn,
            abort_mode="cooperative",
        )
        worker_ref.append(worker)

        # Watcher clears `_live_pending` once set by transcribe_fn — mirrors
        # the live path completing its acquire/release cycle. Without this,
        # the pre-lock gate on the re-enqueued task would block forever.
        stop_watcher = threading.Event()

        def watcher() -> None:
            while not stop_watcher.is_set():
                if worker._live_pending.is_set():
                    time.sleep(0.02)
                    worker.clear_live_pending()
                    return
                time.sleep(0.01)

        watcher_thread = threading.Thread(target=watcher, daemon=True)
        watcher_thread.start()

        worker.start()
        try:
            worker.submit(_make_task("p1"))
            assert done.wait(timeout=2.0), (
                "apply_fn was not called after re-enqueue"
            )
        finally:
            stop_watcher.set()
            watcher_thread.join(timeout=1.0)
            worker.stop(timeout=1.0)

        assert len(calls) == 2
        assert calls[0] is True  # first call saw live_pending set
        assert calls[1] is False  # retry saw it cleared

    def test_pre_lock_gate_holds_off_initial_acquire_when_live_pending(self):
        """Regression for the unfair-lock race: when `_live_pending` is
        already set at task pop-up time, the worker must not enter
        `_model_lock` and start the encoder forward pass — otherwise the
        cooperative abort would fire only in the decoder, after paying
        ~187 ms of MPS encoder cost (per the 2026-04-24 spike). The gate
        defers acquisition until live clears the flag.
        """
        transcribe_calls: List[bool] = []
        done = threading.Event()

        def transcribe_fn(pcm, should_abort=None):
            transcribe_calls.append(
                should_abort() if should_abort is not None else False
            )
            return _FakeResult("", [])

        def apply_fn(_result: RefineResult) -> None:
            done.set()

        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=transcribe_fn,
            apply_fn=apply_fn,
            abort_mode="cooperative",
        )
        # Flag set BEFORE submit — simulates live holding priority by the
        # time the worker pops the task from the queue.
        worker.set_live_pending()
        worker.start()
        worker.submit(_make_task("p1"))

        try:
            # Wait > _POLL_S + margin so the worker has pulled the task
            # and reached the pre-lock gate.
            time.sleep(0.4)
            assert transcribe_calls == [], (
                "transcribe_fn ran while _live_pending was set — pre-lock "
                "gate is not holding off `_model_lock` acquisition"
            )
            worker.clear_live_pending()
            assert done.wait(timeout=2.0), (
                "apply_fn not invoked after gate released"
            )
            assert len(transcribe_calls) == 1
            # By the time transcribe_fn runs, the gate must have released,
            # so should_abort at entry reflects `_live_pending` cleared.
            assert transcribe_calls[0] is False
        finally:
            worker.stop(timeout=1.0)

    def test_post_lock_recheck_aborts_when_live_signals_during_race(self):
        """Regression for the TOCTOU race between `_wait_for_live_released()`
        and `_model_lock` acquisition: even when the pre-lock gate has
        released (because `_live_pending` was clear at the last poll), live
        can still call `set_live_pending()` before the worker grabs the
        lock. Without a post-lock re-check, the worker would run the
        encoder and only abort in the decoder — paying ~187 ms of MPS
        cost that live sits behind. The fix re-reads `_live_pending` once
        the lock is held; if set, it releases and requeues without calling
        `transcribe_fn`.
        """
        transcribe_calls: List[bool] = []

        def transcribe_fn(pcm, should_abort=None):
            transcribe_calls.append(True)
            return _FakeResult("", [])

        def apply_fn(_result: RefineResult) -> None:
            pass

        model_lock = threading.Lock()
        worker = RefineWorker(
            model_lock=model_lock,
            transcribe_fn=transcribe_fn,
            apply_fn=apply_fn,
            abort_mode="cooperative",
        )

        # Hold the lock externally so the worker blocks inside the
        # `with self._model_lock:` guard after `_wait_for_live_released`
        # returns — this opens the race window deterministically.
        model_lock.acquire()
        worker.start()
        worker.submit(_make_task("p1"))

        try:
            # Let the worker pull the task, pass the (empty) gate, and
            # block on the external lock.
            time.sleep(0.4)
            # Simulate live signalling in the gap between gate-release
            # and lock-acquire.
            worker.set_live_pending()
            # Release the lock — worker now acquires it, re-checks the
            # flag, and must abort without calling transcribe_fn.
            model_lock.release()

            time.sleep(0.4)
            assert transcribe_calls == [], (
                "transcribe_fn ran even though _live_pending was set "
                "between gate-release and lock-acquire — the post-lock "
                "re-check did not fire"
            )

            # Clear the flag so the requeued task can complete on retry.
            worker.clear_live_pending()
            deadline = time.time() + 2.0
            while not transcribe_calls and time.time() < deadline:
                time.sleep(0.05)
            assert len(transcribe_calls) == 1, (
                "retry after requeue never ran transcribe_fn"
            )
        finally:
            worker.stop(timeout=1.0)

    def test_coarse_mode_ignores_live_pending(self):
        seen_kwargs: List[dict] = []
        done = threading.Event()

        def transcribe_fn(pcm, **kwargs):
            seen_kwargs.append(kwargs)
            return _FakeResult("coarse-ok", [])

        def apply_fn(_result: RefineResult) -> None:
            done.set()

        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=transcribe_fn,
            apply_fn=apply_fn,
            abort_mode="coarse",
        )
        # Live-priority signal must NOT influence coarse mode.
        worker.set_live_pending()
        worker.start()
        try:
            worker.submit(_make_task("p1"))
            assert done.wait(timeout=2.0), "apply_fn was not invoked"
        finally:
            worker.stop(timeout=1.0)

        assert len(seen_kwargs) == 1
        assert "should_abort" not in seen_kwargs[0]

    def test_stop_joins_cleanly(self):
        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=lambda _pcm, **_kw: _FakeResult("", []),
            apply_fn=lambda _result: None,
        )
        worker.start()
        started = time.time()
        worker.stop(timeout=1.0)
        elapsed = time.time() - started
        assert elapsed < 1.0, f"stop() took {elapsed:.3f}s (>= 1.0s timeout)"
        assert worker._thread is None
