#!/usr/bin/env python3
"""WER harness for the per-speaker refine contour.

Replays per-pid WAVs through the full live + refine pipeline (real
`PerSpeakerIngest`, real `RefineWorker`, real `ASRModule`) and compares
the resulting live-only transcript against the post-refine transcript
on the same reference. Intended to validate the `RECOG_PCM_REFINE`
default-flip acceptance gate from the rollout plan
(docs/plans/2026-04-24-per-speaker-refine-rollout.md §11).

Usage:
    venv-gigaam/bin/python scripts/refine_benchmark.py \
        --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav \
        --wav /tmp/recog_exp/raw_take2/8ee8e2d9__Example2.wav \
        --reference-file experiments/reference_take2.txt \
        --trim 17-65

For each `--wav` the script starts a fresh pipeline (isolated
`MeetingStorage` under a temp dir), slices the audio into 3 s chunks
matching `PCM_CHUNK_DURATION_S`, enqueues them sequentially on a
synthetic pid, and drives the ingest tick loop until the refine worker
has processed every pending window (monolog cap fires every 15 s,
trailing gap triggers the final refine). It then computes WER for:

  * live text  — concatenation of every `append_segment` call, in
    insertion order, captured BEFORE refine can rewrite them.
  * refined text — same sequence with any `update_segments(text=...,
    refined=True)` overrides applied; segments untouched by refine fall
    back to the original live text.

Results are printed as a table and appended to the markdown report at
`experiments/2026-04-24-refine-wer.md` (override via `--report-out`).
Task 3 of the rollout plan is "build only, do not run yet" — running
on real fixtures is Task 4.
"""

from __future__ import annotations

import argparse
import os
import sys
import tempfile
import threading
import time
import wave
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

SCRIPTS_DIR = Path(__file__).resolve().parent
PROJECT_ROOT = SCRIPTS_DIR.parent
sys.path.insert(0, str(PROJECT_ROOT))
sys.path.insert(0, str(SCRIPTS_DIR))

# WER + trim helpers live in the older grid-search script; reuse them
# instead of re-implementing (plan §3.2 explicit).
from pcm_experiment import normalize, parse_trim, trim_wav, wer  # type: ignore

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


@dataclass
class RunResult:
    wav: str
    live_text: str
    refined_text: str
    live_wer: float
    refined_wer: float
    live_br: dict
    refined_br: dict
    segments: int
    refined_segments: int


# -----------------------------------------------------------------------------
# Audio loading
# -----------------------------------------------------------------------------


def _load_wav_int16(path: Path) -> np.ndarray:
    """Return mono Int16 PCM at 16 kHz. ffmpeg-normalises if needed."""
    with wave.open(str(path), "rb") as wf:
        n_channels = wf.getnchannels()
        rate = wf.getframerate()
        width = wf.getsampwidth()
        frames = wf.readframes(wf.getnframes())

    if n_channels == 1 and rate == SAMPLE_RATE and width == 2:
        return np.frombuffer(frames, dtype="<i2").copy()

    # Anything else: ffmpeg-trim into a tempfile at the canonical shape.
    tmp = trim_wav(path, 0.0, wave.open(str(path), "rb").getnframes() / rate)
    try:
        with wave.open(str(tmp), "rb") as wf:
            return np.frombuffer(wf.readframes(wf.getnframes()), dtype="<i2").copy()
    finally:
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass


# -----------------------------------------------------------------------------
# Pipeline construction
# -----------------------------------------------------------------------------


def _build_pipeline(
    tmp_path: Path,
    asr: ASRModule,
) -> Tuple[
    LiveTranscriber,
    MeetingStorage,
    PerSpeakerIngest,
    RefineWorker,
    List[Tuple[int, str]],
    List[Tuple[int, str]],
    threading.Event,
]:
    """Mirror of `tests/test_per_speaker_refine_e2e.py::_build_pipeline`
    but with a real `ASRModule` and no bridge (we don't need the WS
    fan-out for the benchmark — refined text is collected from
    `update_segments` directly).
    """
    storage = MeetingStorage(base_path=tmp_path)
    storage.start_session(mic=False, system_audio=True, ktalk=True)

    append_calls: List[Tuple[int, str]] = []
    update_calls: List[Tuple[int, str]] = []
    refine_event = threading.Event()

    real_append = storage.append_segment

    def recording_append(**kwargs):
        segment_id = real_append(**kwargs)
        append_calls.append((segment_id, kwargs.get("text", "")))
        return segment_id

    storage.append_segment = recording_append  # type: ignore[method-assign]

    real_update = storage.update_segments

    def recording_update(updates, meeting_path=None):
        for u in updates:
            if "text" in u:
                update_calls.append((int(u["segment_id"]), str(u["text"])))
        refine_event.set()
        return real_update(updates, meeting_path=meeting_path)

    storage.update_segments = recording_update  # type: ignore[method-assign]

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

    worker = RefineWorker(
        model_lock=asr._model_lock,
        transcribe_fn=t._refine_transcribe_fn,
        apply_fn=t._apply_refine,
        # Benchmark has no competing live traffic — coarse avoids the
        # decoder monkey-patch overhead and makes per-run timings
        # comparable to pre-refine numbers from pcm_experiment.py.
        abort_mode="coarse",
    )
    t._refine_worker = worker
    ingest.set_refine_submit_callback(worker.submit)
    worker.start()

    return t, storage, ingest, worker, append_calls, update_calls, refine_event


# -----------------------------------------------------------------------------
# Replay loop
# -----------------------------------------------------------------------------


def _iter_chunks(samples: np.ndarray, chunk_samples: int):
    """Yield non-overlapping int16 slices of length <= chunk_samples."""
    n = len(samples)
    for start in range(0, n, chunk_samples):
        yield samples[start : start + chunk_samples]


def _drain_until_idle(
    ingest: PerSpeakerIngest,
    worker: RefineWorker,
    *,
    max_wall_s: float = 600.0,
) -> None:
    """Drive the ingest tick loop + wait for the refine queue to empty.

    Strategy:
      1. Tick until the ingest's per-pid buffers are all empty (live
         path runs synchronously inside tick, so this flushes every
         chunk into MeetingStorage).
      2. Advance synthetic silence by issuing additional ticks (each
         tick bumps `_silence_ms` by `PCM_BATCH_INTERVAL_MS`) until
         the gap trigger (`SPEAKER_TURN_GAP * 1000` ms) fires for
         every pid — this picks up any trailing un-refined audio.
      3. Poll the worker queue + wait for the last apply to settle
         before returning.
    """
    deadline = time.time() + max_wall_s

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


def _run_one(
    wav_path: Path,
    label: str,
    samples: np.ndarray,
    asr: ASRModule,
    tmp_root: Path,
    chunk_s: float,
    ref_words: List[str],
) -> RunResult:
    """Replay one WAV through the pipeline and return a `RunResult`."""
    pid = f"bench-{label}"
    chunk_samples = int(round(chunk_s * SAMPLE_RATE))

    with tempfile.TemporaryDirectory(dir=tmp_root, prefix="refine_bench_") as d:
        t, storage, ingest, worker, append_calls, update_calls, refine_event = (
            _build_pipeline(Path(d), asr)
        )
        try:
            # Enqueue the WAV in sequence order. Use a synthetic
            # monotonic wall-clock so chunk_start_ts is deterministic
            # (matters for the live path's audio_offset_ms derivation).
            base_ts = 1_700_000_000.0
            for seq, chunk in enumerate(_iter_chunks(samples, chunk_samples)):
                if chunk.size < int(0.1 * SAMPLE_RATE):
                    # Tail smaller than 100 ms — GigaAM will emit empty
                    # text; skipping keeps append_calls clean.
                    continue
                pcm_bytes = chunk.astype("<i2", copy=False).tobytes()
                ingest.enqueue(
                    PcmChunk(
                        pid=pid,
                        speaker_name=label,
                        is_local=False,
                        seq=seq,
                        chunk_start_ts=base_ts + seq * chunk_s,
                        chunk_end_ts=base_ts + (seq + 1) * chunk_s,
                        sample_rate=SAMPLE_RATE,
                        pcm=pcm_bytes,
                    )
                )

            _drain_until_idle(ingest, worker)
        finally:
            worker.stop(timeout=5.0)
            ingest.stop()

    # Snapshot collected texts AFTER the worker shuts down so no late
    # refine can mutate the dicts mid-read.
    live_text_parts = [text for _sid, text in append_calls if text.strip()]
    refined_map: Dict[int, str] = {sid: text for sid, text in append_calls}
    for sid, text in update_calls:
        refined_map[sid] = text
    refined_text_parts = [
        refined_map[sid] for sid, _ in append_calls if refined_map[sid].strip()
    ]

    live_text = " ".join(live_text_parts)
    refined_text = " ".join(refined_text_parts)

    live_norm = normalize(live_text)
    refined_norm = normalize(refined_text)

    live_wer, live_br = wer(ref_words, live_norm)
    refined_wer, refined_br = wer(ref_words, refined_norm)

    refined_ids = {sid for sid, _ in update_calls}
    return RunResult(
        wav=wav_path.name,
        live_text=live_text,
        refined_text=refined_text,
        live_wer=live_wer,
        refined_wer=refined_wer,
        live_br=live_br,
        refined_br=refined_br,
        segments=len(append_calls),
        refined_segments=len(refined_ids & {sid for sid, _ in append_calls}),
    )


# -----------------------------------------------------------------------------
# Reporting
# -----------------------------------------------------------------------------


def _print_table(rows: List[RunResult]) -> None:
    header = (
        f"{'wav':<45} {'segs':>5} {'ref':>5} "
        f"{'live WER':>9} {'refined WER':>12} {'Δ':>7}"
    )
    print()
    print(header)
    print("-" * len(header))
    for r in rows:
        print(
            f"{r.wav[:45]:<45} "
            f"{r.segments:>5d} "
            f"{r.refined_segments:>5d} "
            f"{r.live_wer * 100:>8.1f}% "
            f"{r.refined_wer * 100:>11.1f}% "
            f"{(r.refined_wer - r.live_wer) * 100:>+6.1f}%"
        )
    if not rows:
        return
    avg_live = sum(r.live_wer for r in rows) / len(rows)
    avg_refined = sum(r.refined_wer for r in rows) / len(rows)
    print("-" * len(header))
    print(
        f"{'avg':<45} "
        f"{'':>5} {'':>5} "
        f"{avg_live * 100:>8.1f}% "
        f"{avg_refined * 100:>11.1f}% "
        f"{(avg_refined - avg_live) * 100:>+6.1f}%"
    )


def _append_markdown(
    report_path: Path,
    rows: List[RunResult],
    *,
    device: str,
    trim: Optional[str],
    reference_file: str,
) -> None:
    report_path.parent.mkdir(parents=True, exist_ok=True)
    created = not report_path.exists()
    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    with report_path.open("a", encoding="utf-8") as f:
        if created:
            f.write(
                "# Per-Speaker Refine — WER benchmark\n\n"
                "Generated by `scripts/refine_benchmark.py`. Each run "
                "appends a section below; older runs are kept for history.\n"
            )
        f.write(
            f"\n## Run {stamp}\n\n"
            f"- Device: `{device}`\n"
            f"- Trim: `{trim or 'none'}`\n"
            f"- Reference: `{reference_file}`\n\n"
        )
        f.write("| wav | segs | refined | live WER | refined WER | Δ |\n")
        f.write("|---|---:|---:|---:|---:|---:|\n")
        for r in rows:
            f.write(
                f"| `{r.wav}` "
                f"| {r.segments} "
                f"| {r.refined_segments} "
                f"| {r.live_wer * 100:.1f}% "
                f"| {r.refined_wer * 100:.1f}% "
                f"| {(r.refined_wer - r.live_wer) * 100:+.1f}% |\n"
            )
        if rows:
            avg_live = sum(r.live_wer for r in rows) / len(rows)
            avg_refined = sum(r.refined_wer for r in rows) / len(rows)
            f.write(
                f"| **avg** "
                f"|  |  "
                f"| **{avg_live * 100:.1f}%** "
                f"| **{avg_refined * 100:.1f}%** "
                f"| **{(avg_refined - avg_live) * 100:+.1f}%** |\n"
            )
        f.write("\n")
        for r in rows:
            f.write(f"### `{r.wav}`\n\n")
            f.write(
                "<details><summary>Live vs refined (click to expand)</summary>\n\n"
            )
            f.write(f"**Live** ({r.live_br['N']} ref words, edits S/I/D = "
                    f"{r.live_br['S']}/{r.live_br['I']}/{r.live_br['D']}):\n\n"
                    f"> {r.live_text or '_(empty)_'}\n\n")
            f.write(
                f"**Refined** (edits S/I/D = "
                f"{r.refined_br['S']}/{r.refined_br['I']}/{r.refined_br['D']}):\n\n"
                f"> {r.refined_text or '_(empty)_'}\n\n"
            )
            f.write("</details>\n\n")


# -----------------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------------


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument(
        "--wav",
        action="append",
        required=True,
        help="WAV to replay (repeatable; one pipeline run per wav).",
    )
    ap.add_argument(
        "--reference-file",
        required=True,
        help="Path to plain-text ground-truth transcript.",
    )
    ap.add_argument(
        "--trim",
        help="Trim spec, e.g. '17-65' or '0:17-1:05' (see pcm_experiment.py).",
    )
    ap.add_argument(
        "--device",
        default=None,
        help="ASR device: cpu/mps/cuda. Default=auto (MPS on Apple Silicon).",
    )
    ap.add_argument(
        "--chunk-s",
        type=float,
        default=PCM_CHUNK_DURATION_S,
        help=f"Chunk duration in seconds (default: {PCM_CHUNK_DURATION_S}).",
    )
    ap.add_argument(
        "--report-out",
        default=str(PROJECT_ROOT / "experiments" / "2026-04-24-refine-wer.md"),
        help="Markdown report path; results are APPENDED.",
    )
    args = ap.parse_args()

    # Refine gate is server-authoritative; flip the env flags on for this
    # process so `pcm_refine_enabled()` in other modules agrees with what
    # the benchmark is actually exercising (the pipeline here bypasses the
    # gate via direct instantiation, but any downstream diagnostic import
    # that reads the flag will see consistent state).
    os.environ["RECOG_PCM_TRANSPORT"] = "1"
    os.environ["RECOG_PCM_REFINE"] = "1"

    ref_path = Path(args.reference_file)
    if not ref_path.exists():
        print(f"Reference file not found: {ref_path}", file=sys.stderr)
        return 2
    ref_words = normalize(ref_path.read_text(encoding="utf-8"))
    print(
        f"REFERENCE ({len(ref_words)} words): "
        f"{' '.join(ref_words[:20])}{'…' if len(ref_words) > 20 else ''}"
    )

    resolved_device = args.device or (
        "mps"
        if __import__("torch").backends.mps.is_available()
        else "cpu"
    )
    print(f"Loading GigaAM (device={resolved_device})…", file=sys.stderr)
    asr = ASRModule(device=resolved_device)
    asr.load_model(quiet=True)

    rows: List[RunResult] = []
    missing: List[str] = []

    with tempfile.TemporaryDirectory(prefix="refine_bench_session_") as session_dir:
        session_root = Path(session_dir)
        for idx, wav_arg in enumerate(args.wav):
            wav_path = Path(wav_arg).resolve()
            if not wav_path.exists():
                print(f"⚠️  Missing wav: {wav_path} — skipping", file=sys.stderr)
                missing.append(str(wav_path))
                continue

            trimmed_path: Optional[Path] = None
            audio_path = wav_path
            if args.trim:
                start_s, end_s = parse_trim(args.trim)
                trimmed_path = trim_wav(wav_path, start_s, end_s)
                audio_path = trimmed_path

            try:
                samples = _load_wav_int16(audio_path)
                label = f"{idx:02d}-{wav_path.stem}"
                print(
                    f"[{idx + 1}/{len(args.wav)}] {wav_path.name} "
                    f"({samples.size / SAMPLE_RATE:.1f}s, pid={label})",
                    file=sys.stderr,
                )
                result = _run_one(
                    wav_path=wav_path,
                    label=label,
                    samples=samples,
                    asr=asr,
                    tmp_root=session_root,
                    chunk_s=args.chunk_s,
                    ref_words=ref_words,
                )
                rows.append(result)
            finally:
                if trimmed_path is not None:
                    try:
                        trimmed_path.unlink(missing_ok=True)
                    except OSError:
                        pass

    _print_table(rows)

    report_path = Path(args.report_out)
    _append_markdown(
        report_path,
        rows,
        device=resolved_device,
        trim=args.trim,
        reference_file=str(ref_path),
    )
    print(f"\nReport appended to {report_path}", file=sys.stderr)

    if missing:
        print(
            "\n⚠️  Skipped WAVs (not found):\n  - " + "\n  - ".join(missing),
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
