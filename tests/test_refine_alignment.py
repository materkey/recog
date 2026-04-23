"""Unit tests for `LiveTranscriber._apply_refine` (refine-backend Task 9).

`_apply_refine` rebases upstream `Word` timestamps from the refine window's
local frame into the per-pid audio frame (via `task.offset_base_ms`), then
intersects them with `_pcm_live_segments[pid]` to build per-segment update
dicts. Tests cover:

* word rebase into the pid frame (`test_word_rebase_into_pid_frame`);
* empty collection — no intersection — skips the segment entirely
  (`test_empty_collection_skips_segment`);
* identical cached text — no-op to avoid UI flicker / write amplification
  (`test_identical_text_skips_update`);
* multi-segment subset produces a batched `updates` list
  (`test_segment_subset_produces_updates_list`);
* generation drift — meeting switched between dispatch and apply — drops the
  result entirely (`test_generation_mismatch_drops_result`).

We stub out `MeetingStorage` with a Mock so assertions focus on the
`update_segments` payload, not on JSONL-rewrite plumbing (Task 10).
"""
from __future__ import annotations

import sys
import threading
from pathlib import Path
from unittest.mock import Mock

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from gigaam.types import Word

from recog.live import LiveTranscriber
from recog.refine import RefineResult, RefineTask


def _make_task(
    *,
    pid: str = "pid-1",
    offset_base_ms: float = 0.0,
    meeting_generation: int = 0,
) -> RefineTask:
    return RefineTask(
        pid=pid,
        pcm_samples=np.zeros(16, dtype=np.int16),
        offset_base_ms=offset_base_ms,
        submitted_at=0.0,
        meeting_generation=meeting_generation,
    )


def _make_transcriber(
    *,
    meeting_generation: int = 0,
    live_segments: dict[str, list[tuple[int, float, float]]] | None = None,
    segment_texts: dict[int, str] | None = None,
    meeting_path: Path | None = Path("/tmp/fake-meeting"),
) -> Mock:
    """Build a `Mock(spec=LiveTranscriber)` carrying the minimal state the
    method reads. A real `threading.Lock` is used so the `with` statement in
    `_apply_refine` works; the meeting storage is a `Mock` to capture calls.
    """
    t = Mock(spec=LiveTranscriber)
    t._state_lock = threading.Lock()
    t._meeting_generation = meeting_generation
    t._pcm_live_segments = live_segments or {}
    t._pcm_segment_texts = segment_texts or {}
    t._meeting_storage = Mock()
    t._meeting_storage.current_meeting_path = meeting_path
    return t


class TestWordRebaseIntoPidFrame:
    """given refined words in window-local ms - when _apply_refine runs - then word timestamps are rebased by offset_base_ms."""

    def test_rebase_and_intersection_yields_refined_text(self):
        pid = "pid-1"
        # Live segment spans 5000-6000 ms in the pid frame.
        t = _make_transcriber(
            live_segments={pid: [(1, 5_000.0, 6_000.0)]},
            segment_texts={1: "stale text"},
        )
        # Refine window starts at 5000 ms; words at 100/500 and 500/900 rebase
        # into 5100/5500 and 5500/5900 ms, both inside seg 1.
        task = _make_task(pid=pid, offset_base_ms=5_000.0)
        result = RefineResult(
            task=task,
            text="hello world",
            words=[
                Word(text="hello", start=100, end=500),
                Word(text="world", start=500, end=900),
            ],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_called_once()
        args, kwargs = t._meeting_storage.update_segments.call_args
        updates = args[0]
        assert updates == [
            {"segment_id": 1, "text": "hello world", "refined": True},
        ]
        assert kwargs["meeting_path"] == Path("/tmp/fake-meeting")
        # Cache reflects the refined text for the next refine cycle.
        assert t._pcm_segment_texts[1] == "hello world"


class TestEmptyCollectionSkipsSegment:
    """given a refine window that does not overlap any live segment - when _apply_refine runs - then no update is emitted."""

    def test_segment_outside_window_is_skipped(self):
        pid = "pid-1"
        t = _make_transcriber(
            live_segments={pid: [(1, 10_000.0, 11_000.0)]},
            segment_texts={1: "old"},
        )
        task = _make_task(pid=pid, offset_base_ms=0.0)
        result = RefineResult(
            task=task,
            text="a",
            words=[Word(text="a", start=100, end=500)],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_not_called()
        assert t._pcm_segment_texts[1] == "old"

    def test_segment_inside_window_but_no_words_is_skipped(self):
        pid = "pid-1"
        t = _make_transcriber(
            live_segments={pid: [(1, 0.0, 1_000.0)]},
            segment_texts={1: "old"},
        )
        task = _make_task(pid=pid, offset_base_ms=0.0)
        # Words at 1500-2000 don't fall inside [0, 1000].
        result = RefineResult(
            task=task,
            text="noop",
            words=[Word(text="noop", start=1500, end=2000)],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_not_called()

    def test_no_entries_for_pid_skips(self):
        t = _make_transcriber(live_segments={"other-pid": [(1, 0.0, 1_000.0)]})
        task = _make_task(pid="pid-1", offset_base_ms=0.0)
        result = RefineResult(
            task=task,
            text="a",
            words=[Word(text="a", start=100, end=500)],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_not_called()


class TestIdenticalTextSkipsUpdate:
    """given refined text equals cached live text - when _apply_refine runs - then no update is emitted (D8)."""

    def test_byte_identical_refinement_is_a_noop(self):
        pid = "pid-1"
        t = _make_transcriber(
            live_segments={pid: [(1, 0.0, 1_000.0)]},
            segment_texts={1: "hello world"},
        )
        task = _make_task(pid=pid, offset_base_ms=0.0)
        result = RefineResult(
            task=task,
            text="hello world",
            words=[
                Word(text="hello", start=100, end=500),
                Word(text="world", start=500, end=900),
            ],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_not_called()
        # Cache unchanged.
        assert t._pcm_segment_texts[1] == "hello world"

    def test_different_refinement_does_emit(self):
        pid = "pid-1"
        t = _make_transcriber(
            live_segments={pid: [(1, 0.0, 1_000.0)]},
            segment_texts={1: "hello"},
        )
        task = _make_task(pid=pid, offset_base_ms=0.0)
        result = RefineResult(
            task=task,
            text="hello world",
            words=[
                Word(text="hello", start=100, end=500),
                Word(text="world", start=500, end=900),
            ],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_called_once()


class TestSegmentSubsetProducesUpdatesList:
    """given a refine window spanning multiple live segments - when _apply_refine runs - then each intersected segment yields one update entry."""

    def test_multiple_segments_partitioned_by_word_range(self):
        pid = "pid-1"
        # Two live segments in the pid frame: [0, 1000] and [2000, 3000].
        t = _make_transcriber(
            live_segments={pid: [(1, 0.0, 1_000.0), (2, 2_000.0, 3_000.0)]},
            segment_texts={1: "stale-1", 2: "stale-2"},
        )
        task = _make_task(pid=pid, offset_base_ms=0.0)
        result = RefineResult(
            task=task,
            text="a b c",
            words=[
                Word(text="a", start=100, end=400),     # inside seg 1
                Word(text="b", start=400, end=900),     # inside seg 1
                Word(text="c", start=2100, end=2500),   # inside seg 2
            ],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_called_once()
        args, _ = t._meeting_storage.update_segments.call_args
        updates = args[0]
        # Both segments updated; order follows registry iteration (seg 1 first).
        assert updates == [
            {"segment_id": 1, "text": "a b", "refined": True},
            {"segment_id": 2, "text": "c", "refined": True},
        ]
        assert t._pcm_segment_texts[1] == "a b"
        assert t._pcm_segment_texts[2] == "c"

    def test_only_changed_segments_included(self):
        """If one segment's refined text matches cache (no change) but
        another's differs, only the differing one is in the updates list."""
        pid = "pid-1"
        t = _make_transcriber(
            live_segments={pid: [(1, 0.0, 1_000.0), (2, 2_000.0, 3_000.0)]},
            segment_texts={1: "a b", 2: "stale-2"},
        )
        task = _make_task(pid=pid, offset_base_ms=0.0)
        result = RefineResult(
            task=task,
            text="a b c",
            words=[
                Word(text="a", start=100, end=400),
                Word(text="b", start=400, end=900),
                Word(text="c", start=2100, end=2500),
            ],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_called_once()
        args, _ = t._meeting_storage.update_segments.call_args
        assert args[0] == [
            {"segment_id": 2, "text": "c", "refined": True},
        ]


class TestGenerationMismatchDropsResult:
    """given a refine task submitted in a past meeting - when _apply_refine runs after a meeting switch - then the whole result is dropped."""

    def test_different_generation_skips_storage_call(self):
        pid = "pid-1"
        t = _make_transcriber(
            meeting_generation=10,
            live_segments={pid: [(1, 0.0, 1_000.0)]},
            segment_texts={1: "stale"},
        )
        # Task captured generation=5, then live bumped to 10 before apply.
        task = _make_task(pid=pid, offset_base_ms=0.0, meeting_generation=5)
        result = RefineResult(
            task=task,
            text="hello world",
            words=[
                Word(text="hello", start=100, end=500),
                Word(text="world", start=500, end=900),
            ],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_not_called()
        # Cache untouched — a post-switch segment_id collision with the old
        # meeting would otherwise poison the cache for the new meeting.
        assert t._pcm_segment_texts == {1: "stale"}

    def test_matching_generation_does_apply(self):
        pid = "pid-1"
        t = _make_transcriber(
            meeting_generation=7,
            live_segments={pid: [(1, 0.0, 1_000.0)]},
            segment_texts={1: "stale"},
        )
        task = _make_task(pid=pid, offset_base_ms=0.0, meeting_generation=7)
        result = RefineResult(
            task=task,
            text="fresh",
            words=[Word(text="fresh", start=100, end=500)],
        )
        LiveTranscriber._apply_refine(t, result)

        t._meeting_storage.update_segments.assert_called_once()

    def test_no_meeting_storage_is_a_noop(self):
        """Defensive: `_apply_refine` must not crash if storage is absent."""
        pid = "pid-1"
        t = _make_transcriber(live_segments={pid: [(1, 0.0, 1_000.0)]})
        t._meeting_storage = None
        task = _make_task(pid=pid, offset_base_ms=0.0)
        result = RefineResult(
            task=task,
            text="hello",
            words=[Word(text="hello", start=100, end=500)],
        )
        # Must return cleanly — no AttributeError on the None storage.
        LiveTranscriber._apply_refine(t, result)


class TestApplyFnCompatibility:
    """given LiveTranscriber._apply_refine signature - when passed as RefineWorker's apply_fn - then the worker constructs without type errors (Task 9.2)."""

    def test_bound_method_passes_as_apply_fn(self):
        from recog.refine import RefineWorker

        t = _make_transcriber()
        # Simulate what Task 11 will wire — passing the bound method as
        # apply_fn. Construction must succeed without raising.
        worker = RefineWorker(
            model_lock=threading.Lock(),
            transcribe_fn=lambda _pcm, **_kw: Mock(text="", words=[]),
            apply_fn=lambda result: LiveTranscriber._apply_refine(t, result),
        )
        assert worker.abort_mode == "cooperative"
