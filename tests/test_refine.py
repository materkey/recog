"""Unit tests for recog/refine.py — scaffold + basic construction.

Task 3 scaffolded `RefineTask`/`RefineResult` and the worker shell; Task 4
fleshed out the worker (submit/pause/resume/stop + cooperative abort). The
lifecycle + abort-coverage tests are added in Task 5; this file covers:

* dataclass construction and immutability,
* constructor validation of `abort_mode`.
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

from recog.refine import RefineResult, RefineTask, RefineWorker


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
