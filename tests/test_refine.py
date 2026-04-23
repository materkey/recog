"""Unit tests for recog/refine.py — scaffold (Task 3).

The worker loop is implemented in Task 4; these tests verify the module's
public shape only: dataclasses construct cleanly, the worker validates
its constructor args, and the stub methods raise as promised.
"""
from __future__ import annotations

import sys
import threading
import time
from pathlib import Path

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from gigaam.types import Word

from recog.refine import RefineResult, RefineTask, RefineWorker


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
        task = RefineTask(
            pid="p1",
            pcm_samples=np.zeros(16, dtype=np.int16),
            offset_base_ms=0.0,
            submitted_at=0.0,
        )
        with pytest.raises(Exception):
            task.pid = "p2"  # type: ignore[misc]


class TestRefineResult:
    def test_constructs_with_task_and_words(self):
        task = RefineTask(
            pid="p1",
            pcm_samples=np.zeros(16, dtype=np.int16),
            offset_base_ms=0.0,
            submitted_at=0.0,
        )
        words = [Word(text="hi", start=0, end=500), Word(text="world", start=500, end=1200)]
        result = RefineResult(task=task, text="hi world", words=words)
        assert result.task is task
        assert result.text == "hi world"
        assert len(result.words) == 2
        assert result.words[0].text == "hi"

    def test_is_immutable(self):
        task = RefineTask(
            pid="p1",
            pcm_samples=np.zeros(16, dtype=np.int16),
            offset_base_ms=0.0,
            submitted_at=0.0,
        )
        result = RefineResult(task=task, text="x", words=[])
        with pytest.raises(Exception):
            result.text = "y"  # type: ignore[misc]


class TestRefineWorkerScaffold:
    def _make_worker(self, abort_mode="cooperative") -> RefineWorker:
        return RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=lambda _pcm: None,  # type: ignore[return-value]
            apply_fn=lambda _result: None,
            abort_mode=abort_mode,
        )

    def test_constructs_with_default_abort_mode_cooperative(self):
        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=lambda _pcm: None,  # type: ignore[return-value]
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
                transcribe_fn=lambda _pcm: None,  # type: ignore[return-value]
                apply_fn=lambda _result: None,
                abort_mode="bogus",  # type: ignore[arg-type]
            )

    def test_submit_stub_raises(self):
        worker = self._make_worker()
        task = RefineTask(
            pid="p1",
            pcm_samples=np.zeros(16, dtype=np.int16),
            offset_base_ms=0.0,
            submitted_at=0.0,
        )
        with pytest.raises(NotImplementedError):
            worker.submit(task)

    def test_pause_resume_stop_stubs_raise(self):
        worker = self._make_worker()
        with pytest.raises(NotImplementedError):
            worker.pause()
        with pytest.raises(NotImplementedError):
            worker.resume()
        with pytest.raises(NotImplementedError):
            worker.stop()
