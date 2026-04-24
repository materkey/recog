"""Fallback_mixed integration — refine pauses on PCM watchdog trip.

Exercises the wiring added for §9 of `per-speaker-stream-refine`:
`LiveTranscriber._emit_pcm_transition_marker` must call
`RefineWorker.pause()` on the degraded-trip (True) and
`RefineWorker.resume()` on recovery (False). While paused, tasks
submitted through the worker's public `submit` API (which is the path
the ingest's tick-loop uses via `refine_submit_callback`) are dropped
silently; after resume, submissions are processed again.

Test builds `LiveTranscriber` via `__new__` to sidestep the real
constructor's audio-device + GigaAM model setup — only the refine
bookkeeping fields and storage handle matter here.
"""
from __future__ import annotations

import json
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import List
from unittest.mock import Mock

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from gigaam.types import Word

from meeting_storage import MeetingStorage
from recog.live import LiveTranscriber
from recog.refine import RefineTask, RefineWorker


SAMPLE_RATE = 16000


@dataclass
class _StubTranscriptionResult:
    text: str
    words: List[Word]


def _make_task(pid: str = "pid-alice") -> RefineTask:
    return RefineTask(
        pid=pid,
        pcm_samples=np.zeros(SAMPLE_RATE, dtype=np.int16),
        offset_base_ms=0.0,
        submitted_at=time.time(),
    )


def _make_transcriber_with_refine(
    *,
    storage: MeetingStorage,
    asr_transcribe_with_timestamps,
    apply_hook=None,
) -> tuple[LiveTranscriber, RefineWorker]:
    """Build a `LiveTranscriber` + attached `RefineWorker`, skipping the
    real constructor. Returns `(transcriber, refine_worker)` with the
    worker already started; caller must `worker.stop()` on teardown.
    """
    t = LiveTranscriber.__new__(LiveTranscriber)
    t.sample_rate = SAMPLE_RATE
    t._state_lock = threading.Lock()
    t._meeting_generation = 0
    t._pcm_live_segments = {}
    t._pcm_segment_texts = {}
    t._ktalk_bridge = None
    t._per_speaker_ingest = None
    t._meeting_storage = storage
    t.transcript_lines = []

    asr_stub = Mock()
    asr_stub._model_lock = threading.RLock()
    asr_stub.transcribe_with_timestamps.side_effect = (
        lambda pcm, **_kw: asr_transcribe_with_timestamps(pcm)
    )
    t.asr = asr_stub

    original_apply = t._apply_refine

    def tracking_apply(result):
        original_apply(result)
        if apply_hook is not None:
            apply_hook(result)

    worker = RefineWorker(
        model_lock=t.asr._model_lock,
        transcribe_fn=t._refine_transcribe_fn,
        apply_fn=tracking_apply,
        abort_mode="coarse",
    )
    t._refine_worker = worker
    worker.start()
    return t, worker


class TestPauseResumeWiring:
    def test_degraded_true_pauses_worker(self, tmp_path):
        """_emit_pcm_transition_marker(degraded=True) must pause the worker
        so new submissions are silently dropped (no transcribe_fn call)."""
        storage = MeetingStorage(base_path=tmp_path)
        storage.start_session(mic=False, system_audio=True, ktalk=True)

        transcribe_calls: List[int] = []

        def stub(pcm):
            transcribe_calls.append(len(pcm))
            return _StubTranscriptionResult(text="", words=[])

        t, worker = _make_transcriber_with_refine(
            storage=storage,
            asr_transcribe_with_timestamps=stub,
        )

        try:
            # Trip the watchdog: pause must take effect before any new
            # submissions are considered.
            t._emit_pcm_transition_marker(degraded=True, marker_gen=0)

            # Sanity: submit a task — it should be dropped, not processed.
            worker.submit(_make_task())
            time.sleep(0.4)  # > _POLL_S, gives the worker room to run
            assert transcribe_calls == [], (
                "worker processed a task while paused by watchdog trip"
            )
        finally:
            worker.stop(timeout=1.0)

    def test_degraded_false_resumes_worker(self, tmp_path):
        """After a pause→resume cycle driven by watchdog transitions, new
        submissions must flow through transcribe_fn → apply_fn again."""
        storage = MeetingStorage(base_path=tmp_path)
        storage.start_session(mic=False, system_audio=True, ktalk=True)

        done = threading.Event()

        def stub(pcm):
            return _StubTranscriptionResult(text="ok", words=[])

        def apply_hook(_result):
            done.set()

        t, worker = _make_transcriber_with_refine(
            storage=storage,
            asr_transcribe_with_timestamps=stub,
            apply_hook=apply_hook,
        )

        try:
            t._emit_pcm_transition_marker(degraded=True, marker_gen=0)
            # Paused — this submission is dropped.
            worker.submit(_make_task())
            time.sleep(0.3)
            assert not done.is_set()

            # Recovery — flips the worker back to accepting.
            t._emit_pcm_transition_marker(degraded=False, marker_gen=0)
            worker.submit(_make_task())
            assert done.wait(timeout=2.0), (
                "apply_fn not invoked after watchdog recovery"
            )
        finally:
            worker.stop(timeout=1.0)

    def test_refine_worker_none_is_safe(self, tmp_path):
        """When refine is disabled, `_refine_worker` stays None and the
        transition marker must not raise — the pause/resume calls are
        guarded by an is-not-None check."""
        storage = MeetingStorage(base_path=tmp_path)
        storage.start_session(mic=False, system_audio=True, ktalk=True)

        t = LiveTranscriber.__new__(LiveTranscriber)
        t.sample_rate = SAMPLE_RATE
        t._state_lock = threading.Lock()
        t._meeting_generation = 0
        t._pcm_live_segments = {}
        t._pcm_segment_texts = {}
        t._ktalk_bridge = None
        t._per_speaker_ingest = None
        t._meeting_storage = storage
        t.transcript_lines = []
        t._refine_worker = None
        t.asr = Mock()
        t.asr._model_lock = threading.RLock()

        # Should not raise either way.
        t._emit_pcm_transition_marker(degraded=True, marker_gen=0)
        t._emit_pcm_transition_marker(degraded=False, marker_gen=0)


class TestFallbackDropsRefineRewrites:
    def test_degraded_path_does_not_rewrite_transcript_jsonl(self, tmp_path):
        """End-to-end: while the watchdog holds the worker paused, any
        refine submission is a no-op and the live segment's transcript.jsonl
        row keeps its original text (no refined=true flag)."""
        storage = MeetingStorage(base_path=tmp_path)
        storage.start_session(mic=False, system_audio=True, ktalk=True)

        # Seed a live segment so a hypothetical refine could target it.
        seg_id = storage.append_segment(
            speaker="Alice",
            speaker_id="pid-alice",
            text="прив",
            is_local=False,
            is_continuation=False,
            uncertain=False,
            uncertain_reason="",
            audio_offset_ms=0,
        )

        transcribe_calls: List[int] = []

        def stub(pcm):
            transcribe_calls.append(len(pcm))
            return _StubTranscriptionResult(
                text="привет",
                words=[Word(text="привет", start=500, end=2500)],
            )

        t, worker = _make_transcriber_with_refine(
            storage=storage,
            asr_transcribe_with_timestamps=stub,
        )
        t._pcm_live_segments = {"pid-alice": [(seg_id, 0.0, 3000.0)]}
        t._pcm_segment_texts = {seg_id: "прив"}

        try:
            t._emit_pcm_transition_marker(degraded=True, marker_gen=0)

            worker.submit(_make_task())
            time.sleep(0.4)
            assert transcribe_calls == []

            # JSONL row still has the original live text; no refined flag.
            transcript_path = storage.current_meeting_path / "transcript.jsonl"
            rows = [
                json.loads(line)
                for line in transcript_path.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            by_id = {r["id"]: r for r in rows}
            # append_segment writes the seed row; update_segments with text=
            # would add refined=true. Paused worker means no such call.
            assert by_id[seg_id]["text"] == "прив"
            assert by_id[seg_id].get("refined", False) is False
        finally:
            worker.stop(timeout=1.0)
