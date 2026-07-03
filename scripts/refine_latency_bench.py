#!/usr/bin/env python3
"""First-paint latency micro-benchmark for the per-speaker refine contour.

Measures wall-clock from `PerSpeakerIngest.enqueue(chunk)` → the moment
the matching `MeetingStorage.append_segment(...)` call returns, i.e. the
"live bubble has hit the transcript" latency. Runs the pipeline twice —
once with the refine contour disabled (no `RefineWorker`, no submit
callback) and once with it fully enabled — so the delta quantifies the
regression introduced by the tier-2 pass running concurrently with
tier-1 under the shared GigaAM `_model_lock`.

Plan reference: docs/plans/2026-04-24-per-speaker-refine-rollout.md §5.
Acceptance gate from design D6 / Open Question 3: p95 regression ≤ 100 ms.

Usage (default — real-time pacing, 4 pids, 30 s per pid):

    venv-gigaam/bin/python scripts/refine_latency_bench.py \
        --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav \
        --trim 17-47

Design notes:

  * The WAV is replayed simultaneously across `--pids N` synthetic pids:
    at each seq step all N pids enqueue their chunk within a few ms, so
    the ingest batches them into a single `transcribe_batch` call. This
    mirrors the concurrent-speaker case where 4 participants emit PCM
    every 3 s (the worst case for live→refine contention).

  * Latency samples are matched per pid FIFO: the n-th
    `append_segment(speaker_id=pid)` pops the n-th enqueue timestamp
    from that pid's deque. For WAVs that produce exactly one segment
    per chunk (the take-2 fixtures, verified in
    `experiments/2026-04-24-refine-wer.md`) this mapping is exact. If a
    chunk produces empty text (dedup / ASR hallucinates nothing) the
    subsequent appends get attributed to slightly older enqueues,
    introducing a small pessimistic bias — flagged in the per-pid
    `append_ratio` column of the report.

  * `--pace realtime` (default) sleeps between seq steps so chunks
    arrive every `chunk_s` seconds, matching production cadence. `--pace
    fast` dumps every chunk immediately — useful for quick iteration on
    pipeline wiring but NOT the acceptance measurement.

The script appends results to `experiments/2026-04-24-refine-wer.md`
under a "Latency" heading; override via `--report-out`.
"""

from __future__ import annotations

import argparse
import os
import statistics
import sys
import tempfile
import threading
import time
import wave
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Deque, Dict, List, Optional, Tuple

import numpy as np

SCRIPTS_DIR = Path(__file__).resolve().parent
PROJECT_ROOT = SCRIPTS_DIR.parent
sys.path.insert(0, str(PROJECT_ROOT))
sys.path.insert(0, str(SCRIPTS_DIR))

# Reuse the trim helper from the older grid-search script so --trim
# semantics are consistent with scripts/refine_benchmark.py.
from pcm_experiment import parse_trim, trim_wav  # type: ignore

from ktalk_bridge import PcmChunk
from meeting_storage import MeetingStorage
from recog.asr import ASRModule
from recog.config import (
    PCM_BATCH_INTERVAL_MS,
    PCM_CHUNK_DURATION_S,
    SPEAKER_TURN_GAP,
)
from recog.live import LiveTranscriber
from recog.per_speaker import PerSpeakerIngest
from recog.refine import RefineWorker


SAMPLE_RATE = 16000


# -----------------------------------------------------------------------------
# Data structures
# -----------------------------------------------------------------------------


@dataclass
class PidLatencies:
    pid: str
    samples_ms: List[float] = field(default_factory=list)
    chunks_enqueued: int = 0
    appends_seen: int = 0


@dataclass
class ConfigResult:
    label: str  # "refine=0" / "refine=1"
    n_samples: int
    p50_ms: float
    p95_ms: float
    max_ms: float
    mean_ms: float
    per_pid: List[PidLatencies]

    @classmethod
    def from_samples(cls, label: str, per_pid: List[PidLatencies]) -> "ConfigResult":
        all_samples = [s for p in per_pid for s in p.samples_ms]
        if not all_samples:
            return cls(label, 0, 0.0, 0.0, 0.0, 0.0, per_pid)
        ordered = sorted(all_samples)
        return cls(
            label=label,
            n_samples=len(ordered),
            p50_ms=float(statistics.median(ordered)),
            p95_ms=_percentile(ordered, 0.95),
            max_ms=float(ordered[-1]),
            mean_ms=float(statistics.mean(ordered)),
            per_pid=per_pid,
        )


def _percentile(sorted_samples: List[float], q: float) -> float:
    if not sorted_samples:
        return 0.0
    # Nearest-rank method (avoids np dependency for the stat — consistent
    # with statistics.median() above).
    k = max(0, min(len(sorted_samples) - 1, int(round(q * (len(sorted_samples) - 1)))))
    return float(sorted_samples[k])


# -----------------------------------------------------------------------------
# Audio loading
# -----------------------------------------------------------------------------


def _load_wav_int16(path: Path) -> np.ndarray:
    """Return mono Int16 PCM at 16 kHz. Reuses the normaliser shape from
    `refine_benchmark.py::_load_wav_int16`; duplicated here to keep the
    script self-contained.
    """
    with wave.open(str(path), "rb") as wf:
        n_channels = wf.getnchannels()
        rate = wf.getframerate()
        width = wf.getsampwidth()
        frames = wf.readframes(wf.getnframes())

    if n_channels == 1 and rate == SAMPLE_RATE and width == 2:
        return np.frombuffer(frames, dtype="<i2").copy()

    with wave.open(str(path), "rb") as wf:
        total_s = wf.getnframes() / rate
    tmp = trim_wav(path, 0.0, total_s)
    try:
        with wave.open(str(tmp), "rb") as wf:
            return np.frombuffer(wf.readframes(wf.getnframes()), dtype="<i2").copy()
    finally:
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass


# -----------------------------------------------------------------------------
# Pipeline construction (mirrors refine_benchmark.py::_build_pipeline)
# -----------------------------------------------------------------------------


def _build_pipeline(
    tmp_path: Path,
    asr: ASRModule,
    refine_enabled: bool,
) -> Tuple[
    LiveTranscriber,
    MeetingStorage,
    PerSpeakerIngest,
    Optional[RefineWorker],
]:
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(mic=False, system_audio=True, ktalk=True)

    t = LiveTranscriber.__new__(LiveTranscriber)
    t.sample_rate = SAMPLE_RATE
    t._state_lock = threading.Lock()
    t._meeting_generation = 0
    t._pcm_live_segments = {}
    t._pcm_segment_texts = {}
    t._pcm_prev_words = {}
    t._pcm_pid_state = {}
    t._meeting_storage = storage
    t._ktalk_bridge = None
    t._wav_lock = threading.Lock()
    t._wav_writer_system = None
    t._wav_writer_mic = None
    t._system_wav_anchor = None
    t._refine_worker = None
    t.transcript_lines = []
    t.asr = asr

    ingest = PerSpeakerIngest(
        transcribe_callback=t._on_pcm_batch,
        generation_getter=lambda: t._meeting_generation,
        meeting_active_getter=lambda: True,
        max_batch_size=8,
        max_chunks_per_pid=4096,
    )
    t._per_speaker_ingest = ingest

    worker: Optional[RefineWorker] = None
    if refine_enabled:
        worker = RefineWorker(
            model_lock=asr._model_lock,
            transcribe_fn=t._refine_transcribe_fn,
            apply_fn=t._apply_refine,
            # Cooperative mode matches production — the aborted-refine
            # re-enqueue path is part of what the 100 ms regression gate
            # is designed to measure.
            abort_mode="cooperative",
        )
        t._refine_worker = worker
        ingest.set_refine_submit_callback(worker.submit)
        worker.start()

    # Start the ingest background tick loop so batches drain every
    # PCM_BATCH_INTERVAL_MS instead of only at _drain_until_idle. Without
    # this the `realtime` pace mode would buffer all chunks and emit
    # them at teardown, giving nonsense multi-second latencies.
    ingest.start()

    return t, storage, ingest, worker


# -----------------------------------------------------------------------------
# Replay driver
# -----------------------------------------------------------------------------


def _iter_chunks(samples: np.ndarray, chunk_samples: int) -> List[np.ndarray]:
    n = len(samples)
    out: List[np.ndarray] = []
    for start in range(0, n, chunk_samples):
        chunk = samples[start : start + chunk_samples]
        if chunk.size >= int(0.1 * SAMPLE_RATE):
            out.append(chunk)
    return out


def _run_config(
    *,
    asr: ASRModule,
    wav_samples: np.ndarray,
    refine_enabled: bool,
    pids: int,
    chunk_s: float,
    duration_s: float,
    pace_realtime: bool,
    tmp_root: Path,
) -> ConfigResult:
    """Run the pipeline for one configuration (refine on/off) and return a
    ConfigResult holding per-chunk first-paint latencies."""
    label = "refine=1" if refine_enabled else "refine=0"

    chunk_samples = int(round(chunk_s * SAMPLE_RATE))
    chunks = _iter_chunks(wav_samples, chunk_samples)
    chunks_needed = max(1, int(round(duration_s / chunk_s)))
    # Loop the WAV if it's shorter than the requested duration so every
    # pid streams exactly chunks_needed × chunk_s worth of audio.
    if len(chunks) < chunks_needed:
        repeat = (chunks_needed // len(chunks)) + 1
        chunks = (chunks * repeat)[:chunks_needed]
    else:
        chunks = chunks[:chunks_needed]

    per_pid: Dict[str, PidLatencies] = {
        f"bench-pid-{i:02d}": PidLatencies(pid=f"bench-pid-{i:02d}")
        for i in range(pids)
    }
    pid_list = list(per_pid.keys())
    enqueue_queues: Dict[str, Deque[float]] = {
        pid: deque() for pid in pid_list
    }
    lock = threading.Lock()

    with tempfile.TemporaryDirectory(dir=tmp_root, prefix="refine_lat_") as d:
        t, storage, ingest, worker = _build_pipeline(Path(d), asr, refine_enabled)

        real_append = storage.append_segment

        def instrumented_append(**kwargs):
            segment_id = real_append(**kwargs)
            append_ts = time.time()
            speaker_id = kwargs.get("speaker_id") or ""
            with lock:
                q = enqueue_queues.get(speaker_id)
                if q:
                    enq_ts = q.popleft()
                    latency_ms = (append_ts - enq_ts) * 1000.0
                    per_pid[speaker_id].samples_ms.append(latency_ms)
                    per_pid[speaker_id].appends_seen += 1
            return segment_id

        storage.append_segment = instrumented_append  # type: ignore[method-assign]

        base_ts = 1_700_000_000.0
        try:
            start_wall = time.time()
            for seq in range(len(chunks)):
                chunk = chunks[seq]
                pcm_bytes = chunk.astype("<i2", copy=False).tobytes()
                for pid in pid_list:
                    with lock:
                        enqueue_queues[pid].append(time.time())
                        per_pid[pid].chunks_enqueued += 1
                    ingest.enqueue(
                        PcmChunk(
                            pid=pid,
                            speaker_name=pid,
                            is_local=False,
                            seq=seq,
                            chunk_start_ts=base_ts + seq * chunk_s,
                            chunk_end_ts=base_ts + (seq + 1) * chunk_s,
                            sample_rate=SAMPLE_RATE,
                            pcm=pcm_bytes,
                        )
                    )
                if pace_realtime:
                    # Sleep until the next seq boundary relative to the
                    # start of the run. Corrects for whatever wall time
                    # the previous iteration's enqueue + drain consumed.
                    target = start_wall + (seq + 1) * chunk_s
                    drift = target - time.time()
                    if drift > 0:
                        time.sleep(drift)
                else:
                    # Still drive one tick so the batcher runs between
                    # seq steps; otherwise all chunks pile into a single
                    # batch of > PCM_MAX_BATCH_SIZE × pids and the
                    # latency distribution is just queue depth, not
                    # representative of production.
                    ingest.tick()

            _drain_until_idle(ingest, worker, deadline_s=45.0)
        finally:
            if worker is not None:
                worker.stop(timeout=5.0)
            ingest.stop()

    return ConfigResult.from_samples(label, list(per_pid.values()))


def _drain_until_idle(
    ingest: PerSpeakerIngest,
    worker: Optional[RefineWorker],
    *,
    deadline_s: float,
) -> None:
    """Flush every pending chunk through the live path plus (if present)
    the refine worker queue. Copy of refine_benchmark._drain_until_idle
    with the worker being optional."""
    deadline = time.time() + deadline_s

    while time.time() < deadline:
        with ingest._lock:  # noqa: SLF001 — benchmark-only peek
            pending = any(buf for buf in ingest._buffers.values())
        if not pending:
            break
        ingest.tick()
        time.sleep(0.01)

    silence_ticks = int(SPEAKER_TURN_GAP * 1000.0 / PCM_BATCH_INTERVAL_MS) + 4
    for _ in range(silence_ticks):
        if time.time() >= deadline:
            break
        ingest.tick()
        time.sleep(PCM_BATCH_INTERVAL_MS / 1000.0 / 4.0)

    if worker is None:
        return
    last_qsize = -1
    stable_polls = 0
    while time.time() < deadline:
        qsize = worker._queue.qsize()  # noqa: SLF001 — benchmark-only peek
        if qsize == 0 and last_qsize == 0:
            stable_polls += 1
            if stable_polls >= 3:
                break
        else:
            stable_polls = 0
        last_qsize = qsize
        time.sleep(0.25)


# -----------------------------------------------------------------------------
# Reporting
# -----------------------------------------------------------------------------


def _print_summary(baseline: ConfigResult, refine: ConfigResult) -> None:
    print()
    print(f"{'config':<10} {'n':>5} {'p50':>8} {'p95':>8} {'max':>8} {'mean':>8}")
    print("-" * 50)
    for row in (baseline, refine):
        print(
            f"{row.label:<10} "
            f"{row.n_samples:>5d} "
            f"{row.p50_ms:>7.1f}ms "
            f"{row.p95_ms:>7.1f}ms "
            f"{row.max_ms:>7.1f}ms "
            f"{row.mean_ms:>7.1f}ms"
        )
    p95_delta = refine.p95_ms - baseline.p95_ms
    p50_delta = refine.p50_ms - baseline.p50_ms
    print("-" * 50)
    print(f"{'Δ p50':<10} {p50_delta:>+7.1f}ms")
    print(f"{'Δ p95':<10} {p95_delta:>+7.1f}ms")
    print(f"{'gate':<10} {'PASS' if p95_delta <= 100.0 else 'MISS'} (target ≤ 100 ms)")


def _append_markdown(
    report_path: Path,
    baseline: ConfigResult,
    refine: ConfigResult,
    *,
    device: str,
    pids: int,
    duration_s: float,
    chunk_s: float,
    pace_realtime: bool,
    wav_name: str,
) -> None:
    report_path.parent.mkdir(parents=True, exist_ok=True)
    created = not report_path.exists()
    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    p95_delta = refine.p95_ms - baseline.p95_ms
    p50_delta = refine.p50_ms - baseline.p50_ms
    gate_pass = p95_delta <= 100.0

    with report_path.open("a", encoding="utf-8") as f:
        if created:
            f.write(
                "# Per-Speaker Refine — WER benchmark\n\n"
                "Generated by `scripts/refine_benchmark.py` and "
                "`scripts/refine_latency_bench.py`.\n"
            )
        f.write(
            f"\n## Latency {stamp}\n\n"
            f"- Device: `{device}`\n"
            f"- Pids: `{pids}` × `{duration_s:.0f}s` "
            f"(chunk={chunk_s:.1f}s, pace={'realtime' if pace_realtime else 'fast'})\n"
            f"- Source WAV: `{wav_name}`\n"
            f"- Target: Δ p95 ≤ 100 ms (design D6)\n\n"
        )
        f.write("| config | n | p50 | p95 | max | mean |\n")
        f.write("|---|---:|---:|---:|---:|---:|\n")
        for row in (baseline, refine):
            f.write(
                f"| `{row.label}` "
                f"| {row.n_samples} "
                f"| {row.p50_ms:.1f} ms "
                f"| {row.p95_ms:.1f} ms "
                f"| {row.max_ms:.1f} ms "
                f"| {row.mean_ms:.1f} ms |\n"
            )
        f.write(
            f"| **Δ refine − baseline** "
            f"|  | **{p50_delta:+.1f} ms** "
            f"| **{p95_delta:+.1f} ms** "
            f"|  |  |\n"
        )
        verdict = "PASS" if gate_pass else "⚠️ MISS"
        f.write(f"\n**Acceptance gate (Δ p95 ≤ 100 ms): {verdict}**\n\n")

        # Per-pid breakdown for sanity-checking the FIFO latency mapping.
        # If `append_ratio < 1.0` on any pid, some chunks produced empty
        # text and the FIFO tail samples carry a small pessimistic bias.
        f.write("<details><summary>Per-pid breakdown</summary>\n\n")
        f.write("| config | pid | chunks | appends | ratio | p50 | p95 | max |\n")
        f.write("|---|---|---:|---:|---:|---:|---:|---:|\n")
        for row in (baseline, refine):
            for p in row.per_pid:
                ratio = (p.appends_seen / p.chunks_enqueued) if p.chunks_enqueued else 0.0
                sorted_s = sorted(p.samples_ms)
                p50 = _percentile(sorted_s, 0.5) if sorted_s else 0.0
                p95 = _percentile(sorted_s, 0.95) if sorted_s else 0.0
                mx = sorted_s[-1] if sorted_s else 0.0
                f.write(
                    f"| `{row.label}` | `{p.pid}` "
                    f"| {p.chunks_enqueued} "
                    f"| {p.appends_seen} "
                    f"| {ratio:.2f} "
                    f"| {p50:.1f} ms "
                    f"| {p95:.1f} ms "
                    f"| {mx:.1f} ms |\n"
                )
        f.write("\n</details>\n\n")


# -----------------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument(
        "--wav",
        required=True,
        help="Source WAV to replay across synthetic pids.",
    )
    ap.add_argument(
        "--trim",
        help="Trim spec, e.g. '17-47' or '0:17-0:47' (see pcm_experiment.py).",
    )
    ap.add_argument("--pids", type=int, default=4, help="Number of parallel pids (default 4).")
    ap.add_argument(
        "--duration-s",
        type=float,
        default=30.0,
        help="Per-pid stream duration in seconds (default 30).",
    )
    ap.add_argument(
        "--chunk-s",
        type=float,
        default=PCM_CHUNK_DURATION_S,
        help=f"Chunk stride in seconds (default {PCM_CHUNK_DURATION_S}).",
    )
    ap.add_argument(
        "--pace",
        choices=("realtime", "fast"),
        default="realtime",
        help=(
            "realtime = sleep between seq steps to match production cadence "
            "(default); fast = enqueue immediately, measures pipeline "
            "throughput not latency."
        ),
    )
    ap.add_argument(
        "--device",
        default=None,
        help="ASR device: cpu/mps/cuda. Default=auto (MPS on Apple Silicon).",
    )
    ap.add_argument(
        "--runs",
        choices=("baseline", "refine", "both"),
        default="both",
        help="Which config(s) to run (default both).",
    )
    ap.add_argument(
        "--report-out",
        default=str(PROJECT_ROOT / "experiments" / "2026-04-24-refine-wer.md"),
        help="Markdown report path; results are APPENDED.",
    )
    args = ap.parse_args()

    # Refine gate is server-authoritative; flip env flags for the refine
    # run and reset for the baseline so downstream `pcm_refine_enabled()`
    # readers (diagnostic imports) see consistent state.
    os.environ["RECOG_PCM_TRANSPORT"] = "1"

    wav_path = Path(args.wav).resolve()
    if not wav_path.exists():
        print(f"WAV not found: {wav_path}", file=sys.stderr)
        return 2

    audio_path = wav_path
    trimmed: Optional[Path] = None
    if args.trim:
        start_s, end_s = parse_trim(args.trim)
        trimmed = trim_wav(wav_path, start_s, end_s)
        audio_path = trimmed

    try:
        wav_samples = _load_wav_int16(audio_path)
    except Exception as e:
        print(f"Failed to load WAV: {e}", file=sys.stderr)
        if trimmed is not None:
            try:
                trimmed.unlink(missing_ok=True)
            except OSError:
                pass
        return 2

    resolved_device = args.device or (
        "mps" if __import__("torch").backends.mps.is_available() else "cpu"
    )
    print(f"Loading GigaAM (device={resolved_device})…", file=sys.stderr)
    asr = ASRModule(device=resolved_device)
    asr.load_model(quiet=True)

    pace_realtime = args.pace == "realtime"

    # Warmup: run a short pipeline to force all lazy MPS kernel
    # compilation / memory allocation so the first measured run does
    # not carry a cold-start bias that would mask the refine delta.
    # Exercises both batch=args.pids (live path) and refine=1
    # (transcribe_with_timestamps) so the subsequent measurement sees
    # only steady-state kernel paths.
    print(
        f"Warming up ({args.pids} pids × 2 chunks, refine path included)…",
        file=sys.stderr,
    )
    with tempfile.TemporaryDirectory(prefix="refine_lat_warmup_") as warmup_dir:
        _run_config(
            asr=asr,
            wav_samples=wav_samples,
            refine_enabled=True,
            pids=args.pids,
            chunk_s=args.chunk_s,
            duration_s=args.chunk_s * 2,
            pace_realtime=False,
            tmp_root=Path(warmup_dir),
        )

    with tempfile.TemporaryDirectory(prefix="refine_lat_session_") as session_dir:
        session_root = Path(session_dir)
        baseline: Optional[ConfigResult] = None
        refine: Optional[ConfigResult] = None

        if args.runs in ("baseline", "both"):
            os.environ["RECOG_PCM_REFINE"] = "0"
            print(
                f"[1/{'2' if args.runs == 'both' else '1'}] baseline "
                f"(refine=0, {args.pids} pids × {args.duration_s:.0f}s)…",
                file=sys.stderr,
            )
            baseline = _run_config(
                asr=asr,
                wav_samples=wav_samples,
                refine_enabled=False,
                pids=args.pids,
                chunk_s=args.chunk_s,
                duration_s=args.duration_s,
                pace_realtime=pace_realtime,
                tmp_root=session_root,
            )

        if args.runs in ("refine", "both"):
            os.environ["RECOG_PCM_REFINE"] = "1"
            prefix = "2/2" if args.runs == "both" else "1/1"
            print(
                f"[{prefix}] refine "
                f"(refine=1, {args.pids} pids × {args.duration_s:.0f}s)…",
                file=sys.stderr,
            )
            refine = _run_config(
                asr=asr,
                wav_samples=wav_samples,
                refine_enabled=True,
                pids=args.pids,
                chunk_s=args.chunk_s,
                duration_s=args.duration_s,
                pace_realtime=pace_realtime,
                tmp_root=session_root,
            )

    if trimmed is not None:
        try:
            trimmed.unlink(missing_ok=True)
        except OSError:
            pass

    if baseline is None or refine is None:
        # Single-run mode; print what we have and skip gate / markdown.
        single = baseline or refine
        assert single is not None
        print()
        print(
            f"{single.label} n={single.n_samples} "
            f"p50={single.p50_ms:.1f}ms p95={single.p95_ms:.1f}ms "
            f"max={single.max_ms:.1f}ms mean={single.mean_ms:.1f}ms"
        )
        return 0

    _print_summary(baseline, refine)
    _append_markdown(
        Path(args.report_out),
        baseline,
        refine,
        device=resolved_device,
        pids=args.pids,
        duration_s=args.duration_s,
        chunk_s=args.chunk_s,
        pace_realtime=pace_realtime,
        wav_name=wav_path.name,
    )
    print(f"\nReport appended to {args.report_out}", file=sys.stderr)

    gate_pass = (refine.p95_ms - baseline.p95_ms) <= 100.0
    return 0 if gate_pass else 1


if __name__ == "__main__":
    sys.exit(main())
