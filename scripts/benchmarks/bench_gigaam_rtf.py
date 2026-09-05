"""GigaAM Real-Time Factor benchmark.

Measures how fast GigaAM (v3_e2e_rnnt) transcribes on the current machine.
Answers three questions that drive the per-speaker pipeline architecture:

    1. Single-stream RTF — can one instance keep up with realtime?
    2. Concurrency scaling — what happens with N threads hitting the same
       loaded model? (Does PyTorch release GIL here?)
    3. Batched inference — if we group N chunks into one forward() call,
       how does per-sample latency drop?

Plus: memory footprint of one loaded model, chunk-latency at realistic
chunk sizes (1s / 2s).

Output:
    Human-readable table to stdout, plus a JSON dump to
    /tmp/bench_gigaam_rtf.json for later programmatic use.

Usage:
    ./venv-gigaam/bin/python scripts/benchmarks/bench_gigaam_rtf.py
"""
from __future__ import annotations

import json
import os
import statistics
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import psutil
import sys
import torch

PROJECT_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(PROJECT_ROOT))
# GigaAM checkout supplies example.wav; override with GIGAAM_DIR when the
# clone does not sit next to this repo.
GIGAAM_DIR = Path(os.environ.get("GIGAAM_DIR") or PROJECT_ROOT.parent / "GigaAM")
AUDIO_SHORT = GIGAAM_DIR / "example.wav"                             # 11.3s
AUDIO_60S = PROJECT_ROOT / ".local/examples/vlad_sample.wav"                         # 60s
AUDIO_30S = PROJECT_ROOT / "tests/e2e/fixtures/speech_reference.wav" # 30s

CONCURRENCY_LEVELS = (1, 2, 3, 4)
BATCH_SIZES = (1, 2, 4, 8)
CHUNK_DURATIONS_S = (1.0, 2.0, 4.0)
TRIALS = 3
WARMUP_CALLS = 2


def rss_mb() -> float:
    return psutil.Process().memory_info().rss / 1024 / 1024


def median_min_max(xs: list[float]) -> dict[str, float]:
    return {
        "median": round(statistics.median(xs), 4),
        "min": round(min(xs), 4),
        "max": round(max(xs), 4),
    }


def _require_audio() -> None:
    """Skip (under pytest) or bail out (as a script) when bench audio is absent.

    None of these files are in the repo: example.wav lives in the GigaAM
    checkout, vlad_sample.wav is a personal recording.
    """
    missing = [p for p in (AUDIO_SHORT, AUDIO_60S, AUDIO_30S) if not p.exists()]
    if not missing:
        return
    reason = (
        "bench audio missing: "
        + ", ".join(str(p) for p in missing)
        + " (set GIGAAM_DIR to a GigaAM checkout containing example.wav)"
    )
    if "pytest" in sys.modules:
        import pytest
        pytest.skip(reason, allow_module_level=True)
    raise SystemExit(f"[bench] {reason}")


def run():
    results: dict = {}

    print(f"[bench] audio_short: {AUDIO_SHORT}")
    print(f"[bench] audio_60s:   {AUDIO_60S}")
    print(f"[bench] audio_30s:   {AUDIO_30S}")
    _require_audio()

    from gigaam.preprocess import SAMPLE_RATE, load_audio
    from recog.asr import ASRModule

    # -------- load model + record memory --------
    rss_before = rss_mb()
    t_load_0 = time.perf_counter()
    asr = ASRModule()
    model = asr.load_model(quiet=True)
    t_load_1 = time.perf_counter()
    rss_after = rss_mb()
    device = str(model._device)
    dtype = str(model._dtype)
    results["model"] = {
        "name": asr.model_name,
        "device": device,
        "dtype": dtype,
        "load_seconds": round(t_load_1 - t_load_0, 2),
        "rss_before_mb": round(rss_before, 1),
        "rss_after_mb": round(rss_after, 1),
        "rss_delta_mb": round(rss_after - rss_before, 1),
    }
    print(f"[bench] device={device} dtype={dtype} load={t_load_1 - t_load_0:.1f}s "
          f"rss={rss_before:.0f}→{rss_after:.0f}MB (Δ{rss_after - rss_before:.0f}MB)")

    # Pre-load short audio tensor (reused across batched tests)
    wav_short = load_audio(str(AUDIO_SHORT))
    short_duration = wav_short.shape[0] / SAMPLE_RATE
    print(f"[bench] short audio duration: {short_duration:.2f}s")

    # -------- warmup --------
    print("[bench] warmup...")
    for _ in range(WARMUP_CALLS):
        model.transcribe(str(AUDIO_SHORT))

    # -------- 1. Single-stream RTF on 11s --------
    print(f"\n[bench] Scenario 1: single-stream RTF on {short_duration:.1f}s audio")
    wall_times = []
    for _ in range(TRIALS):
        t0 = time.perf_counter()
        model.transcribe(str(AUDIO_SHORT))
        wall_times.append(time.perf_counter() - t0)
    stats = median_min_max(wall_times)
    rtf = round(stats["median"] / short_duration, 4)
    results["single_stream_short"] = {
        "audio_duration_s": short_duration,
        "wall_seconds": stats,
        "rtf": rtf,
        "max_parallel_realtime": round(1.0 / rtf, 2) if rtf > 0 else None,
    }
    print(f"  wall={stats['median']:.2f}s  RTF={rtf}  → theoretically {1/rtf:.1f}× realtime")

    # -------- 1b. Single-stream RTF on 60s (longform path) --------
    print(f"\n[bench] Scenario 1b: longform RTF on 60s audio")
    wall_times = []
    for _ in range(max(2, TRIALS - 1)):
        t0 = time.perf_counter()
        asr.transcribe_longform(str(AUDIO_60S))
        wall_times.append(time.perf_counter() - t0)
    stats = median_min_max(wall_times)
    rtf_long = round(stats["median"] / 60.0, 4)
    results["single_stream_60s"] = {
        "audio_duration_s": 60.0,
        "wall_seconds": stats,
        "rtf": rtf_long,
    }
    print(f"  wall={stats['median']:.2f}s  RTF={rtf_long}")

    # -------- 2. Chunk latency (realistic per-participant chunks) --------
    print(f"\n[bench] Scenario 2: chunk latency (one forward per chunk, serial)")
    wav_60 = load_audio(str(AUDIO_60S))
    chunk_latencies: dict[str, dict] = {}
    for chunk_s in CHUNK_DURATIONS_S:
        chunk_samples = int(chunk_s * SAMPLE_RATE)
        sample_chunk = wav_60[:chunk_samples]
        wav_tensor = sample_chunk.to(model._device).to(model._dtype).unsqueeze(0)
        length = torch.full([1], chunk_samples, device=model._device)
        # warmup
        with torch.inference_mode():
            enc, enc_len = model.forward(wav_tensor, length)
            _ = model.decoding.decode(model.head, enc, enc_len)
        latencies = []
        for _ in range(TRIALS):
            t0 = time.perf_counter()
            with torch.inference_mode():
                enc, enc_len = model.forward(wav_tensor, length)
                _ = model.decoding.decode(model.head, enc, enc_len)
            latencies.append(time.perf_counter() - t0)
        stats = median_min_max(latencies)
        chunk_rtf = round(stats["median"] / chunk_s, 4)
        chunk_latencies[f"{chunk_s}s"] = {
            "wall": stats,
            "rtf": chunk_rtf,
            "realtime_ok": stats["median"] < chunk_s,
        }
        print(f"  chunk={chunk_s:.0f}s  latency={stats['median']*1000:.0f}ms  RTF={chunk_rtf}  "
              f"{'[ok]' if stats['median'] < chunk_s else '[LATE]'}")
    results["chunk_latency"] = chunk_latencies

    # -------- 3. Thread concurrency on shared model --------
    print(f"\n[bench] Scenario 3: N threads, shared model, each transcribes 11s audio")
    concurrency = {}
    for n in CONCURRENCY_LEVELS:
        # warmup one
        model.transcribe(str(AUDIO_SHORT))
        walls = []
        for _ in range(TRIALS):
            t0 = time.perf_counter()
            with ThreadPoolExecutor(max_workers=n) as ex:
                futures = [ex.submit(model.transcribe, str(AUDIO_SHORT)) for _ in range(n)]
                for f in futures:
                    f.result()
            walls.append(time.perf_counter() - t0)
        stats = median_min_max(walls)
        per_call = round(stats["median"] / 1, 4)  # total wall == per-call wall since all run concurrently
        total_audio = n * short_duration
        throughput_rtf = round(stats["median"] / total_audio, 4)
        concurrency[f"n={n}"] = {
            "wall_total": stats,
            "total_audio_s": total_audio,
            "throughput_rtf": throughput_rtf,
            "streams_sustained": round(total_audio / stats["median"], 2),
        }
        print(f"  n={n}  wall={stats['median']:.2f}s  total_audio={total_audio:.0f}s  "
              f"→ sustained {total_audio/stats['median']:.1f}× realtime")
    results["concurrency"] = concurrency

    # -------- 4. Batched forward on shared model --------
    print(f"\n[bench] Scenario 4: batched forward (B wavs → 1 forward call)")
    batched = {}
    for B in BATCH_SIZES:
        wav_tensor = wav_short.to(model._device).to(model._dtype).unsqueeze(0).expand(B, -1).contiguous()
        length = torch.full([B], wav_short.shape[-1], device=model._device)
        # warmup
        with torch.inference_mode():
            enc, enc_len = model.forward(wav_tensor, length)
            _ = model.decoding.decode(model.head, enc, enc_len)
        walls = []
        for _ in range(TRIALS):
            t0 = time.perf_counter()
            with torch.inference_mode():
                enc, enc_len = model.forward(wav_tensor, length)
                _ = model.decoding.decode(model.head, enc, enc_len)
            walls.append(time.perf_counter() - t0)
        stats = median_min_max(walls)
        per_sample = round(stats["median"] / B, 4)
        per_sample_rtf = round(per_sample / short_duration, 4)
        total_audio = B * short_duration
        throughput_rtf = round(stats["median"] / total_audio, 4)
        batched[f"B={B}"] = {
            "wall_total": stats,
            "per_sample_wall": per_sample,
            "per_sample_rtf": per_sample_rtf,
            "batched_throughput_rtf": throughput_rtf,
            "streams_sustained": round(total_audio / stats["median"], 2),
        }
        print(f"  B={B}  wall={stats['median']:.2f}s  per-sample={per_sample*1000:.0f}ms  "
              f"RTF={per_sample_rtf}  → sustained {total_audio/stats['median']:.1f}× realtime")
    results["batched"] = batched

    # -------- final summary --------
    print("\n[bench] === SUMMARY ===")
    short_rtf = results["single_stream_short"]["rtf"]
    print(f"  Device: {device} ({dtype})")
    print(f"  Memory: {results['model']['rss_delta_mb']} MB for one loaded model")
    print(f"  Single stream: RTF={short_rtf} → one instance ≈ {1/short_rtf:.1f}× realtime")
    best_thread = max(concurrency.values(), key=lambda r: r["streams_sustained"])
    print(f"  Best concurrency: {best_thread['streams_sustained']}× realtime across threads")
    best_batch = max(batched.values(), key=lambda r: r["streams_sustained"])
    print(f"  Best batched: {best_batch['streams_sustained']}× realtime (batched forward)")

    out_json = Path("/tmp/bench_gigaam_rtf.json")
    out_json.write_text(json.dumps(results, indent=2, ensure_ascii=False))
    print(f"\n[bench] results → {out_json}")


if __name__ == "__main__":
    run()
