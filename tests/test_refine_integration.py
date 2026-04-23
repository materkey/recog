"""Integration smoke test for the full refine contour
(docs/plans/2026-04-24-per-speaker-refine-backend.md Task 11.5).

Exercises:
    `PerSpeakerIngest.enqueue` (PCM chunk ingest)
        → silence idle → `get_refine_candidate` gap trigger
        → `RefineWorker.submit` via ingest's tick loop
        → `RefineWorker._process_task` with stubbed `transcribe_with_timestamps`
        → `LiveTranscriber._apply_refine` (real method, not a mock)
        → `MeetingStorage.update_segments(text=..., refined=True)`.

The `asr` module is fully stubbed — we do not load a real GigaAM model in CI;
the focus here is the wiring, not the inference. We build `LiveTranscriber`
via `__new__` to bypass constructor side-effects (no audio devices, no ASR
load), and populate only the fields the exercised code paths read.
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

from ktalk_bridge import PcmChunk
from meeting_storage import MeetingStorage
from recog.config import PCM_REFINE_WINDOW_S
from recog.live import LiveTranscriber
from recog.per_speaker import PerSpeakerIngest
from recog.refine import RefineWorker


SAMPLE_RATE = 16000


@dataclass
class _StubTranscriptionResult:
    text: str
    words: List[Word]


def _make_transcriber(
    *,
    asr_transcribe_with_timestamps,
    sample_rate: int = SAMPLE_RATE,
) -> LiveTranscriber:
    """Build a `LiveTranscriber` via `__new__` with only the state the
    refine contour reads. Avoids the real constructor's audio-device +
    ASR-model setup, which is not viable in a unit-test environment.
    """
    t = LiveTranscriber.__new__(LiveTranscriber)
    t.sample_rate = sample_rate
    t._state_lock = threading.Lock()
    t._meeting_generation = 0
    t._pcm_live_segments = {}
    t._pcm_segment_texts = {}
    t._refine_worker = None
    # Minimal ASR stub — only `transcribe_with_timestamps` and `_model_lock`
    # are consulted by the refine path.
    asr_stub = Mock()
    asr_stub._model_lock = threading.RLock()
    asr_stub.transcribe_with_timestamps.side_effect = (
        lambda pcm, **_kw: asr_transcribe_with_timestamps(pcm)
    )
    t.asr = asr_stub
    # Required so `_apply_refine`'s prune math doesn't crash on missing attrs.
    t._per_speaker_ingest = None
    return t


def _make_chunk(
    *,
    pid: str,
    seq: int,
    sample_rate: int = SAMPLE_RATE,
    pcm: bytes = b"",
    speaker_name: str = "Alice",
) -> PcmChunk:
    return PcmChunk(
        pid=pid,
        speaker_name=speaker_name,
        is_local=False,
        seq=seq,
        chunk_start_ts=1000.0 + seq,
        chunk_end_ts=1001.0 + seq,
        sample_rate=sample_rate,
        pcm=pcm,
    )


def test_refine_integration_rewrites_jsonl_with_refined_flag(
    tmp_path, monkeypatch
):
    """
    given: `RECOG_PCM_TRANSPORT=1` + `RECOG_PCM_REFINE=1`, a real
           `MeetingStorage` pointed at `tmp_path`, two seeded live segments
           whose pid-frame bounds cover the refine window
    when:  synthetic PCM is enqueued, the ingest's tick loop fires the
           gap-trigger, the `RefineWorker` runs a stubbed
           `transcribe_with_timestamps` returning refined words, and
           `_apply_refine` is invoked on the worker's thread
    then:  `transcript.jsonl` is rewritten in-place with the refined text
           and `refined: true` for the matching segment_id(s); other
           fields (speaker, speaker_id, uncertain) are preserved
    """
    monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
    monkeypatch.setenv("RECOG_PCM_REFINE", "1")

    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(mic=False, system_audio=True, ktalk=True)
    # Seed two live segments that map into the pid frame — segment 1 covers
    # pid_audio_ms [0, 3000], segment 2 covers [3000, 6000]. After refine
    # fires on a 6 s trailing window (offset_base_ms=0), both get rewritten.
    seg_id_1 = storage.append_segment(
        speaker="Alice",
        speaker_id="pid-alice",
        text="прив",
        is_local=False,
        is_continuation=False,
        uncertain=False,
        uncertain_reason="",
        audio_offset_ms=0,
    )
    seg_id_2 = storage.append_segment(
        speaker="Alice",
        speaker_id="pid-alice",
        text="как дела",
        is_local=False,
        is_continuation=True,
        uncertain=False,
        uncertain_reason="",
        audio_offset_ms=3000,
    )

    # Stubbed ASR: returns two refined words spanning the 6 s window.
    # Word ms are relative to the window's start (offset_base_ms=0 so they
    # stay put after rebase). Segment 1 contains word[0]; segment 2 contains
    # word[1].
    refined_words = [
        Word(text="привет", start=500, end=2500),
        Word(text="как-дела", start=3500, end=5500),
    ]

    def stubbed_transcribe(pcm_samples):
        # Assert PCM reached the ASR — guards against a wiring regression
        # where the worker fires but with an empty payload.
        assert isinstance(pcm_samples, np.ndarray)
        assert pcm_samples.size > 0
        return _StubTranscriptionResult(
            text="привет как-дела",
            words=list(refined_words),
        )

    transcriber = _make_transcriber(
        asr_transcribe_with_timestamps=stubbed_transcribe,
    )
    transcriber._meeting_storage = storage
    # Pre-populate live-segment registry so `_apply_refine` can map refined
    # words back to seg_id_1 / seg_id_2 without running the full PCM batch
    # path (that needs real ASR).
    transcriber._pcm_live_segments = {
        "pid-alice": [
            (seg_id_1, 0.0, 3000.0),
            (seg_id_2, 3000.0, 6000.0),
        ],
    }
    transcriber._pcm_segment_texts = {seg_id_1: "прив", seg_id_2: "как дела"}

    # Done-event wrapping the real _apply_refine so the test can wait for
    # the worker thread to finish its async write.
    done = threading.Event()
    original_apply = transcriber._apply_refine

    def tracking_apply(result):
        original_apply(result)
        done.set()

    refine_worker = RefineWorker(
        model_lock=transcriber.asr._model_lock,
        transcribe_fn=transcriber._refine_transcribe_fn,
        apply_fn=tracking_apply,
        abort_mode="coarse",  # skip decoder monkey-patch — we stub the ASR
    )
    transcriber._refine_worker = refine_worker
    refine_worker.start()

    # Ingest uses the worker's submit as the refine callback.
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda _b: None,  # live path not exercised here
        generation_getter=lambda: transcriber._meeting_generation,
        meeting_active_getter=lambda: True,
        refine_submit_callback=refine_worker.submit,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )

    try:
        # Feed 6 one-second chunks for pid-alice so the ring covers the
        # full 6 s window refined above.
        pcm_one_second = np.zeros(SAMPLE_RATE, dtype="<i2").tobytes()
        for s in range(1, 7):
            ingest.enqueue(_make_chunk(
                pid="pid-alice",
                seq=s,
                pcm=pcm_one_second,
            ))

        # Drive the ingest's tick loop until silence exceeds SPEAKER_TURN_GAP
        # and the gap trigger fires. Each tick advances silence by
        # PCM_BATCH_INTERVAL_MS — SPEAKER_TURN_GAP=3 s so 18 ticks clears it
        # (+ some slack for the first tick that consumes touched pids).
        for _ in range(20):
            ingest.tick()

        assert done.wait(timeout=2.0), "apply_fn was not invoked within 2 s"
    finally:
        refine_worker.stop(timeout=2.0)
        ingest.stop()

    # Read JSONL back and verify the refine rewrote both segments.
    transcript_path = storage.current_meeting_path / "transcript.jsonl"
    rows = [
        json.loads(line)
        for line in transcript_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    by_id = {r["id"]: r for r in rows}

    assert by_id[seg_id_1]["text"] == "привет"
    assert by_id[seg_id_1]["refined"] is True
    assert by_id[seg_id_1]["speaker"] == "Alice"
    assert by_id[seg_id_1]["speaker_id"] == "pid-alice"

    assert by_id[seg_id_2]["text"] == "как-дела"
    assert by_id[seg_id_2]["refined"] is True
    assert by_id[seg_id_2]["speaker"] == "Alice"


def test_refine_disabled_setup_is_noop(tmp_path, monkeypatch):
    """
    given: `RECOG_PCM_REFINE=0` (transport enabled, refine gate off)
    when:  `_setup_refine_worker` runs after `_setup_per_speaker_ingest`
    then:  `_refine_worker` stays None and the ingest's refine callback
           stays unset — no worker thread, no queue, no overhead
    """
    monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
    monkeypatch.setenv("RECOG_PCM_REFINE", "0")

    transcriber = _make_transcriber(
        asr_transcribe_with_timestamps=lambda pcm: _StubTranscriptionResult(
            "", []
        ),
    )
    # Minimal ingest — `_setup_refine_worker` reads it to decide whether
    # to wire the callback.
    transcriber._per_speaker_ingest = PerSpeakerIngest(
        transcribe_callback=lambda _b: None,
        generation_getter=lambda: 0,
        meeting_active_getter=lambda: False,
    )
    try:
        transcriber._setup_refine_worker()

        assert transcriber._refine_worker is None
        assert transcriber._per_speaker_ingest._refine_submit_callback is None
    finally:
        transcriber._per_speaker_ingest.stop()
