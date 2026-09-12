"""End-to-end test for the per-speaker stream-refine contour.

Plan reference: docs/plans/2026-04-24-per-speaker-refine-rollout.md §2.

Wires the full PCM → live → refine pipeline together with stubbed ASR
(no real GigaAM model is loaded in CI). All other components are real:

  * `LiveTranscriber._on_pcm_batch` — runs synchronously inside the
    ingest's tick, registers live segments, calls `bridge.send_transcript`.
  * `PerSpeakerIngest` — buffers chunks per pid, drains via `tick`, and
    feeds `get_refine_candidate` results to the worker.
  * `RefineWorker` — daemon thread; popping a task triggers the stubbed
    `transcribe_with_timestamps` and then `_apply_refine`.
  * `MeetingStorage` — real instance pointed at `tmp_path`. We wrap
    `update_segments` with a recorder so we can assert the refine
    rewrite path is exercised end-to-end.

A second scenario trips the watchdog mid-stream and asserts the paused
worker silently drops refine submissions — no `update_segments(text=...)`
calls, no `bridge.send_refine` invocations.
"""
from __future__ import annotations

import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import List
from unittest.mock import MagicMock, Mock

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from gigaam.types import Word

from recog.ktalk_bridge import PcmChunk
from recog.meeting_storage import MeetingStorage
from recog.live import LiveTranscriber
from recog.per_speaker import PerSpeakerIngest
from recog.refine import RefineWorker


SAMPLE_RATE = 16000
CHUNK_SECONDS = 3.0
SAMPLES_PER_CHUNK = int(CHUNK_SECONDS * SAMPLE_RATE)


@dataclass
class _StubTranscriptionResult:
    text: str
    words: List[Word]


def _make_chunk(
    *,
    pid: str,
    seq: int,
    pcm: bytes,
    sample_rate: int = SAMPLE_RATE,
    speaker_name: str = "Alice",
) -> PcmChunk:
    return PcmChunk(
        pid=pid,
        speaker_name=speaker_name,
        is_local=False,
        seq=seq,
        chunk_start_ts=1000.0 + seq * CHUNK_SECONDS,
        chunk_end_ts=1000.0 + (seq + 1) * CHUNK_SECONDS,
        sample_rate=sample_rate,
        pcm=pcm,
    )


def _build_pipeline(
    tmp_path: Path,
    *,
    transcribe_batch_fn,
    transcribe_ts_fn,
    apply_hook=None,
):
    """Construct the live + refine pipeline with stubbed ASR.

    Returns a tuple of (transcriber, storage, ingest, refine_worker, bridge,
    update_segments_calls). `update_segments_calls` is a list of the lists
    passed to `MeetingStorage.update_segments` (snapshotted under the
    recorder so subsequent mutations don't affect what we asserted on).
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(mic=False, system_audio=True, ktalk=True)

    update_calls: list[list[dict]] = []
    real_update = storage.update_segments

    def recording_update(updates, meeting_path=None):
        update_calls.append([dict(u) for u in updates])
        return real_update(updates, meeting_path=meeting_path)

    storage.update_segments = recording_update

    bridge = MagicMock()

    asr_stub = Mock()
    asr_stub._model_lock = threading.RLock()
    asr_stub.transcribe_batch.side_effect = transcribe_batch_fn
    asr_stub.transcribe_with_timestamps.side_effect = (
        lambda pcm, **_kw: transcribe_ts_fn(pcm)
    )

    t = LiveTranscriber.__new__(LiveTranscriber)
    t.ktalk_only = False
    t.sample_rate = SAMPLE_RATE
    t._state_lock = threading.Lock()
    t._meeting_generation = 0
    t._pcm_live_segments = {}
    t._pcm_segment_texts = {}
    t._pcm_prev_words = {}
    t._pcm_pid_state = {}
    t._meeting_storage = storage
    t._ktalk_bridge = bridge
    t._wav_lock = threading.Lock()
    t._wav_writer_system = None
    t._wav_writer_mic = None
    t._system_wav_anchor = None
    t._refine_worker = None
    t.transcript_lines = []
    t.asr = asr_stub

    ingest = PerSpeakerIngest(
        transcribe_callback=t._on_pcm_batch,
        generation_getter=lambda: t._meeting_generation,
        meeting_active_getter=lambda: True,
        max_batch_size=8,
        max_chunks_per_pid=1000,
    )
    t._per_speaker_ingest = ingest

    original_apply = t._apply_refine

    def wrapped_apply(result):
        original_apply(result)
        if apply_hook is not None:
            apply_hook(result)

    refine_worker = RefineWorker(
        model_lock=asr_stub._model_lock,
        transcribe_fn=t._refine_transcribe_fn,
        apply_fn=wrapped_apply,
        # Coarse mode skips the decoder monkey-patch — the ASR is a Mock
        # without a real `model.decoding.decode` to swap.
        abort_mode="coarse",
    )
    t._refine_worker = refine_worker
    ingest.set_refine_submit_callback(refine_worker.submit)
    refine_worker.start()

    return t, storage, ingest, refine_worker, bridge, update_calls


def _silence_chunk_bytes() -> bytes:
    return np.zeros(SAMPLES_PER_CHUNK, dtype="<i2").tobytes()


def test_full_refine_contour_emits_refined_updates(tmp_path):
    """
    given: a real `MeetingStorage`, real `PerSpeakerIngest` and
           `RefineWorker`, stubbed ASR — and a synthetic 15-second
           PCM stream for one pid (5 × 3-second
           chunks) followed by a quiescent tick window
    when:  the ingest's tick loop drains the batch (5 live segments
           appended), `get_refine_candidate` fires the monolog trigger
           (continuous_speech_ms ≥ 15000), and the worker re-transcribes
           the trailing 15 s window
    then:  `MeetingStorage.update_segments` is called with `text=...` +
           `refined=True` for at least one segment_id, and
           `bridge.send_refine` is invoked with matching ids
    """
    pid = "pid-alice"

    seq_counter = [0]

    def transcribe_batch_fn(pcm_list):
        out = []
        for _ in pcm_list:
            seq_counter[0] += 1
            out.append(f"chunk{seq_counter[0]}")
        return out

    def transcribe_ts_fn(pcm_samples):
        # Deterministic refined words at 500 ms cadence, spanning every live
        # segment in the 15 s window. Word(start, end) are in milliseconds
        # relative to `pcm_samples[0]`; `_apply_refine` rebases them via
        # `task.offset_base_ms` (which is 0 here — the window covers all
        # drained audio).
        n_words = max(1, pcm_samples.size // (SAMPLE_RATE // 2))
        words = [
            Word(text=f"refined{i}", start=i * 500, end=i * 500 + 400)
            for i in range(n_words)
        ]
        return _StubTranscriptionResult(
            text=" ".join(w.text for w in words), words=words
        )

    apply_event = threading.Event()

    def on_apply(_result):
        apply_event.set()

    t, storage, ingest, worker, bridge, update_calls = _build_pipeline(
        tmp_path,
        transcribe_batch_fn=transcribe_batch_fn,
        transcribe_ts_fn=transcribe_ts_fn,
        apply_hook=on_apply,
    )

    chunk_bytes = _silence_chunk_bytes()
    try:
        for s in range(1, 6):
            ingest.enqueue(_make_chunk(pid=pid, seq=s, pcm=chunk_bytes))

        # One tick: drains all 5 chunks (max_batch_size=8), runs
        # `_on_pcm_batch` which registers 5 live segments (pid-frame bounds
        # [0,3000], [3000,6000], ..., [12000,15000]), then submits a refine
        # task because `_continuous_speech_ms[pid] = 15000` ≥ monolog cap.
        ingest.tick()
        assert len(t._pcm_live_segments.get(pid, [])) == 5, (
            "live path did not register all 5 segments"
        )

        # Wait for the refine worker to pop the task, run the stubbed
        # transcribe, and finish `_apply_refine`.
        assert apply_event.wait(timeout=3.0), (
            "refine apply_fn was not invoked within 3 s"
        )

        text_updates = [
            u
            for snapshot in update_calls
            for u in snapshot
            if "text" in u and u.get("refined") is True
        ]
        assert text_updates, (
            f"no update_segments(text=..., refined=True) calls; "
            f"recorded={update_calls}"
        )

        send_refine_calls = bridge.send_refine.call_args_list
        assert send_refine_calls, "bridge.send_refine was never called"
        broadcast_ids = {
            u["segment_id"]
            for call in send_refine_calls
            for u in call.args[0]
        }
        rewrite_ids = {u["segment_id"] for u in text_updates}
        assert rewrite_ids & broadcast_ids, (
            f"send_refine ids {broadcast_ids} do not overlap with "
            f"update_segments ids {rewrite_ids}"
        )
    finally:
        worker.stop(timeout=2.0)
        ingest.stop()


def test_full_refine_contour_pauses_when_watchdog_degraded(tmp_path):
    """
    given: same end-to-end pipeline, but `_emit_pcm_transition_marker(
           degraded=True)` is invoked before chunks arrive — the watchdog
           transition pauses the refine worker
    when:  PCM chunks are enqueued and the ingest's tick loop drains them
           (live path appends segments and `get_refine_candidate` still
           submits refine tasks)
    then:  `MeetingStorage.update_segments(text=...)` is never called
           (paused worker drops every submission), and `bridge.send_refine`
           stays at zero invocations — the live append_segment path is
           unaffected
    """
    pid = "pid-alice"
    seq_counter = [0]

    def transcribe_batch_fn(pcm_list):
        out = []
        for _ in pcm_list:
            seq_counter[0] += 1
            out.append(f"chunk{seq_counter[0]}")
        return out

    def transcribe_ts_fn(pcm_samples):
        # Should never run — refine is paused for the duration of the test.
        return _StubTranscriptionResult(
            text="refined",
            words=[Word(text="refined", start=0, end=1000)],
        )

    t, storage, ingest, worker, bridge, update_calls = _build_pipeline(
        tmp_path,
        transcribe_batch_fn=transcribe_batch_fn,
        transcribe_ts_fn=transcribe_ts_fn,
    )

    chunk_bytes = _silence_chunk_bytes()
    try:
        # Trip the watchdog → `_refine_worker.pause()` is called via
        # `_emit_pcm_transition_marker`. Subsequent submits are dropped.
        t._emit_pcm_transition_marker(degraded=True, marker_gen=0)

        for s in range(1, 6):
            ingest.enqueue(_make_chunk(pid=pid, seq=s, pcm=chunk_bytes))

        # Drain the batch + give the worker a few poll cycles; if the pause
        # were leaky we'd see at least one refine apply within this window.
        for _ in range(5):
            ingest.tick()
        time.sleep(0.5)

        # Live segments still get appended during degraded mode (the live
        # path runs independently of refine); refine just shouldn't rewrite.
        assert len(t._pcm_live_segments.get(pid, [])) == 5, (
            "live path should still register segments in degraded mode"
        )

        text_updates = [
            u
            for snapshot in update_calls
            for u in snapshot
            if "text" in u
        ]
        assert text_updates == [], (
            f"refine ran while watchdog held worker paused; "
            f"recorded={update_calls}"
        )
        assert bridge.send_refine.call_count == 0, (
            f"send_refine called {bridge.send_refine.call_count} times "
            f"while paused"
        )
    finally:
        worker.stop(timeout=2.0)
        ingest.stop()
