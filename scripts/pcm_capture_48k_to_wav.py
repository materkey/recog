#!/usr/bin/env python3
"""Reconstruct per-pid 48 kHz WAV from a RECOG_PCM_CAPTURE_DIR (48 kHz path).

Mirrors scripts/pcm_capture_reconstruct.py but reads Float32LE `_48k.raw`
files produced when RECOG_PCM_CAPTURE_48K=1. Output is a continuous
16-bit PCM WAV (Float32 normalized then quantized) so it opens in any
player. Overlap windows are dropped the same way as the 16 kHz script.

Usage:
    ./scripts/pcm_capture_48k_to_wav.py \
        --capture-dir /tmp/recog_pcm_capture \
        --meeting-id 2026-04-24_083728_1732cc6e \
        --out-dir /tmp/recog_exp/raw48k
"""
from __future__ import annotations

import argparse
import json
import sys
import wave
from pathlib import Path

import numpy as np


SAMPLE_RATE = 48000
BYTES_PER_SAMPLE = 4  # Float32LE


def _iter_meta(jsonl_path: Path):
    with open(jsonl_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def _meta_sort_key(meta: dict):
    start = float(meta.get("chunk_start_ts", meta.get("chunk_end_ts", 0.0)))
    end = float(meta.get("chunk_end_ts", start))
    byte_offset = int(meta.get("byte_offset", 0))
    seq = int(meta.get("seq", 0))
    return (start, end, byte_offset, seq)


def reconstruct_pid(raw_path: Path, jsonl_path: Path, out_wav: Path) -> dict:
    metas = sorted(_iter_meta(jsonl_path), key=_meta_sort_key)
    if not metas:
        return {"chunk_count": 0, "duration_s": 0.0, "gaps_count": 0}

    speaker = metas[0].get("speaker_name") or metas[0]["pid"]
    gaps = 0
    chunk_count = 0
    written_samples = 0

    anchor_ts = metas[0]["chunk_start_ts"]
    last_end_ts = anchor_ts

    # Collect all Float32 segments in timeline order, then normalize across
    # the whole file before writing — per-chunk normalize would flatten
    # quiet passages into silence noise.
    segments: list[np.ndarray] = []

    with open(raw_path, "rb") as f:
        for meta in metas:
            chunk_count += 1
            chunk_end_ts = meta["chunk_end_ts"]
            f.seek(meta["byte_offset"])
            payload = f.read(meta["num_bytes"])
            samples = np.frombuffer(payload, dtype=np.float32)
            chunk_dur = samples.size / SAMPLE_RATE
            effective_start = chunk_end_ts - chunk_dur
            delta = effective_start - last_end_ts

            if delta > 0.05:
                pad = np.zeros(int(delta * SAMPLE_RATE), dtype=np.float32)
                segments.append(pad)
                written_samples += pad.size
                gaps += 1
                segments.append(samples)
                written_samples += samples.size
            elif delta < -0.001:
                drop = int(-delta * SAMPLE_RATE)
                if drop < samples.size:
                    segments.append(samples[drop:])
                    written_samples += samples.size - drop
            else:
                segments.append(samples)
                written_samples += samples.size

            last_end_ts = chunk_end_ts

    out_wav.parent.mkdir(parents=True, exist_ok=True)
    full = np.concatenate(segments) if segments else np.zeros(0, dtype=np.float32)
    peak = float(np.abs(full).max()) if full.size else 0.0
    i16 = np.clip(full * 32767.0, -32768.0, 32767.0).astype(np.int16)

    with wave.open(str(out_wav), "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(SAMPLE_RATE)
        wf.writeframes(i16.tobytes())

    return {
        "speaker_name": speaker,
        "chunk_count": chunk_count,
        "duration_s": round(written_samples / SAMPLE_RATE, 2),
        "gaps_count": gaps,
        "peak": round(peak, 4),
        "wav_path": str(out_wav),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--capture-dir", required=True)
    ap.add_argument("--meeting-id", required=True)
    ap.add_argument("--out-dir", required=True)
    args = ap.parse_args()

    meeting_dir = Path(args.capture_dir).resolve() / args.meeting_id
    out_dir = Path(args.out_dir).resolve()

    if not meeting_dir.exists():
        print(f"Meeting dir not found: {meeting_dir}", file=sys.stderr)
        return 1

    raws = sorted(meeting_dir.glob("*_48k.raw"))
    if not raws:
        print(f"No *_48k.raw files in {meeting_dir}", file=sys.stderr)
        return 1

    manifest: dict = {
        "meeting_id": args.meeting_id,
        "capture_dir": str(meeting_dir.parent),
        "out_dir": str(out_dir),
        "sample_rate": SAMPLE_RATE,
        "format": "f32le",
        "pids": {},
    }

    for raw_path in raws:
        stem = raw_path.stem  # "<safe_pid>_48k"
        jsonl_path = raw_path.with_suffix(".jsonl")
        if not jsonl_path.exists():
            print(f"skip {stem}: no .jsonl", file=sys.stderr)
            continue
        try:
            first = next(_iter_meta(jsonl_path))
        except StopIteration:
            print(f"skip {stem}: empty .jsonl", file=sys.stderr)
            continue
        safe_name = "".join(
            c if c.isalnum() or c in "-_" else "_"
            for c in (first.get("speaker_name") or first["pid"])
        )
        wav_path = out_dir / f"{stem}__{safe_name}.wav"
        info = reconstruct_pid(raw_path, jsonl_path, wav_path)
        manifest["pids"][stem] = info
        print(
            f"{stem}  {info['speaker_name'][:20]:20s}  "
            f"{info['duration_s']:7.1f}s  "
            f"{info['chunk_count']:3d} chunks  gaps={info['gaps_count']}  "
            f"peak={info['peak']:.3f}  {wav_path.name}"
        )

    manifest_path = out_dir / "manifest.json"
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2, ensure_ascii=False)
    print(f"\nmanifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
