#!/usr/bin/env python3
"""Micro-benchmark: ASRModule.transcribe_batch wall-clock on CPU vs MPS.

Loads one ASRModule per device, synthesises B ∈ {1, 4, 8} copies of a
10-second 16 kHz silence PCM buffer, and times ``transcribe_batch``
end-to-end (preprocess + pad + forward + decode).  On MPS we bracket
each iteration with ``torch.mps.synchronize()`` since Metal ops are
async; on CPU the call blocks, so ``time.perf_counter()`` alone is
enough.

Writes a markdown report to stdout.  Intended invocation:

    venv-gigaam/bin/python scripts/benchmarks/mps_batch_bench.py \\
        > docs/experiments/2026-04-24-mps-batch.md
"""
from __future__ import annotations

import argparse
import statistics
import sys
import time
from pathlib import Path
from typing import Dict, List, Optional

import numpy as np
import torch

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from recog.asr import ASRModule


SAMPLE_RATE = 16000
CLIP_SECONDS = 10
BATCH_SIZES = (1, 4, 8)
WARMUP_ITERS = 1
MEASURE_ITERS = 5


def synthesize_batch(batch_size: int) -> List[np.ndarray]:
    """B copies of a 10-second float32 silence buffer."""
    clip = np.zeros(SAMPLE_RATE * CLIP_SECONDS, dtype=np.float32)
    return [clip.copy() for _ in range(batch_size)]


def sync(device: str) -> None:
    if device == "mps":
        torch.mps.synchronize()
    elif device == "cuda":
        torch.cuda.synchronize()


def bench_device(device: str) -> List[Dict[str, object]]:
    """Run the sweep on a single device; return per-batch-size stats."""
    print(f"[bench] loading ASRModule(device={device})…", file=sys.stderr)
    asr = ASRModule(device=device)
    asr.load_model(quiet=True)

    # Global warmup on smallest batch so graph/allocator overhead for the
    # very first forward doesn't contaminate B=1.
    asr.transcribe_batch(synthesize_batch(1))
    sync(device)

    rows: List[Dict[str, object]] = []
    for B in BATCH_SIZES:
        batch = synthesize_batch(B)

        # Per-shape warmup: some backends recompile when tensor shape changes.
        for _ in range(WARMUP_ITERS):
            asr.transcribe_batch(batch)
            sync(device)

        timings: List[float] = []
        for _ in range(MEASURE_ITERS):
            sync(device)
            t0 = time.perf_counter()
            asr.transcribe_batch(batch)
            sync(device)
            timings.append(time.perf_counter() - t0)

        rows.append({
            "device": device,
            "B": B,
            "timings_s": timings,
            "median_s": statistics.median(timings),
            "mean_s": statistics.fmean(timings),
            "min_s": min(timings),
            "max_s": max(timings),
        })
        print(
            f"[bench] device={device} B={B} "
            f"median={rows[-1]['median_s']*1000:.1f} ms "
            f"min={rows[-1]['min_s']*1000:.1f} ms "
            f"max={rows[-1]['max_s']*1000:.1f} ms",
            file=sys.stderr,
        )

    return rows


def render_report(
    cpu_rows: List[Dict[str, object]],
    mps_rows: Optional[List[Dict[str, object]]],
) -> str:
    lines: List[str] = []
    lines.append("# MPS vs CPU batched-forward micro-bench")
    lines.append("")
    lines.append(
        f"Configuration: {CLIP_SECONDS} s of 16 kHz silence per sample, "
        f"{WARMUP_ITERS} warmup + {MEASURE_ITERS} measured iterations per "
        f"(device, B)."
    )
    lines.append(
        "Timing bracket: `torch.mps.synchronize()` + `time.perf_counter()` "
        "on MPS, plain `perf_counter` on CPU."
    )
    lines.append("")

    lines.append("## Raw wall-clock (ms)")
    lines.append("")
    lines.append("| device | B | median | mean | min | max |")
    lines.append("|--------|---|--------|------|-----|-----|")

    def _fmt(row: Dict[str, object]) -> str:
        return (
            f"| {row['device']} | {row['B']} | "
            f"{row['median_s']*1000:.1f} | "
            f"{row['mean_s']*1000:.1f} | "
            f"{row['min_s']*1000:.1f} | "
            f"{row['max_s']*1000:.1f} |"
        )

    for row in cpu_rows:
        lines.append(_fmt(row))
    if mps_rows is not None:
        for row in mps_rows:
            lines.append(_fmt(row))
    lines.append("")

    if mps_rows is not None:
        lines.append("## Speedup (MPS vs CPU, median)")
        lines.append("")
        lines.append("| B | cpu (ms) | mps (ms) | mps / cpu | speedup (cpu / mps) |")
        lines.append("|---|----------|----------|-----------|---------------------|")
        cpu_by_b = {r["B"]: r for r in cpu_rows}
        for row in mps_rows:
            B = row["B"]
            cpu_ms = cpu_by_b[B]["median_s"] * 1000
            mps_ms = row["median_s"] * 1000
            ratio = mps_ms / cpu_ms if cpu_ms > 0 else float("nan")
            speedup = cpu_ms / mps_ms if mps_ms > 0 else float("nan")
            lines.append(
                f"| {B} | {cpu_ms:.1f} | {mps_ms:.1f} | "
                f"{ratio:.2f}× | {speedup:.2f}× |"
            )
        lines.append("")

        # Acceptance line (plan Task 7.3): MPS wall ≤ CPU wall at B=8.
        cpu_b8 = cpu_by_b[8]["median_s"] * 1000
        mps_b8 = [r for r in mps_rows if r["B"] == 8][0]["median_s"] * 1000
        verdict = "PASS" if mps_b8 <= cpu_b8 else "FAIL (MPS slower)"
        lines.append(
            f"Acceptance (MPS ≤ CPU at B=8): **{verdict}** "
            f"— CPU {cpu_b8:.1f} ms vs MPS {mps_b8:.1f} ms."
        )
        lines.append("")
    else:
        lines.append(
            "⚠️ MPS unavailable on this host "
            "(`torch.backends.mps.is_available()` returned False). "
            "Only CPU numbers reported."
        )
        lines.append("")

    # Per-iteration detail for transparency.
    lines.append("## Per-iteration timings (ms)")
    lines.append("")
    lines.append("| device | B | iterations |")
    lines.append("|--------|---|------------|")
    all_rows = list(cpu_rows) + (mps_rows or [])
    for row in all_rows:
        fmt_timings = ", ".join(f"{t*1000:.1f}" for t in row["timings_s"])
        lines.append(f"| {row['device']} | {row['B']} | {fmt_timings} |")
    lines.append("")

    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--skip-mps",
        action="store_true",
        help="Force CPU-only run even if MPS is available (debug).",
    )
    args = ap.parse_args()

    cpu_rows = bench_device("cpu")

    mps_rows: Optional[List[Dict[str, object]]] = None
    if args.skip_mps:
        print("[bench] --skip-mps: skipping MPS bench", file=sys.stderr)
    elif not torch.backends.mps.is_available():
        print(
            "[bench] torch.backends.mps.is_available() == False; "
            "skipping MPS bench",
            file=sys.stderr,
        )
    else:
        mps_rows = bench_device("mps")

    sys.stdout.write(render_report(cpu_rows, mps_rows))
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
