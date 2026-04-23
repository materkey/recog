"""Per-speaker PCM ingest for live KTalk transcription.

Collects PCM chunks received from the Chrome extension (via `ktalk_bridge`),
buffers them per participant (pid), reorders by seq, and batches them up to
`PCM_MAX_BATCH_SIZE` for a single `ASRModule.transcribe_batch` call.

Meeting-generation filter: each enqueued chunk captures the current
`meeting_generation` (via injected getter). Chunks whose snapshot does not
match the current generation at batch-drain time are discarded — this is the
same mechanism used by `LiveTranscriber._process_chunk` to avoid leaking
audio across meeting boundaries.

Silence watchdog: if PCM traffic stops for more than `PCM_SILENCE_WATCHDOG_S`
while a meeting is active, the ingest enters degraded mode. The live
transcriber reads `degraded` to decide whether to fall back to the mixed
(system-capture) path; the ingest does NOT drive the fallback itself.

See docs/plans/2026-04-22-per-speaker-asr-pipeline.md Task 3.
"""

from __future__ import annotations

import bisect
import sys
import threading
import time
from dataclasses import dataclass, field
from typing import Callable, Dict, List, Optional, Tuple

import numpy as np

from .config import (
    PCM_BATCH_INTERVAL_MS,
    PCM_MAX_BATCH_SIZE,
    PCM_SILENCE_WATCHDOG_S,
)

# The PcmChunk dataclass lives in `ktalk_bridge`; importing lazily inside
# callers would break static type checkers, so we re-import at module level.
# `ktalk_bridge` imports from `recog.config` but not from this module, so
# this does not introduce a cycle.
from ktalk_bridge import PcmChunk


# Payload produced for ASRModule.transcribe_batch:
# (pid, speaker_name, is_local, pcm_int16_np, chunk_start_ms, chunk_end_ms)
BatchedChunk = Tuple[str, str, bool, np.ndarray, float, float]


@dataclass
class _BufferedChunk:
    """Internal wrapper that pairs a PcmChunk with the meeting generation
    snapshot taken at enqueue time."""
    chunk: PcmChunk
    meeting_generation: int
    enqueued_at: float


class PerSpeakerIngest:
    """Per-pid ring buffers + batched dispatch to ASR.

    Thread-safe: `enqueue` is called from the bridge's asyncio thread;
    `tick`/`_run` executes on an internal worker thread; `drain`/`stop` may
    be called from any thread.
    """

    def __init__(
        self,
        transcribe_callback: Callable[[List[BatchedChunk]], None],
        generation_getter: Optional[Callable[[], int]] = None,
        meeting_active_getter: Optional[Callable[[], bool]] = None,
        batch_interval_ms: int = PCM_BATCH_INTERVAL_MS,
        max_batch_size: int = PCM_MAX_BATCH_SIZE,
        silence_watchdog_s: float = PCM_SILENCE_WATCHDOG_S,
        max_chunks_per_pid: int = 3 * PCM_MAX_BATCH_SIZE,
        time_fn: Callable[[], float] = time.time,
    ):
        self._transcribe_callback = transcribe_callback
        self._generation_getter = generation_getter or (lambda: 0)
        self._meeting_active_getter = meeting_active_getter or (lambda: False)
        self._batch_interval_s = batch_interval_ms / 1000.0
        self._max_batch_size = max_batch_size
        self._silence_watchdog_s = silence_watchdog_s
        self._max_chunks_per_pid = max_chunks_per_pid
        self._time_fn = time_fn

        self._lock = threading.Lock()
        # Per-pid list kept sorted by seq; bisect.insort is O(log n) lookup +
        # O(n) insert — fine for n ≤ max_chunks_per_pid (24 by default).
        self._buffers: Dict[str, List[_BufferedChunk]] = {}
        # Per-pid lightweight counters for diagnostics/observability. Grows
        # only with distinct participant count, not chunks — bounded.
        self._stats: Dict[str, dict] = {}
        self._last_chunk_ts: Optional[float] = None
        self._degraded = False
        # Whether any PCM chunk has been seen for the current meeting. Stays
        # False from session start / after `drain()` until the first enqueue.
        # `LiveTranscriber` gates its PCM-primary branch on this so system
        # audio is not silently dropped during the pre-join / broken-extension
        # window (where PCM has never arrived and watchdog has not yet fired).
        self._pcm_ever_seen = False
        # First time-fn reading where `meeting_active_getter()` flipped True;
        # lets the watchdog fire even if no PCM chunk ever arrives (broken
        # extension → `_last_chunk_ts` would otherwise stay None forever).
        self._meeting_active_since: Optional[float] = None
        # Meeting generation snapshot taken inside the most recent
        # `_drain_batch_locked`.  Read by the transcribe callback to guard
        # against `_invalidate_pending_chunks` bumping the counter between
        # drain and callback entry (see plan §code-review-pass-2 Bug B).
        self._last_drain_generation: Optional[int] = None

        self._stop_event = threading.Event()
        self._wake_event = threading.Event()
        self._thread: Optional[threading.Thread] = None

    # ----- public API -----------------------------------------------------

    @property
    def degraded(self) -> bool:
        with self._lock:
            return self._degraded

    def get_stats(self) -> dict:
        """Snapshot per-pid counters + top-level flags for live diagnostics.

        Safe to call from any thread. Returned dict is a deep copy.
        """
        now = self._time_fn()
        with self._lock:
            per_pid = {}
            for pid, s in self._stats.items():
                chunks = s["chunks_seen"]
                duration = max(now - s["first_seen_ts"], 0.001)
                per_pid[pid] = {
                    "speaker_name": s["speaker_name"],
                    "is_local": s["is_local"],
                    "chunks_seen": chunks,
                    "last_seq": s["last_seq"],
                    "gaps_count": s["gaps_count"],
                    "out_of_order_count": s["out_of_order_count"],
                    "first_seen_ts": s["first_seen_ts"],
                    "last_seen_ts": s["last_seen_ts"],
                    "age_s": round(now - s["first_seen_ts"], 2),
                    "since_last_chunk_s": round(now - s["last_seen_ts"], 2),
                    "chunks_per_sec": round(chunks / duration, 2),
                }
            return {
                "pcm_ever_seen": self._pcm_ever_seen,
                "degraded": self._degraded,
                "meeting_active_since": self._meeting_active_since,
                "last_chunk_ts": self._last_chunk_ts,
                "now": now,
                "pending_buffer_sizes": {
                    pid: len(buf) for pid, buf in self._buffers.items()
                },
                "per_pid": per_pid,
            }

    @property
    def pcm_ever_seen(self) -> bool:
        """True once at least one PCM chunk has been enqueued for the current
        meeting.  Reset by `drain()` on meeting boundary.  `LiveTranscriber`
        uses this to keep the mixed-path running until PCM actually arrives,
        so starting the session (or joining a broken extension) does not
        silently swallow system audio for up to `PCM_SILENCE_WATCHDOG_S`.
        """
        with self._lock:
            return self._pcm_ever_seen

    @property
    def last_drain_generation(self) -> Optional[int]:
        """Generation snapshot taken at the most recent `_drain_batch_locked`.

        The transcribe callback reads this to reject batches whose meeting
        has switched between drain and callback invocation — `self._meeting_
        generation` on `LiveTranscriber` is NOT safe to snapshot at callback
        entry because `_invalidate_pending_chunks` may have bumped it in
        between.  All chunks in a given batch share the same generation
        (enforced by drain's stale-gen filter) so one read suffices.
        """
        with self._lock:
            return self._last_drain_generation

    def enqueue(self, chunk: PcmChunk) -> None:
        """Enqueue a PCM chunk.  Safe to call from the bridge asyncio thread."""
        now = self._time_fn()
        buffered = _BufferedChunk(
            chunk=chunk,
            meeting_generation=self._generation_getter(),
            enqueued_at=now,
        )
        wake = False
        with self._lock:
            buf = self._buffers.setdefault(chunk.pid, [])
            # Keep the list sorted by seq so drain emits in-order per pid.
            # bisect needs a key comparable — extract via small helper.
            seqs = [b.chunk.seq for b in buf]
            idx = bisect.bisect_left(seqs, chunk.seq)
            buf.insert(idx, buffered)
            # Drop-oldest when over capacity.
            while len(buf) > self._max_chunks_per_pid:
                buf.pop(0)

            stats = self._stats.setdefault(chunk.pid, {
                "speaker_name": chunk.speaker_name,
                "is_local": chunk.is_local,
                "chunks_seen": 0,
                "first_seen_ts": now,
                "last_seen_ts": now,
                "last_seq": None,
                "gaps_count": 0,
                "out_of_order_count": 0,
            })
            stats["speaker_name"] = chunk.speaker_name or stats["speaker_name"]
            stats["chunks_seen"] += 1
            stats["last_seen_ts"] = now
            prev_seq = stats["last_seq"]
            if prev_seq is not None:
                if chunk.seq > prev_seq + 1:
                    stats["gaps_count"] += chunk.seq - prev_seq - 1
                elif chunk.seq <= prev_seq:
                    stats["out_of_order_count"] += 1
            if prev_seq is None or chunk.seq > prev_seq:
                stats["last_seq"] = chunk.seq

            # Watchdog: new chunk → exit degraded mode if we were in it.
            if self._degraded:
                print(
                    "[PerSpeakerIngest] pcm recovered; exiting degraded mode",
                    file=sys.stderr,
                )
                self._degraded = False
            self._last_chunk_ts = now
            self._pcm_ever_seen = True

            total = sum(len(b) for b in self._buffers.values())
            if total >= self._max_batch_size:
                wake = True
        if wake:
            # Poke the worker thread to drain immediately without waiting
            # for the 200 ms tick.
            self._wake_event.set()

    def drain(self) -> None:
        """Clear all per-pid buffers.  Called from `_invalidate_pending_chunks`
        on meeting boundary before `_meeting_generation` is bumped."""
        with self._lock:
            self._buffers.clear()
            self._stats.clear()
            # Leave watchdog state untouched — a new meeting has a fresh
            # `_last_chunk_ts` on the first enqueue.
            self._last_chunk_ts = None
            self._degraded = False
            # New meeting starts without any PCM seen yet — until a chunk
            # arrives, `LiveTranscriber` should keep running the mixed-path
            # rather than dropping system audio in anticipation of PCM.
            self._pcm_ever_seen = False
            # New meeting = new grace period; old reference is stale.
            self._meeting_active_since = None
            # Intentionally do NOT reset `_last_drain_generation`: an in-flight
            # callback from the previous meeting needs the old gen to compare
            # against the bumped `self._meeting_generation` and correctly
            # discard the batch.

    def tick(self, now: Optional[float] = None) -> List[BatchedChunk]:
        """Advance state by one tick: check watchdog, drain up to one batch,
        dispatch to callback.

        Returns the batch that was dispatched (primarily for tests).
        """
        if now is None:
            now = self._time_fn()
        batch: List[BatchedChunk] = []
        with self._lock:
            self._check_watchdog_locked(now)
            batch = self._drain_batch_locked()
        if batch:
            try:
                self._transcribe_callback(batch)
            except Exception as e:
                print(
                    f"[PerSpeakerIngest] transcribe callback raised: {e}",
                    file=sys.stderr,
                )
        return batch

    def start(self) -> None:
        """Spawn the background worker thread."""
        if self._thread is not None and self._thread.is_alive():
            return
        self._stop_event.clear()
        self._wake_event.clear()
        self._thread = threading.Thread(
            target=self._run,
            name="PerSpeakerIngest",
            daemon=True,
        )
        self._thread.start()

    def stop(self, timeout: float = 2.0) -> None:
        """Signal worker thread to exit and wait for join."""
        self._stop_event.set()
        self._wake_event.set()
        if self._thread is not None:
            self._thread.join(timeout=timeout)
            self._thread = None

    # ----- internal -------------------------------------------------------

    def _check_watchdog_locked(self, now: float) -> None:
        """Must be called with self._lock held."""
        if self._degraded:
            return
        if not self._meeting_active_getter():
            # Outside a meeting the ingest sits idle; clear the grace-period
            # anchor so the next meeting start gets a fresh window.
            self._meeting_active_since = None
            return
        # Anchor the watchdog window at the first tick where the meeting went
        # active.  If PCM chunks never arrive (e.g., broken extension), this
        # gives the watchdog a non-None reference so it can still fire and
        # hand off to the mixed-path fallback.
        if self._meeting_active_since is None:
            self._meeting_active_since = now
        reference = (
            self._last_chunk_ts
            if self._last_chunk_ts is not None
            else self._meeting_active_since
        )
        elapsed = now - reference
        if elapsed > self._silence_watchdog_s:
            suffix = (
                f"{elapsed:.1f}s since last chunk"
                if self._last_chunk_ts is not None
                else f"{elapsed:.1f}s since meeting start with no pcm ever"
            )
            print(
                f"[PerSpeakerIngest] pcm silence watchdog fired "
                f"({suffix}); falling back to mixed-path",
                file=sys.stderr,
            )
            self._degraded = True

    def _drain_batch_locked(self) -> List[BatchedChunk]:
        """Collect up to `max_batch_size` chunks across all pids and return
        them as BatchedChunk tuples.  Stale-generation chunks are discarded.

        Must be called with self._lock held.
        """
        current_gen = self._generation_getter()
        # Record drain-time gen so the transcribe callback can compare it
        # against the current `_meeting_generation` in `LiveTranscriber` and
        # skip emission if the meeting has switched since drain.
        self._last_drain_generation = current_gen
        batch: List[BatchedChunk] = []
        dropped_stale = 0

        while len(batch) < self._max_batch_size:
            # Pick the pid whose head chunk has the lowest seq (globally).
            # Ties broken lexically by pid.  This gives deterministic
            # interleaving across pids while preserving in-pid seq order.
            best_pid: Optional[str] = None
            best_seq: Optional[int] = None
            for pid in sorted(self._buffers.keys()):
                buf = self._buffers[pid]
                if not buf:
                    continue
                head_seq = buf[0].chunk.seq
                if best_seq is None or head_seq < best_seq:
                    best_pid = pid
                    best_seq = head_seq
            if best_pid is None:
                break

            buffered = self._buffers[best_pid].pop(0)
            if buffered.meeting_generation != current_gen:
                dropped_stale += 1
                continue

            pcm_i16 = _decode_pcm_int16(buffered.chunk.pcm)
            batch.append((
                buffered.chunk.pid,
                buffered.chunk.speaker_name,
                buffered.chunk.is_local,
                pcm_i16,
                buffered.chunk.chunk_start_ts * 1000.0,
                buffered.chunk.chunk_end_ts * 1000.0,
            ))

        if dropped_stale:
            print(
                f"[PerSpeakerIngest] dropped {dropped_stale} stale-generation "
                f"chunks (current_gen={current_gen})",
                file=sys.stderr,
            )
        return batch

    def _run(self) -> None:
        """Background worker loop: tick every `batch_interval_s`, or sooner
        if woken via `_wake_event` (e.g. batch is already full)."""
        while not self._stop_event.is_set():
            self._wake_event.wait(timeout=self._batch_interval_s)
            self._wake_event.clear()
            if self._stop_event.is_set():
                break
            try:
                self.tick()
            except Exception as e:
                print(
                    f"[PerSpeakerIngest] tick raised: {e}",
                    file=sys.stderr,
                )


def _decode_pcm_int16(pcm_bytes: bytes) -> np.ndarray:
    """Decode Int16LE PCM bytes into a 1-D int16 numpy array (zero-copy)."""
    return np.frombuffer(pcm_bytes, dtype="<i2")
