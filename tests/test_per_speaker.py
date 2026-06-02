"""Unit tests for recog/per_speaker.py.

Covers: ring buffer capacity, seq reordering, batch-size trigger, batch-timeout
trigger, stale-generation filtering, drain(), and silence watchdog.
"""
from __future__ import annotations

import sys
import time
from pathlib import Path
from typing import List

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from ktalk_bridge import PcmChunk
from recog.config import PCM_MAX_BATCH_SIZE, PCM_SILENCE_WATCHDOG_S
from recog.per_speaker import BatchedChunk, PerSpeakerIngest


def _make_chunk(
    *,
    pid: str = "p1",
    speaker_name: str = "Alice",
    is_local: bool = False,
    seq: int,
    start_ts: float = 1000.0,
    end_ts: float = 1003.3,
    sample_rate: int = 16000,
    pcm: bytes = b"\x00\x00\x01\x00",
) -> PcmChunk:
    return PcmChunk(
        pid=pid,
        speaker_name=speaker_name,
        is_local=is_local,
        seq=seq,
        chunk_start_ts=start_ts,
        chunk_end_ts=end_ts,
        sample_rate=sample_rate,
        pcm=pcm,
    )


class _Clock:
    """Simple controllable clock for watchdog tests."""
    def __init__(self, start: float = 10_000.0):
        self.now = start

    def __call__(self) -> float:
        return self.now

    def advance(self, delta: float) -> None:
        self.now += delta


def test_ring_buffer_drops_oldest_on_overflow():
    """
    given: PerSpeakerIngest with max_chunks_per_pid=3
    when: enqueuing 5 chunks for the same pid with increasing seq
    then: buffer retains the 3 most-recent-seq chunks (oldest dropped)
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        max_batch_size=100,  # avoid auto-fire on enqueue
        max_chunks_per_pid=3,
    )
    for s in range(1, 6):  # seq 1..5
        ingest.enqueue(_make_chunk(seq=s))

    # Drain manually via tick and inspect what was emitted.
    ingest.tick(now=0.0)
    assert len(received) == 1
    batch = received[0]
    assert [b[0] for b in batch] == ["p1", "p1", "p1"]
    # Ring buffer keeps the last 3 seqs (1 & 2 dropped).  Emission order = seq.
    # Verify by inspecting the 5th tuple field (end_ms) to keep test robust
    # against tuple layout churn: we only care that seq ordering is monotonic.
    # Simpler: reconstruct from callback record via a custom chunk factory.
    # Use a fresh ingest that captures raw seq through start_ts.
    received.clear()
    ingest2 = PerSpeakerIngest(
        transcribe_callback=received.append,
        max_batch_size=100,
        max_chunks_per_pid=3,
    )
    for s in range(1, 6):
        # encode seq in chunk_start_ts so we can recover after drain
        ingest2.enqueue(_make_chunk(seq=s, start_ts=float(s), end_ts=float(s) + 0.1))
    ingest2.tick(now=0.0)
    assert len(received) == 1
    batch = received[0]
    # We expect seq 3, 4, 5 (1 and 2 dropped as oldest).
    recovered_seqs = [int(b[4] / 1000.0) for b in batch]
    assert recovered_seqs == [3, 4, 5]


def test_batch_collector_fires_on_max_batch():
    """
    given: PerSpeakerIngest with max_batch_size=4
    when: 4 chunks are enqueued and tick() is called
    then: one batch of exactly 4 chunks is dispatched to callback
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        max_batch_size=4,
        max_chunks_per_pid=100,
    )
    for s in range(1, 5):
        ingest.enqueue(_make_chunk(seq=s))
    # Still should have 4 buffered; background thread not started, so drain manually.
    ingest.tick(now=0.0)
    assert len(received) == 1
    assert len(received[0]) == 4


def test_batch_collector_fires_on_max_batch_through_wake_event():
    """
    given: PerSpeakerIngest.start() with worker thread running
    when: max_batch_size chunks are enqueued rapidly
    then: the worker thread wakes and dispatches within ~200 ms — covers
          the wake_event optimization that short-circuits the tick timer.
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        batch_interval_ms=2000,  # long timer; we must rely on wake_event
        max_batch_size=3,
        max_chunks_per_pid=100,
    )
    ingest.start()
    try:
        for s in range(1, 4):
            ingest.enqueue(_make_chunk(seq=s))
        deadline = time.time() + 1.0
        while not received and time.time() < deadline:
            time.sleep(0.01)
        assert received, "batch should have fired via wake_event"
        assert len(received[0]) == 3
    finally:
        ingest.stop()


def test_batch_collector_fires_on_timeout():
    """
    given: PerSpeakerIngest.start() with short batch_interval, one chunk buffered
    when: the timer expires without reaching max_batch_size
    then: the single buffered chunk is still dispatched
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        batch_interval_ms=50,
        max_batch_size=100,  # never auto-fires; wait for timer
        max_chunks_per_pid=100,
    )
    ingest.start()
    try:
        ingest.enqueue(_make_chunk(seq=1))
        deadline = time.time() + 1.0
        while not received and time.time() < deadline:
            time.sleep(0.01)
        assert received, "batch should have fired on timeout"
        assert len(received[0]) == 1
    finally:
        ingest.stop()


def test_reordering_by_seq():
    """
    given: single pid, chunks enqueued in out-of-order seq: 3, 1, 2
    when: tick() drains the batch
    then: callback receives chunks in seq order [1, 2, 3]
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    ingest.enqueue(_make_chunk(seq=3, start_ts=3.0, end_ts=3.1))
    ingest.enqueue(_make_chunk(seq=1, start_ts=1.0, end_ts=1.1))
    ingest.enqueue(_make_chunk(seq=2, start_ts=2.0, end_ts=2.1))
    ingest.tick(now=0.0)
    assert len(received) == 1
    batch = received[0]
    # Recover seqs via start_ts encoded in test fixture.
    start_ms_values = [b[4] for b in batch]
    assert start_ms_values == [1000.0, 2000.0, 3000.0]


def test_batch_collector_drops_stale_generation():
    """
    given: chunk enqueued at gen=0, then generation bumped to 1
    when: tick() drains the batch
    then: callback receives empty batch (chunk filtered out)
    """
    received: List[List[BatchedChunk]] = []
    gen = {"value": 0}
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        generation_getter=lambda: gen["value"],
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    ingest.enqueue(_make_chunk(seq=1))
    assert gen["value"] == 0

    gen["value"] = 1
    ingest.tick(now=0.0)

    assert received == []  # no callback invocation when batch is empty


def test_enqueue_drops_stale_generation_before_refine_bookkeeping():
    """
    given: enqueue captured the old generation, then a meeting switch bumped
           generation before enqueue acquired the ingest lock
    when: enqueue resumes
    then: the stale chunk is dropped before it mutates new-meeting refine state
    """
    calls = {"count": 0}

    def generation_getter() -> int:
        calls["count"] += 1
        return 0 if calls["count"] == 1 else 1

    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        generation_getter=generation_getter,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )

    ingest.enqueue(_make_chunk(seq=1, sample_rate=sample_rate, pcm=pcm_one_second))

    assert ingest._buffers == {}
    assert ingest._stats == {}
    assert ingest._pcm_rings == {}
    assert ingest._pid_audio_ms == {}
    assert ingest._silence_ms == {}
    assert ingest._continuous_speech_ms == {}
    assert ingest._pids_had_chunk_since_last_tick == set()
    assert ingest.pcm_ever_seen is False


def test_drain_clears_all_buffers_and_cancels_timer():
    """
    given: 5 chunks enqueued across 2 pids
    when: drain() is called
    then: subsequent tick() dispatches nothing; internal buffers are empty
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    ingest.enqueue(_make_chunk(pid="p1", seq=1))
    ingest.enqueue(_make_chunk(pid="p1", seq=2))
    ingest.enqueue(_make_chunk(pid="p2", seq=1))
    ingest.enqueue(_make_chunk(pid="p2", seq=2))
    ingest.enqueue(_make_chunk(pid="p2", seq=3))

    ingest.drain()

    # Internal state: buffers empty, no pending chunks.
    assert ingest._buffers == {}
    assert ingest._last_chunk_ts is None
    assert ingest._degraded is False

    ingest.tick(now=0.0)
    assert received == []


def test_watchdog_fires_after_silence_threshold():
    """
    given: one chunk enqueued, meeting active, then time advances past
           PCM_SILENCE_WATCHDOG_S without further chunks
    when: tick() runs after the threshold elapsed
    then: ingest.degraded becomes True
    """
    received: List[List[BatchedChunk]] = []
    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        meeting_active_getter=lambda: True,
        silence_watchdog_s=PCM_SILENCE_WATCHDOG_S,
        time_fn=clock,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    ingest.enqueue(_make_chunk(seq=1))
    clock.advance(PCM_SILENCE_WATCHDOG_S + 1.0)
    # Drain any buffered data so tick checks watchdog on empty state.
    ingest.tick(now=clock.now)
    assert ingest.degraded is True


def test_watchdog_does_not_fire_when_meeting_inactive():
    """
    given: meeting_active_getter returns False
    when: silence exceeds threshold
    then: degraded stays False (watchdog does not fire outside meeting)
    """
    received: List[List[BatchedChunk]] = []
    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        meeting_active_getter=lambda: False,
        silence_watchdog_s=PCM_SILENCE_WATCHDOG_S,
        time_fn=clock,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    ingest.enqueue(_make_chunk(seq=1))
    clock.advance(PCM_SILENCE_WATCHDOG_S + 5.0)
    ingest.tick(now=clock.now)
    assert ingest.degraded is False


def test_watchdog_recovers_on_new_chunk():
    """
    given: watchdog has fired, degraded=True
    when: a new chunk is enqueued
    then: degraded flips back to False
    """
    received: List[List[BatchedChunk]] = []
    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        meeting_active_getter=lambda: True,
        silence_watchdog_s=PCM_SILENCE_WATCHDOG_S,
        time_fn=clock,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    ingest.enqueue(_make_chunk(seq=1))
    clock.advance(PCM_SILENCE_WATCHDOG_S + 1.0)
    ingest.tick(now=clock.now)
    assert ingest.degraded is True

    # New chunk arrives — recovery.
    ingest.enqueue(_make_chunk(seq=2))
    assert ingest.degraded is False


def test_enqueue_is_thread_safe_under_burst():
    """
    given: 2 threads enqueuing chunks concurrently
    when: both threads complete
    then: all chunks are recorded and buffers are internally consistent
    """
    import threading

    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )

    def worker(pid: str, base: int):
        for i in range(50):
            ingest.enqueue(_make_chunk(pid=pid, seq=base + i))

    t1 = threading.Thread(target=worker, args=("p1", 1))
    t2 = threading.Thread(target=worker, args=("p2", 1))
    t1.start(); t2.start()
    t1.join(); t2.join()

    total = sum(len(b) for b in ingest._buffers.values())
    assert total == 100
    assert len(ingest._buffers["p1"]) == 50
    assert len(ingest._buffers["p2"]) == 50


def test_batch_pcm_payload_is_int16_numpy():
    """
    given: a chunk with Int16LE bytes in `pcm`
    when: tick() drains the batch
    then: callback payload has a 1-D int16 numpy array at index 3
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    # 4 samples: [1, -1, 32767, -32768] as Int16LE.
    pcm_bytes = (
        np.array([1, -1, 32767, -32768], dtype="<i2").tobytes()
    )
    ingest.enqueue(_make_chunk(seq=1, pcm=pcm_bytes))
    ingest.tick(now=0.0)

    assert len(received) == 1
    batch = received[0]
    assert len(batch) == 1
    (
        pid,
        speaker_name,
        is_local,
        pcm_np,
        start_ms,
        end_ms,
        pid_audio_ms_start,
        pid_audio_ms_end,
    ) = batch[0]
    assert isinstance(pcm_np, np.ndarray)
    assert pcm_np.dtype == np.int16
    assert pcm_np.tolist() == [1, -1, 32767, -32768]


def test_batch_size_limit_respected_across_multiple_pids():
    """
    given: max_batch_size=4, 3 pids with 3 chunks each (9 total)
    when: tick() is called repeatedly
    then: each batch respects max_batch_size; 9 chunks split into 4 + 4 + 1
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        max_batch_size=4,
        max_chunks_per_pid=100,
    )
    for pid in ("pa", "pb", "pc"):
        for s in range(1, 4):
            ingest.enqueue(_make_chunk(pid=pid, seq=s))

    ingest.tick(now=0.0)
    ingest.tick(now=0.0)
    ingest.tick(now=0.0)
    assert [len(b) for b in received] == [4, 4, 1]


def test_watchdog_fires_when_no_pcm_chunk_ever_arrived():
    """
    given: meeting flips active, but no PCM chunks are ever enqueued
           (Chrome extension absent or silently broken)
    when: watchdog ticks after the silence threshold has elapsed since the
          meeting became active
    then: ingest.degraded becomes True so `_process_chunk` hands off to the
          mixed-path fallback instead of silently dropping system audio
    """
    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        meeting_active_getter=lambda: True,
        silence_watchdog_s=PCM_SILENCE_WATCHDOG_S,
        time_fn=clock,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    # First tick anchors _meeting_active_since; degraded still False.
    ingest.tick(now=clock.now)
    assert ingest.degraded is False

    # Advance past the threshold with no enqueue → _last_chunk_ts stays None.
    clock.advance(PCM_SILENCE_WATCHDOG_S + 1.0)
    ingest.tick(now=clock.now)
    assert ingest.degraded is True


def test_last_drain_generation_tracks_snapshot_between_drain_and_bump():
    """
    given: chunk enqueued at gen=5; drain captures the generation
    when: generation is bumped externally after drain
    then: last_drain_generation keeps the drain-time value (5), so the
          transcribe callback can compare it against current gen (6) and
          reject the stale batch (see code-review-pass-2 Bug B)
    """
    gen = {"value": 5}
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=received.append,
        generation_getter=lambda: gen["value"],
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    ingest.enqueue(_make_chunk(seq=1))

    ingest.tick(now=0.0)
    assert len(received) == 1
    assert ingest.last_drain_generation == 5

    # Simulate concurrent meeting switch after drain.
    gen["value"] = 6
    # Snapshot is stable — the callback reads it as the authoritative gen.
    assert ingest.last_drain_generation == 5


def test_pcm_ever_seen_false_until_first_enqueue_and_reset_on_drain():
    """
    given: fresh ingest (no chunks yet)
    when: enqueue, then drain, then enqueue again
    then: pcm_ever_seen starts False, flips True on the first enqueue, resets
          to False on drain (new meeting has no PCM yet), and flips True
          again on the next enqueue.  `LiveTranscriber` relies on this to
          keep the mixed-path running during the pre-join / broken-extension
          window instead of silently swallowing system audio.
    """
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        max_batch_size=100,
        max_chunks_per_pid=100,
    )
    assert ingest.pcm_ever_seen is False

    ingest.enqueue(_make_chunk(seq=1))
    assert ingest.pcm_ever_seen is True

    ingest.drain()
    assert ingest.pcm_ever_seen is False

    ingest.enqueue(_make_chunk(seq=2))
    assert ingest.pcm_ever_seen is True


def test_stop_joins_worker_thread():
    """
    given: ingest with started worker thread
    when: stop() is called
    then: internal _thread is cleaned up and no longer alive
    """
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda batch: None,
        batch_interval_ms=20,
    )
    ingest.start()
    assert ingest._thread is not None
    assert ingest._thread.is_alive()
    ingest.stop(timeout=1.0)
    assert ingest._thread is None


# ---------------------------------------------------------------------------
# Task 6: per-pid rolling PCM ring + counters (per-speaker-stream-refine §3)
# ---------------------------------------------------------------------------


def test_ring_caps_at_window_plus_head_room():
    """
    given: 30 one-second PCM chunks enqueued for a single pid
    when: all chunks are appended
    then: ring holds <= (PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S + 3.0) s
          of samples — oldest entries are dropped beyond capacity, while
          pid_audio_ms keeps counting raw input monotonically
    """
    from recog.config import PCM_CHUNK_DURATION_S, PCM_REFINE_WINDOW_S

    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()
    for s in range(1, 31):  # 30 s of audio
        ingest.enqueue(_make_chunk(
            seq=s, sample_rate=sample_rate, pcm=pcm_one_second,
        ))

    cap_seconds = PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S + 3.0
    cap_samples = int(cap_seconds * sample_rate)
    ring = ingest._pcm_rings["p1"]
    total_samples = sum(int(samples.size) for samples, _end_ms in ring)
    assert total_samples <= cap_samples
    # Head-room is ≥ one chunk (1 s at 16 kHz = 16000 samples), so we should
    # retain at least (cap_seconds - 1) s worth; guards against over-trimming.
    assert total_samples >= int((cap_seconds - 1) * sample_rate)
    # pid_audio_ms tracks total input regardless of ring trimming.
    assert ingest.get_pid_audio_ms("p1") == pytest.approx(30_000.0)


def test_pid_audio_ms_monotonic_per_pid():
    """
    given: two pids receive different numbers of chunks
    when: pid_audio_ms is read for each
    then: counters increment independently and start at 0 for unknown pids
    """
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()

    assert ingest.get_pid_audio_ms("p1") == 0.0
    for s in range(1, 6):  # p1: 5 s
        ingest.enqueue(_make_chunk(
            pid="p1", seq=s, sample_rate=sample_rate, pcm=pcm_one_second,
        ))
    for s in range(1, 4):  # p2: 3 s
        ingest.enqueue(_make_chunk(
            pid="p2", seq=s, sample_rate=sample_rate, pcm=pcm_one_second,
        ))

    assert ingest.get_pid_audio_ms("p1") == pytest.approx(5_000.0)
    assert ingest.get_pid_audio_ms("p2") == pytest.approx(3_000.0)
    assert ingest.get_pid_audio_ms("never_seen") == 0.0


def test_production_payload_chunks_strip_overlap_in_pid_frame():
    """
    given: chunks shaped like the production worklet emits — 3.6 s payload
           @ 16 kHz with 600 ms overlap carried from the previous chunk
    when:  three back-to-back chunks are enqueued for the same pid
    then:  `_pid_audio_ms` advances by the full payload on the first chunk
           and by the 3.0 s stride on every subsequent chunk (overlap
           stripped), and the refine ring stores non-overlapping samples
           so concatenation during `get_refine_candidate` cannot duplicate
           audio at the 3 s boundary (bug flagged in commit `ae25f69`
           "Known follow-up").
    """
    from recog.config import (
        PCM_CHUNK_DURATION_S,
        PCM_CHUNK_OVERLAP_MS,
        PCM_CHUNK_PAYLOAD_S,
    )

    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    payload_samples = int(PCM_CHUNK_PAYLOAD_S * sample_rate)
    overlap_samples = int(PCM_CHUNK_OVERLAP_MS * sample_rate / 1000)
    stride_samples = payload_samples - overlap_samples
    production_pcm = np.zeros(payload_samples, dtype="<i2").tobytes()

    for seq in range(1, 4):
        ingest.enqueue(_make_chunk(
            seq=seq, sample_rate=sample_rate, pcm=production_pcm,
        ))

    # pid_audio_ms: 3.6 s (first) + 3.0 s + 3.0 s = 9.6 s.
    expected_ms = (
        PCM_CHUNK_PAYLOAD_S + 2 * PCM_CHUNK_DURATION_S
    ) * 1000.0
    assert ingest.get_pid_audio_ms("p1") == pytest.approx(expected_ms)

    # Ring stores stripped chunks — first full payload, then stride-only.
    ring = ingest._pcm_rings["p1"]
    assert len(ring) == 3
    first_samples, first_end = ring[0]
    second_samples, second_end = ring[1]
    third_samples, third_end = ring[2]
    assert first_samples.size == payload_samples
    assert second_samples.size == stride_samples
    assert third_samples.size == stride_samples
    # End bounds advance with pid_audio_ms and are non-overlapping.
    assert first_end == pytest.approx(PCM_CHUNK_PAYLOAD_S * 1000.0)
    assert second_end == pytest.approx(
        (PCM_CHUNK_PAYLOAD_S + PCM_CHUNK_DURATION_S) * 1000.0
    )
    assert third_end == pytest.approx(expected_ms)


def test_silence_ms_resets_on_new_chunk():
    """
    given: a chunk enqueued, then several idle ticks, then another chunk
    when: silence counter is advanced by tick() for pids without new chunks
    then: _silence_ms[pid] increments by PCM_BATCH_INTERVAL_MS each idle tick
          and snaps back to 0 on the next enqueue
    """
    from recog.config import PCM_BATCH_INTERVAL_MS

    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        time_fn=clock,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()

    ingest.enqueue(_make_chunk(seq=1, sample_rate=sample_rate, pcm=pcm_one_second))
    assert ingest._silence_ms["p1"] == 0.0

    # First tick right after enqueue consumes the "touched" flag; silence
    # stays 0 because the pid was active during the previous interval.
    ingest.tick(now=clock.now)
    assert ingest._silence_ms["p1"] == 0.0

    # Subsequent idle ticks bump the counter by PCM_BATCH_INTERVAL_MS each.
    ingest.tick(now=clock.now)
    assert ingest._silence_ms["p1"] == pytest.approx(float(PCM_BATCH_INTERVAL_MS))
    ingest.tick(now=clock.now)
    assert ingest._silence_ms["p1"] == pytest.approx(2 * float(PCM_BATCH_INTERVAL_MS))

    # A new chunk for the same pid resets the counter immediately.
    ingest.enqueue(_make_chunk(seq=2, sample_rate=sample_rate, pcm=pcm_one_second))
    assert ingest._silence_ms["p1"] == 0.0


def test_continuous_speech_resets_after_turn_gap():
    """
    given: active speech on a pid, then silence crossing SPEAKER_TURN_GAP
    when: idle ticks accumulate past the turn-gap threshold
    then: _continuous_speech_ms[pid] is reset to 0 — prevents the monolog
          trigger in Task 7 from re-firing stale counters after a pause
    """
    from recog.config import PCM_BATCH_INTERVAL_MS, SPEAKER_TURN_GAP

    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        time_fn=clock,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()

    # Two chunks of continuous speech.
    ingest.enqueue(_make_chunk(seq=1, sample_rate=sample_rate, pcm=pcm_one_second))
    ingest.enqueue(_make_chunk(seq=2, sample_rate=sample_rate, pcm=pcm_one_second))
    assert ingest._continuous_speech_ms["p1"] == pytest.approx(2_000.0)

    # Enough idle ticks to cross SPEAKER_TURN_GAP (3 s by default).
    turn_gap_ms = SPEAKER_TURN_GAP * 1000.0
    ticks_needed = int(turn_gap_ms // PCM_BATCH_INTERVAL_MS) + 2  # +2 for slack
    for _ in range(ticks_needed):
        ingest.tick(now=clock.now)

    assert ingest._silence_ms["p1"] >= turn_gap_ms
    assert ingest._continuous_speech_ms["p1"] == 0.0


def test_refine_state_cleared_on_drain():
    """
    given: ring, pid_audio_ms and silence counters populated by enqueue
    when: drain() is called on the meeting boundary
    then: all per-pid refine state is wiped so the next meeting starts clean
    """
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()
    ingest.enqueue(_make_chunk(seq=1, sample_rate=sample_rate, pcm=pcm_one_second))
    assert ingest._pcm_rings
    assert ingest._pid_audio_ms["p1"] > 0.0

    ingest.drain()

    assert ingest._pcm_rings == {}
    assert ingest._pid_audio_ms == {}
    assert ingest._pid_drained_ms == {}
    assert ingest._refine_watermark_ms == {}
    assert ingest._silence_ms == {}
    assert ingest._continuous_speech_ms == {}
    assert ingest._pids_had_chunk_since_last_tick == set()
    assert ingest._pid_sample_rate == {}
    assert ingest.get_pid_audio_ms("p1") == 0.0


# ---------------------------------------------------------------------------
# Task 7: get_refine_candidate trigger logic (per-speaker-stream-refine §3.5)
# ---------------------------------------------------------------------------


def _advance_silence_past_turn_gap(ingest: PerSpeakerIngest, clock: "_Clock") -> None:
    """Drive enough idle ticks so `_silence_ms[pid] >= SPEAKER_TURN_GAP * 1000`
    for every pid seen so far.

    The first tick only *consumes* the touched-pids set carried over from the
    enqueue phase (it sets silence to 0 for those pids), subsequent ticks bump
    silence by `PCM_BATCH_INTERVAL_MS` each.
    """
    from recog.config import PCM_BATCH_INTERVAL_MS, SPEAKER_TURN_GAP

    turn_gap_ms = SPEAKER_TURN_GAP * 1000.0
    # +2 slack: one tick for consuming `touched`, one for rounding.
    ticks_needed = int(turn_gap_ms // PCM_BATCH_INTERVAL_MS) + 2
    for _ in range(ticks_needed):
        ingest.tick(now=clock.now)


def test_gap_trigger_returns_window_and_advances_watermark():
    """
    given: 5 one-second chunks on one pid followed by enough idle ticks to
           cross SPEAKER_TURN_GAP of silence
    when: get_refine_candidate("p1") is called
    then: returns a RefineTask covering the full 5 s trailing window with
          offset_base_ms=0 (ring starts at the pid origin), watermark advances
          to pid_audio_ms=5000 and the next call returns None
    """
    from recog.refine import RefineTask

    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        time_fn=clock,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()
    for s in range(1, 6):
        ingest.enqueue(_make_chunk(
            seq=s, sample_rate=sample_rate, pcm=pcm_one_second,
        ))

    _advance_silence_past_turn_gap(ingest, clock)

    task = ingest.get_refine_candidate("p1")
    assert isinstance(task, RefineTask)
    assert task.pid == "p1"
    assert task.pcm_samples.dtype == np.int16
    # 5 s of audio — the full trailing window fits inside PCM_REFINE_WINDOW_S.
    assert task.pcm_samples.size == 5 * sample_rate
    # Window begins at the pid origin (watermark was 0) — offset_base_ms = 0.
    assert task.offset_base_ms == pytest.approx(0.0)
    assert task.submitted_at == pytest.approx(clock.now)
    # Watermark advanced to pid_audio_ms, preventing a second gap-fire on the
    # same un-refined audio.
    assert ingest._refine_watermark_ms["p1"] == pytest.approx(5_000.0)
    assert ingest.get_refine_candidate("p1") is None


def test_monolog_trigger_fires_at_cap():
    """
    given: 15 s of continuous speech on one pid (no silence gap, no ticks
           between enqueues)
    when: get_refine_candidate("p1") is called
    then: monolog fallback fires (continuous_speech >= PCM_REFINE_MONOLOG_CAP_S
          * 1000), returns a RefineTask for the 15 s window, and
          _continuous_speech_ms is reset to 0 so the trigger does not re-fire
          on the next tick without fresh speech
    """
    from recog.config import PCM_REFINE_MONOLOG_CAP_S
    from recog.refine import RefineTask

    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        time_fn=clock,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()

    monolog_seconds = int(PCM_REFINE_MONOLOG_CAP_S)
    for s in range(1, monolog_seconds + 1):
        ingest.enqueue(_make_chunk(
            seq=s, sample_rate=sample_rate, pcm=pcm_one_second,
        ))
    # One tick drains the batch (max_batch_size=1000 covers all chunks) so
    # `_pid_drained_ms` matches `_pid_audio_ms` by the time
    # `get_refine_candidate` reads the drained-audio upper bound.
    ingest.tick(now=clock.now)
    # Monolog trigger precondition, gap trigger should NOT be active.
    assert (
        ingest._continuous_speech_ms["p1"]
        >= PCM_REFINE_MONOLOG_CAP_S * 1000.0
    )
    assert ingest._silence_ms["p1"] == 0.0

    task = ingest.get_refine_candidate("p1")
    assert isinstance(task, RefineTask)
    assert task.pid == "p1"
    assert task.pcm_samples.size == monolog_seconds * sample_rate
    assert task.offset_base_ms == pytest.approx(0.0)
    # Continuous-speech counter reset so next enqueue starts from zero.
    assert ingest._continuous_speech_ms["p1"] == 0.0
    assert ingest._refine_watermark_ms["p1"] == pytest.approx(
        float(monolog_seconds) * 1_000.0
    )


def test_no_candidate_when_no_unrefined_audio():
    """
    given: a pid that either (a) has never received a chunk, or (b) has had
           its watermark advanced to match pid_audio_ms (all audio already
           handed off for refine)
    when: get_refine_candidate is called
    then: returns None — the trigger check is short-circuited before any
          window assembly happens, and no triggers (gap/monolog) can fire on
          zero un-refined audio
    """
    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        time_fn=clock,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )

    # Case (a): pid never seen.
    assert ingest.get_refine_candidate("never_seen") is None

    # Case (b): pid saw audio + gap, then refine drained it; now another idle
    # tick or two keep silence high but un-refined audio is zero.
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()
    ingest.enqueue(_make_chunk(seq=1, sample_rate=sample_rate, pcm=pcm_one_second))
    _advance_silence_past_turn_gap(ingest, clock)
    assert ingest.get_refine_candidate("p1") is not None
    # No new chunks → still silent, still triggers check, but unrefined == 0.
    for _ in range(3):
        ingest.tick(now=clock.now)
    assert ingest.get_refine_candidate("p1") is None


def test_watermark_advance_prevents_double_fire():
    """
    given: a pid with buffered audio and silence past SPEAKER_TURN_GAP
    when: get_refine_candidate is called twice in a row without any new
          chunks arriving between the calls
    then: the first call returns a RefineTask; the second returns None
          because the watermark advance (performed atomically with task
          creation) leaves no un-refined audio for the second call
    """
    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        time_fn=clock,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()
    for s in range(1, 4):  # 3 s of audio
        ingest.enqueue(_make_chunk(
            seq=s, sample_rate=sample_rate, pcm=pcm_one_second,
        ))
    _advance_silence_past_turn_gap(ingest, clock)

    first = ingest.get_refine_candidate("p1")
    assert first is not None
    # Watermark sits at pid_audio_ms, so a second call on identical state
    # short-circuits at the unrefined_ms<=0 guard.
    second = ingest.get_refine_candidate("p1")
    assert second is None


def test_refine_candidate_caps_at_refine_window():
    """
    given: 30 s of continuous speech on one pid (exceeds PCM_REFINE_WINDOW_S)
    when: get_refine_candidate fires via monolog trigger
    then: the RefineTask's pcm_samples are capped at PCM_REFINE_WINDOW_S and
          offset_base_ms reflects the trailing window start inside the pid
          frame (pid_audio_ms - window_ms)
    """
    from recog.config import PCM_REFINE_WINDOW_S

    clock = _Clock(start=100.0)
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda b: None,
        time_fn=clock,
        max_batch_size=1000,
        max_chunks_per_pid=1000,
    )
    sample_rate = 16000
    pcm_one_second = np.zeros(sample_rate, dtype="<i2").tobytes()
    total_seconds = 30
    for s in range(1, total_seconds + 1):
        ingest.enqueue(_make_chunk(
            seq=s, sample_rate=sample_rate, pcm=pcm_one_second,
        ))
    # One tick drains the batch so `_pid_drained_ms` is in sync with
    # `_pid_audio_ms` — `get_refine_candidate` bounds the window on the
    # drained counter to avoid advancing the watermark past un-drained audio.
    ingest.tick(now=clock.now)

    task = ingest.get_refine_candidate("p1")
    assert task is not None
    window_samples = int(PCM_REFINE_WINDOW_S * sample_rate)
    assert task.pcm_samples.size == window_samples
    # Trailing window: offset_base_ms = pid_audio_ms - window_ms = 30000-20000.
    assert task.offset_base_ms == pytest.approx(
        float(total_seconds) * 1_000.0 - PCM_REFINE_WINDOW_S * 1_000.0
    )
    # Watermark advanced to full pid_audio_ms (not the window start): the
    # remaining pre-window audio is considered "skipped" — refine operates on
    # the trailing window only and does not loop back.
    assert ingest._refine_watermark_ms["p1"] == pytest.approx(
        float(total_seconds) * 1_000.0
    )


# =============================================================================
# ASR availability drain gate
# (docs/plans/2026-06-02-asr-fallback-buffering-design.md §3)
# =============================================================================

from recog.remote_asr import ASRTemporarilyUnavailable


def test_tick_does_not_drain_when_asr_unavailable():
    """
    given: ASR недоступен (asr_available_getter -> False)
    when: tick()
    then: transcribe_callback НЕ вызывается, чанки остаются в буферах;
          после восстановления ASR следующий tick выгребает накопленное
    """
    received: List[List[BatchedChunk]] = []
    asr_available = [False]
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda batch: received.append(batch),
        generation_getter=lambda: 0,
        meeting_active_getter=lambda: True,
        asr_available_getter=lambda: asr_available[0],
    )
    ingest.enqueue(_make_chunk(pid="p1", seq=0))

    ingest.tick()
    assert received == []

    # ASR вернулся -> следующий tick выгребает накопленное
    asr_available[0] = True
    ingest.tick()
    assert len(received) == 1
    assert len(received[0]) == 1
    assert received[0][0][0] == "p1"  # pid первого BatchedChunk


def test_unavailable_exception_holds_batch_for_retry():
    """
    given: callback кидает ASRTemporarilyUnavailable (гонка первого сбоя)
    when: tick(), затем ASR восстановился и tick() снова
    then: тот же батч диспатчится повторно — ничего не потеряно
    """
    calls: List[List[BatchedChunk]] = []
    fail_first = [True]

    def callback(batch):
        calls.append(batch)
        if fail_first[0]:
            fail_first[0] = False
            raise ASRTemporarilyUnavailable("remote just died")

    ingest = PerSpeakerIngest(
        transcribe_callback=callback,
        generation_getter=lambda: 0,
        meeting_active_getter=lambda: True,
        asr_available_getter=lambda: True,
    )
    ingest.enqueue(_make_chunk(pid="p1", seq=0))

    ingest.tick()   # callback падает с unavailable -> батч сохранён
    ingest.tick()   # повторный диспатч того же батча

    assert len(calls) == 2
    assert calls[0] == calls[1]  # тот же батч (те же BatchedChunk кортежи)


def test_default_asr_available_getter_keeps_existing_behavior():
    """
    given: PerSpeakerIngest без asr_available_getter (старый конструктор)
    when: tick()
    then: drain работает как раньше (обратная совместимость)
    """
    received: List[List[BatchedChunk]] = []
    ingest = PerSpeakerIngest(
        transcribe_callback=lambda batch: received.append(batch),
        generation_getter=lambda: 0,
        meeting_active_getter=lambda: True,
    )
    ingest.enqueue(_make_chunk(pid="p1", seq=0))
    ingest.tick()
    assert len(received) == 1
