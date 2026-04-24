"""Unit tests for LiveTranscriber._register_pcm_live_segment (refine-backend Task 8).

The registry is a per-pid list of `(segment_id, pid_audio_ms_start,
pid_audio_ms_end)` populated after `append_segment` in `_on_pcm_batch`.
Task 9 (alignment) consumes it to map refined word timestamps back to the
live segment IDs.
"""
from __future__ import annotations

import sys
import threading
from pathlib import Path
from unittest.mock import Mock

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.config import PCM_REFINE_WINDOW_S
from recog.live import LiveTranscriber


SAMPLE_RATE = 16000
CHUNK_3S_SAMPLES = SAMPLE_RATE * 3  # 3 s chunk => 3000 ms


def _make_transcriber() -> Mock:
    """Build a Mock(spec=LiveTranscriber) with the minimal state the helper
    reads: _pcm_live_segments, _pcm_segment_texts (pruned in lockstep with
    the registry by Task 9), _state_lock, sample_rate, and a stubbed
    _per_speaker_ingest (non-None sentinel — the helper's no-op guard reads
    only `is None`, so a bare Mock is sufficient; per-chunk `pid_audio_ms_end`
    is now threaded in through the call argument).
    """
    t = Mock(spec=LiveTranscriber)
    t._state_lock = threading.Lock()
    t.sample_rate = SAMPLE_RATE
    t._pcm_live_segments = {}
    t._pcm_segment_texts = {}
    t._per_speaker_ingest = Mock()
    return t


class TestRegistryAppendsOnEmit:
    """given a PCM chunk emission - when _register_pcm_live_segment - then registry entry added with correct pid-frame bounds."""

    def test_appends_entry_with_pid_frame_bounds(self):
        pid = "pid-1"
        t = _make_transcriber()
        samples = np.zeros(CHUNK_3S_SAMPLES, dtype=np.int16)

        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid, 42, samples, 3000.0)

        assert t._pcm_live_segments[pid] == [(42, 0.0, 3000.0)]

    def test_appends_preserve_order_on_multiple_chunks(self):
        pid = "pid-1"
        t = _make_transcriber()
        samples = np.zeros(CHUNK_3S_SAMPLES, dtype=np.int16)

        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid, 1, samples, 3000.0)
        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid, 2, samples, 6000.0)
        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid, 3, samples, 9000.0)

        entries = t._pcm_live_segments[pid]
        assert [e[0] for e in entries] == [1, 2, 3]
        assert entries[0] == (1, 0.0, 3000.0)
        assert entries[1] == (2, 3000.0, 6000.0)
        assert entries[2] == (3, 6000.0, 9000.0)

    def test_multiple_chunks_same_pid_same_batch_use_per_chunk_bounds(self):
        """given: two chunks of the same pid drained together in one batch
           when:  _register_pcm_live_segment called for each with its own
                  pid_audio_ms_end (snapshotted at enqueue time)
           then:  each segment records its true per-chunk bounds rather than
                  collapsing to the latest-seen counter value — this is the
                  burst/reconnect bug that the per-chunk threading fixes
        """
        pid = "pid-1"
        t = _make_transcriber()
        samples = np.zeros(CHUNK_3S_SAMPLES, dtype=np.int16)

        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid, 1, samples, 3000.0)
            LiveTranscriber._register_pcm_live_segment(t, pid, 2, samples, 6000.0)

        entries = t._pcm_live_segments[pid]
        assert entries[0] == (1, 0.0, 3000.0)
        assert entries[1] == (2, 3000.0, 6000.0)

    def test_no_ingest_no_op(self):
        """If _per_speaker_ingest is None the helper must be a no-op — path
        is reachable during teardown / when pcm-transport is disabled."""
        t = _make_transcriber()
        t._per_speaker_ingest = None
        samples = np.zeros(CHUNK_3S_SAMPLES, dtype=np.int16)

        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, "pid-1", 1, samples, 3000.0)

        assert t._pcm_live_segments == {}


class TestRegistryPrunesOldEntries:
    """given an old entry outside the refine window + head room - when a new chunk is registered - then old entries are dropped."""

    def test_prune_floor_matches_window_plus_head_room(self):
        pid = "pid-1"
        # PCM_REFINE_WINDOW_S=20, head room=5 => prune floor = now - 25 s = now - 25000 ms.
        now_ms = 40_000.0
        t = _make_transcriber()
        # Seed three prior entries: two below the floor (14 s, 10 s), one right on the boundary (15 s).
        t._pcm_live_segments[pid] = [
            (1, 2_000.0, 5_000.0),    # end=5_000 < 15_000 => pruned
            (2, 7_000.0, 10_000.0),   # end=10_000 < 15_000 => pruned
            (3, 14_000.0, 15_000.0),  # end=15_000 NOT < 15_000 => kept
        ]
        samples = np.zeros(CHUNK_3S_SAMPLES, dtype=np.int16)

        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid, 4, samples, now_ms)

        entries = t._pcm_live_segments[pid]
        assert [e[0] for e in entries] == [3, 4]
        assert entries[-1] == (4, 37_000.0, 40_000.0)

    def test_prune_keeps_entries_inside_window(self):
        pid = "pid-1"
        now_ms = 30_000.0
        t = _make_transcriber()
        # floor = 30_000 - (PCM_REFINE_WINDOW_S + 5) * 1000 = 30_000 - 25_000 = 5_000.
        # Entries with end >= 5_000 must survive.
        t._pcm_live_segments[pid] = [
            (10, 3_000.0, 6_000.0),
            (11, 6_000.0, 9_000.0),
        ]
        samples = np.zeros(CHUNK_3S_SAMPLES, dtype=np.int16)

        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid, 12, samples, now_ms)

        entries = t._pcm_live_segments[pid]
        assert [e[0] for e in entries] == [10, 11, 12]


class TestRegistryPartitionedByPid:
    """given chunks from multiple pids - when _register_pcm_live_segment is called - then entries do not leak across pids."""

    def test_two_pids_independent(self):
        pid_a = "pid-A"
        pid_b = "pid-B"
        t = _make_transcriber()
        samples = np.zeros(CHUNK_3S_SAMPLES, dtype=np.int16)

        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid_a, 10, samples, 3_000.0)
            LiveTranscriber._register_pcm_live_segment(t, pid_b, 20, samples, 6_000.0)

        assert t._pcm_live_segments[pid_a] == [(10, 0.0, 3_000.0)]
        assert t._pcm_live_segments[pid_b] == [(20, 3_000.0, 6_000.0)]

    def test_prune_does_not_touch_other_pid(self):
        pid_a = "pid-A"
        pid_b = "pid-B"
        t = _make_transcriber()
        # pid_a is active (now=40s) so its own old entry gets pruned; pid_b is idle
        # and its old-looking entry must stay because we never ran a prune for pid_b.
        t._pcm_live_segments[pid_a] = [(1, 2_000.0, 5_000.0)]  # will be pruned
        t._pcm_live_segments[pid_b] = [(99, 0.0, 500.0)]       # must survive
        samples = np.zeros(CHUNK_3S_SAMPLES, dtype=np.int16)

        with t._state_lock:
            LiveTranscriber._register_pcm_live_segment(t, pid_a, 2, samples, 40_000.0)

        assert [e[0] for e in t._pcm_live_segments[pid_a]] == [2]
        assert t._pcm_live_segments[pid_b] == [(99, 0.0, 500.0)]
