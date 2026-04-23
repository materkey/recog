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

    def test_live_pending_aborts_cooperative(self):
        calls: List[bool] = []
        done = threading.Event()
        worker_ref: List[RefineWorker] = []

        def transcribe_fn(pcm, should_abort=None):
            assert should_abort is not None, (
                "cooperative mode must pass should_abort"
            )
            pending = should_abort()
            calls.append(pending)
            if len(calls) == 1:
                # First call: live path has priority — clear the flag so the
                # retry can run, then bail as the decoder monkey-patch would.
                worker_ref[0].clear_live_pending()
                raise RefineAborted("first-call abort")
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

        worker.set_live_pending()
        worker.start()
        try:
            worker.submit(_make_task("p1"))
            assert done.wait(timeout=2.0), (
                "apply_fn was not called after re-enqueue"
            )
        finally:
            worker.stop(timeout=1.0)

        assert len(calls) == 2
        assert calls[0] is True  # first call saw live_pending set
        assert calls[1] is False  # retry saw it cleared

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
