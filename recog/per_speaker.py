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
from collections import deque
from dataclasses import dataclass, field
from typing import Callable, Deque, Dict, List, Optional, Set, Tuple

import numpy as np

from .config import (
    PCM_BATCH_INTERVAL_MS,
    PCM_CHUNK_DURATION_S,
    PCM_MAX_BATCH_SIZE,
    PCM_REFINE_MONOLOG_CAP_S,
    PCM_REFINE_WINDOW_S,
    PCM_SILENCE_WATCHDOG_S,
    SPEAKER_TURN_GAP,
)
from .refine import RefineTask

# The PcmChunk dataclass lives in `ktalk_bridge`; importing lazily inside
# callers would break static type checkers, so we re-import at module level.
# `ktalk_bridge` imports from `recog.config` but not from this module, so
# this does not introduce a cycle.
from ktalk_bridge import PcmChunk


# Payload produced for ASRModule.transcribe_batch:
# (pid, speaker_name, is_local, pcm_int16_np, chunk_start_ms, chunk_end_ms,
#  pid_audio_ms_end) — the last field is the per-pid monotonic audio counter
# snapshot taken at enqueue time (right after `_pid_audio_ms[pid]` was bumped
# for this chunk). Threaded through so `_register_pcm_live_segment` can stamp
# per-chunk bounds instead of reading a shared `_pid_audio_ms[pid]` that
# would be the SAME value for every chunk of a multi-chunk-same-pid batch.
BatchedChunk = Tuple[str, str, bool, np.ndarray, float, float, float]


@dataclass
class _BufferedChunk:
    """Internal wrapper that pairs a PcmChunk with the meeting generation
    snapshot taken at enqueue time."""
    chunk: PcmChunk
    meeting_generation: int
    enqueued_at: float
    # Snapshot of `_pid_audio_ms[chunk.pid]` taken right after this chunk was
    # accounted for — used to stamp per-chunk pid-frame bounds in the
    # live-segment registry instead of reading a shared counter that may have
    # already advanced past this chunk's end.
    pid_audio_ms_end: float = 0.0


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
        refine_submit_callback: Optional[Callable[[RefineTask], None]] = None,
    ):
        self._transcribe_callback = transcribe_callback
        self._generation_getter = generation_getter or (lambda: 0)
        self._meeting_active_getter = meeting_active_getter or (lambda: False)
        self._batch_interval_s = batch_interval_ms / 1000.0
        self._max_batch_size = max_batch_size
        self._silence_watchdog_s = silence_watchdog_s
        self._max_chunks_per_pid = max_chunks_per_pid
        self._time_fn = time_fn
        # Callback invoked on every tick for each pid that has a refine
        # candidate ready (gap or monolog trigger fired). `LiveTranscriber`
        # wires this to `RefineWorker.submit` when `pcm_refine_enabled()` is
        # True; None disables the refine path entirely (no iteration, no
        # overhead). Set via constructor or `set_refine_submit_callback`.
        self._refine_submit_callback: Optional[Callable[[RefineTask], None]] = (
            refine_submit_callback
        )

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

        # ----- refine state (§3 of per-speaker-stream-refine) ----------------
        # Per-pid rolling PCM ring: deque of Int16 np.ndarray chunks. Capacity
        # in seconds = PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S + head_room;
        # enforced per-pid at enqueue time by dropping oldest entries.
        self._pcm_rings: Dict[str, Deque[np.ndarray]] = {}
        # Per-pid monotonic audio counter (ms), D1 timeline contract. Advances
        # by chunk_ms on each enqueue; never reset except on meeting drain.
        self._pid_audio_ms: Dict[str, float] = {}
        # Per-pid pid_audio_ms of the oldest un-refined sample. Starts at 0 on
        # the first chunk; advanced by get_refine_candidate when a window is
        # handed off to the RefineWorker.
        self._refine_watermark_ms: Dict[str, float] = {}
        # Per-pid silence counter (ms). Reset to 0 on enqueue; advanced by
        # PCM_BATCH_INTERVAL_MS each tick the pid saw no new chunk.
        self._silence_ms: Dict[str, float] = {}
        # Per-pid continuous-speech counter (ms). Bumped by chunk_ms on
        # enqueue; reset to 0 when silence crosses SPEAKER_TURN_GAP.
        self._continuous_speech_ms: Dict[str, float] = {}
        # Which pids saw at least one chunk since the last tick — used to
        # decide which pids get silence-incremented by the tick loop.
        self._pids_had_chunk_since_last_tick: Set[str] = set()
        # Per-pid sample rate (samples/s) recorded from the most recent chunk.
        # Used by get_refine_candidate to convert samples↔ms; chunks from the
        # same pid share a rate in practice (extension is hard-coded to 16 kHz).
        self._pid_sample_rate: Dict[str, int] = {}
        # Ring capacity (ms) — derived from config once at construction.
        self._pid_ring_capacity_ms: float = (
            (PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S + 3.0) * 1000.0
        )

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

            # ----- refine per-pid bookkeeping (§3) ---------------------------
            # Decode once here so the refine ring and drain path can share it;
            # np.frombuffer is zero-copy so a second decode in _drain_batch_
            # locked is cheap, but keeping one authoritative decode is cleaner.
            samples = _decode_pcm_int16(chunk.pcm)
            sample_rate = chunk.sample_rate or 16000
            chunk_ms = (samples.size / sample_rate) * 1000.0 if sample_rate else 0.0
            if samples.size > 0 and chunk_ms > 0.0:
                ring = self._pcm_rings.setdefault(chunk.pid, deque())
                ring.append(samples)
                self._pid_audio_ms[chunk.pid] = (
                    self._pid_audio_ms.get(chunk.pid, 0.0) + chunk_ms
                )
                # Watermark is initialised to 0 on the first chunk; advanced by
                # get_refine_candidate (Task 7) when a window is handed off.
                self._refine_watermark_ms.setdefault(chunk.pid, 0.0)
                self._silence_ms[chunk.pid] = 0.0
                self._continuous_speech_ms[chunk.pid] = (
                    self._continuous_speech_ms.get(chunk.pid, 0.0) + chunk_ms
                )
                self._pid_sample_rate[chunk.pid] = sample_rate
                self._enforce_ring_capacity_locked(chunk.pid, sample_rate)
                self._pids_had_chunk_since_last_tick.add(chunk.pid)
                # Snapshot per-chunk end bound AFTER counter advance so each
                # chunk carries its own pid-frame end through the batch
                # pipeline. Multiple chunks from the same pid in one batch
                # would otherwise all read the same latest counter value at
                # _register_pcm_live_segment time and collapse to identical
                # bounds.
                buffered.pid_audio_ms_end = self._pid_audio_ms[chunk.pid]

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

            # Refine state: pid frames are per-meeting, so wipe all per-pid
            # rings and counters together with the live buffers.
            self._pcm_rings.clear()
            self._pid_audio_ms.clear()
            self._refine_watermark_ms.clear()
            self._silence_ms.clear()
            self._continuous_speech_ms.clear()
            self._pids_had_chunk_since_last_tick.clear()
            self._pid_sample_rate.clear()

    def tick(self, now: Optional[float] = None) -> List[BatchedChunk]:
        """Advance state by one tick: check watchdog, drain up to one batch,
        dispatch to callback, and hand any ready refine candidates off to
        `_refine_submit_callback`.

        Returns the batch that was dispatched (primarily for tests).
        """
        if now is None:
            now = self._time_fn()
        batch: List[BatchedChunk] = []
        active_pids: List[str] = []
        with self._lock:
            self._check_watchdog_locked(now)
            batch = self._drain_batch_locked()
            # Advance per-pid silence counter for pids that did not see a new
            # chunk this interval. Runs unconditionally so Task 7's trigger
            # logic has fresh `_silence_ms` / `_continuous_speech_ms` numbers
            # even when the drain itself was a no-op.
            self._update_per_pid_silence_locked()
            if self._refine_submit_callback is not None:
                # Snapshot pid list under the lock; get_refine_candidate
                # re-acquires _lock (it's threading.Lock, not reentrant), so
                # the per-pid check must run without _lock held.
                active_pids = list(self._pid_audio_ms.keys())
        if batch:
            try:
                self._transcribe_callback(batch)
            except Exception as e:
                print(
                    f"[PerSpeakerIngest] transcribe callback raised: {e}",
                    file=sys.stderr,
                )
        if active_pids and self._refine_submit_callback is not None:
            for pid in active_pids:
                task = self.get_refine_candidate(pid)
                if task is None:
                    continue
                try:
                    self._refine_submit_callback(task)
                except Exception as e:
                    print(
                        f"[PerSpeakerIngest] refine submit raised: {e}",
                        file=sys.stderr,
                    )
        return batch

    def set_refine_submit_callback(
        self, callback: Optional[Callable[[RefineTask], None]]
    ) -> None:
        """Attach / detach the per-tick refine submit callback.

        Exists so `LiveTranscriber` can build the `PerSpeakerIngest` first
        (fast, cheap) and only later build the `RefineWorker` (gated on
        `pcm_refine_enabled()`). `None` disables the refine iteration in
        `tick()` entirely.
        """
        with self._lock:
            self._refine_submit_callback = callback

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

    def get_pid_audio_ms(self, pid: str) -> float:
        """Return the per-pid monotonic audio counter in milliseconds.

        Used by LiveTranscriber._on_pcm_batch (Task 8) to stamp live segments
        with pid-frame bounds. Reads self._lock to observe a consistent value
        against concurrent enqueue updates.
        """
        with self._lock:
            return self._pid_audio_ms.get(pid, 0.0)

    def get_refine_candidate(self, pid: str) -> Optional[RefineTask]:
        """Return a `RefineTask` for `pid` if the gap or monolog trigger has
        fired, else None.

        Gap trigger: `_silence_ms[pid] >= SPEAKER_TURN_GAP * 1000`.
        Monolog fallback: `_continuous_speech_ms[pid] >= PCM_REFINE_MONOLOG_CAP_S * 1000`.

        Assembles the trailing PCM window from the per-pid ring — spanning
        `[watermark, pid_audio_ms]` and capped at `PCM_REFINE_WINDOW_S * 1000`
        ms. Computes `offset_base_ms = max(0, pid_audio_ms - samples_duration_ms)`
        so upstream word timestamps can be rebased into the pid frame. Advances
        `_refine_watermark_ms[pid]` to `pid_audio_ms` atomically so a second
        call without new audio returns None. On monolog trigger also resets
        `_continuous_speech_ms[pid]=0` to avoid re-firing next tick.

        Thread-safe: the watermark advance and task creation happen atomically
        under `self._lock`, so a concurrent `enqueue` cannot slip un-refined
        audio past the handoff.
        """
        with self._lock:
            pid_audio_ms = self._pid_audio_ms.get(pid, 0.0)
            watermark = self._refine_watermark_ms.get(pid, 0.0)
            unrefined_ms = pid_audio_ms - watermark
            if unrefined_ms <= 0.0:
                return None

            silence_ms = self._silence_ms.get(pid, 0.0)
            continuous_ms = self._continuous_speech_ms.get(pid, 0.0)
            gap_triggered = silence_ms >= SPEAKER_TURN_GAP * 1000.0
            monolog_triggered = (
                continuous_ms >= PCM_REFINE_MONOLOG_CAP_S * 1000.0
            )
            if not (gap_triggered or monolog_triggered):
                return None

            ring = self._pcm_rings.get(pid)
            if not ring:
                return None

            sample_rate = self._pid_sample_rate.get(pid, 16000) or 16000
            window_ms = PCM_REFINE_WINDOW_S * 1000.0
            target_ms = min(unrefined_ms, window_ms)
            target_samples = int(round(target_ms * sample_rate / 1000.0))
            if target_samples <= 0:
                return None

            pulled: List[np.ndarray] = []
            total = 0
            for entry in reversed(ring):
                pulled.append(entry)
                total += int(entry.size)
                if total >= target_samples:
                    break
            if not pulled:
                return None
            pulled.reverse()
            pcm_samples = np.concatenate(pulled)
            if pcm_samples.dtype != np.int16:
                pcm_samples = pcm_samples.astype(np.int16, copy=False)
            # Trim leading samples if we overshot (happens when the oldest
            # pulled chunk's tail overshoots target — only the tail of that
            # chunk belongs to the window).
            if pcm_samples.size > target_samples:
                pcm_samples = pcm_samples[pcm_samples.size - target_samples:]

            actual_samples_ms = (pcm_samples.size / sample_rate) * 1000.0
            offset_base_ms = max(0.0, pid_audio_ms - actual_samples_ms)

            self._refine_watermark_ms[pid] = pid_audio_ms
            if monolog_triggered:
                self._continuous_speech_ms[pid] = 0.0

            return RefineTask(
                pid=pid,
                pcm_samples=pcm_samples,
                offset_base_ms=offset_base_ms,
                submitted_at=self._time_fn(),
                meeting_generation=self._generation_getter(),
            )

    # ----- internal -------------------------------------------------------

    def _enforce_ring_capacity_locked(self, pid: str, sample_rate: int) -> None:
        """Drop oldest ring entries for `pid` until total samples <= capacity.

        Assumes self._lock is held. Rings hold whole chunks (deque entries are
        np.ndarray slices of size ~= PCM_CHUNK_PAYLOAD_S * sr), so we drop at
        chunk granularity — the `+3.0` head-room in capacity absorbs up to one
        over-cap chunk.
        """
        ring = self._pcm_rings.get(pid)
        if not ring or sample_rate <= 0:
            return
        cap_samples = int(self._pid_ring_capacity_ms * sample_rate / 1000.0)
        if cap_samples <= 0:
            return
        total = sum(int(entry.size) for entry in ring)
        while total > cap_samples and len(ring) > 1:
            oldest = ring.popleft()
            total -= int(oldest.size)

    def _update_per_pid_silence_locked(self) -> None:
        """Advance `_silence_ms` for pids idle this tick; reset continuous
        speech when silence crosses SPEAKER_TURN_GAP.

        Assumes self._lock is held. Pids that saw at least one chunk since the
        previous tick keep `_silence_ms=0` (reset by enqueue) and do not get
        bumped here.
        """
        touched = self._pids_had_chunk_since_last_tick
        self._pids_had_chunk_since_last_tick = set()
        turn_gap_ms = SPEAKER_TURN_GAP * 1000.0
        for pid in list(self._pid_audio_ms.keys()):
            if pid in touched:
                continue
            new_silence = self._silence_ms.get(pid, 0.0) + float(PCM_BATCH_INTERVAL_MS)
            self._silence_ms[pid] = new_silence
            if new_silence >= turn_gap_ms:
                self._continuous_speech_ms[pid] = 0.0

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
                buffered.pid_audio_ms_end,
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
