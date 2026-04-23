#!/usr/bin/env python3
"""Reconstruct per-pid continuous WAV files from a RECOG_PCM_CAPTURE_DIR.

Input layout (written by ktalk_bridge._capture_pcm_chunk):
    <capture_dir>/<meeting_id>/<safe-pid>.raw      # Int16LE samples, all chunks concatenated (with overlap)
    <capture_dir>/<meeting_id>/<safe-pid>.jsonl    # metadata per chunk: pid, seq, chunk_start_ts, chunk_end_ts, byte_offset, num_bytes

Output:
    <out_dir>/<pid>__<speaker>.wav            # 16 kHz mono 16-bit PCM, continuous timeline
    <out_dir>/manifest.json                   # pid → {speaker_name, duration_s, wav_path, chunk_count, gaps_count}

The reconstruction rebuilds a single continuous timeline per pid by
taking the NEW portion of each chunk (the tail after the previous
chunk's end timestamp), padding real gaps with silence, and discarding
already-seen overlap samples. This gives a clean, concatenation-ready
audio file suitable for running any ASR model or custom experiment on.

Usage:
    ./scripts/pcm_capture_reconstruct.py \
        --capture-dir /tmp/recog_pcm_capture \
        --meeting-id 2026-04-23_111322_2bff8971 \
        --out-dir /tmp/recog_pcm_capture/experiments/raw
"""
from __future__ import annotations

import argparse
import json
import struct
import sys
import wave
from pathlib import Path


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


def _read_raw_slice(raw_path: Path, offset: int, n: int) -> bytes:
    with open(raw_path, "rb") as f:
        f.seek(offset)
        return f.read(n)


def _meta_sort_key(meta: dict) -> tuple[float, float, int, int]:
    """Sort chunks by capture timeline; seq can reset after track reconnect."""
    chunk_start_ts = float(meta.get("chunk_start_ts", meta.get("chunk_end_ts", 0.0)))
    chunk_end_ts = float(meta.get("chunk_end_ts", chunk_start_ts))
    byte_offset = int(meta.get("byte_offset", 0))
    seq = int(meta.get("seq", 0))
    return (chunk_start_ts, chunk_end_ts, byte_offset, seq)


def reconstruct_pid(raw_path: Path, jsonl_path: Path, out_wav: Path) -> dict:
    metas = sorted(_iter_meta(jsonl_path), key=_meta_sort_key)
    if not metas:
        return {"chunk_count": 0, "duration_s": 0.0, "gaps_count": 0}

    sample_rate = metas[0]["sample_rate"]
    bytes_per_sample = 2

    speaker = metas[0].get("speaker_name") or metas[0]["pid"]
    gaps = 0
    chunk_count = 0
    written_samples = 0

    # Anchor timeline at the first chunk's start. Ignore wall-clock
    # offsets — we want a continuous audio clip covering only the span
    # of captured data for this pid.
    anchor_ts = metas[0]["chunk_start_ts"]
    last_end_ts = anchor_ts

    out_wav.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(out_wav), "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(bytes_per_sample)
        wf.setframerate(sample_rate)

        for meta in metas:
            chunk_count += 1
            chunk_start_ts = meta["chunk_start_ts"]
            chunk_end_ts = meta["chunk_end_ts"]
            payload = _read_raw_slice(
                raw_path,
                offset=meta["byte_offset"],
                n=meta["num_bytes"],
            )
            samples_in_chunk = len(payload) // bytes_per_sample
            chunk_actual_duration = samples_in_chunk / sample_rate

            # Where does THIS chunk's content actually start, given its
            # declared end and its known length? The extension uses
            # end = now, start = end - payload_s, but we trust the end
            # (timestamped at emit) more than the start.
            effective_start_ts = chunk_end_ts - chunk_actual_duration

            # Gap vs overlap vs exact continuation from last_end_ts:
            delta = effective_start_ts - last_end_ts
            if delta > 0.05:  # real gap ≥ 50 ms → pad with silence
                gap_samples = int(delta * sample_rate)
                silence = b"\x00\x00" * gap_samples
                wf.writeframes(silence)
                written_samples += gap_samples
                gaps += 1
                new_segment = payload
            elif delta < -0.001:  # overlap → drop the duplicate prefix
                overlap_samples = int(-delta * sample_rate)
                drop_bytes = overlap_samples * bytes_per_sample
                new_segment = payload[drop_bytes:] if drop_bytes < len(payload) else b""
            else:
                new_segment = payload

            wf.writeframes(new_segment)
            written_samples += len(new_segment) // bytes_per_sample
            last_end_ts = chunk_end_ts

    return {
        "speaker_name": speaker,
        "chunk_count": chunk_count,
        "duration_s": round(written_samples / sample_rate, 2),
        "gaps_count": gaps,
        "wav_path": str(out_wav),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--capture-dir", required=True, help="Directory passed to RECOG_PCM_CAPTURE_DIR")
    ap.add_argument("--meeting-id", required=True, help="Meeting id subdirectory under capture-dir")
    ap.add_argument("--out-dir", required=True, help="Where to write reconstructed WAVs")
    args = ap.parse_args()

    capture_dir = Path(args.capture_dir).resolve()
    meeting_dir = capture_dir / args.meeting_id
    out_dir = Path(args.out_dir).resolve()

    if not meeting_dir.exists():
        print(f"Meeting dir not found: {meeting_dir}", file=sys.stderr)
        return 1

    raws = sorted(meeting_dir.glob("*.raw"))
    if not raws:
        print(f"No .raw files in {meeting_dir}", file=sys.stderr)
        return 1

    manifest: dict = {
        "meeting_id": args.meeting_id,
        "capture_dir": str(capture_dir),
        "out_dir": str(out_dir),
        "pids": {},
    }

    for raw_path in raws:
        pid = raw_path.stem
        jsonl_path = raw_path.with_suffix(".jsonl")
        if not jsonl_path.exists():
            print(f"skip {pid}: no .jsonl", file=sys.stderr)
            continue
        # Read first meta to grab speaker_name for filename.
        try:
            first = next(_iter_meta(jsonl_path))
        except StopIteration:
            print(f"skip {pid}: empty .jsonl", file=sys.stderr)
            continue
        safe_name = "".join(
            c if c.isalnum() or c in "-_" else "_"
            for c in (first.get("speaker_name") or pid)
        )
        wav_path = out_dir / f"{pid}__{safe_name}.wav"
        info = reconstruct_pid(raw_path, jsonl_path, wav_path)
        manifest["pids"][pid] = info
        print(
            f"{pid}  {info['speaker_name'][:20]:20s}  "
            f"{info['duration_s']:6.1f}s  "
            f"{info['chunk_count']:3d} chunks  gaps={info['gaps_count']}  "
            f"{wav_path.name}"
        )

    manifest_path = out_dir / "manifest.json"
    with open(manifest_path, "w") as f:
        json.dump(manifest, f, indent=2, ensure_ascii=False)
    print(f"\nmanifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
