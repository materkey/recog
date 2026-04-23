#!/usr/bin/env python3
"""Offline grid-search for ASR params on captured per-pid WAV files.

Runs GigaAM (and optionally other backends) over a set of
(chunk_duration, overlap) combinations against a reference
transcript, reports WER for each.

Usage:
    ./scripts/pcm_experiment.py \
        --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav \
        --wav /tmp/recog_exp/raw_take2/8ee8e2d9__Example2.wav \
        --trim 17:65 \
        --reference-file /tmp/recog_exp/reference.txt
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import List, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.asr import ASRModule


# -----------------------------------------------------------------------------
# Text normalization + WER
# -----------------------------------------------------------------------------

_NON_WORD_RE = re.compile(r"[^\wа-яА-ЯёЁ\s-]+", re.UNICODE)
_WHITESPACE_RE = re.compile(r"\s+", re.UNICODE)


def normalize(text: str) -> List[str]:
    """Lowercase, strip punctuation, split on whitespace; return word list.

    Keeps Cyrillic + Latin word chars + hyphen (so «ой-ой-ой» stays as one
    token). Common ASR quirks: «ё» ↔ «е» folded, since GigaAM often drops ё.
    """
    text = text.lower().replace("ё", "е")
    text = _NON_WORD_RE.sub(" ", text)
    text = _WHITESPACE_RE.sub(" ", text).strip()
    return text.split()


def wer(ref_words: List[str], hyp_words: List[str]) -> Tuple[float, dict]:
    """Standard WER via Levenshtein edit distance on word sequences.

    Returns (wer, breakdown) where breakdown has subs/ins/del/total ref len.
    """
    n, m = len(ref_words), len(hyp_words)
    # DP table: dp[i][j] = min edits to convert ref[:i] into hyp[:j]
    dp = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n + 1):
        dp[i][0] = i
    for j in range(m + 1):
        dp[0][j] = j
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            if ref_words[i - 1] == hyp_words[j - 1]:
                dp[i][j] = dp[i - 1][j - 1]
            else:
                dp[i][j] = 1 + min(
                    dp[i - 1][j],      # deletion
                    dp[i][j - 1],      # insertion
                    dp[i - 1][j - 1],  # substitution
                )
    # Backtrack to count S/I/D.
    i, j = n, m
    subs = ins = dels = 0
    while i > 0 or j > 0:
        if i > 0 and j > 0 and ref_words[i - 1] == hyp_words[j - 1]:
            i -= 1
            j -= 1
        elif i > 0 and j > 0 and dp[i][j] == dp[i - 1][j - 1] + 1:
            subs += 1
            i -= 1
            j -= 1
        elif i > 0 and dp[i][j] == dp[i - 1][j] + 1:
            dels += 1
            i -= 1
        else:
            ins += 1
            j -= 1
    total_edits = subs + ins + dels
    w = total_edits / max(n, 1)
    return w, {"S": subs, "I": ins, "D": dels, "N": n, "edits": total_edits}


# -----------------------------------------------------------------------------
# Audio utilities
# -----------------------------------------------------------------------------

def parse_trim(spec: str) -> Tuple[float, float]:
    """Accept '17:65' or '0:17-1:05' or '17-65' — return (start_s, end_s)."""
    spec = spec.strip()
    if "-" in spec and ":" in spec:
        a, b = spec.split("-", 1)
        return _to_seconds(a), _to_seconds(b)
    if ":" in spec:
        a, b = spec.split(":", 1)
        # Could be HH:MM:SS or SS:SS shorthand. Treat as (start, end) if two plain ints.
        if a.count(":") == 0 and b.count(":") == 0:
            return float(a), float(b)
        return _to_seconds(a), _to_seconds(b)
    if "-" in spec:
        a, b = spec.split("-", 1)
        return float(a), float(b)
    raise ValueError(f"Cannot parse trim spec: {spec!r}")


def _to_seconds(t: str) -> float:
    parts = t.split(":")
    if len(parts) == 1:
        return float(parts[0])
    if len(parts) == 2:
        return int(parts[0]) * 60 + float(parts[1])
    if len(parts) == 3:
        return int(parts[0]) * 3600 + int(parts[1]) * 60 + float(parts[2])
    raise ValueError(f"Bad time spec: {t!r}")


def trim_wav(wav_path: Path, start_s: float, end_s: float) -> Path:
    """ffmpeg-trim wav to a tempfile; return its path. Caller removes it."""
    tmp = Path(tempfile.mkstemp(suffix=".wav")[1])
    subprocess.run(
        [
            "ffmpeg", "-y", "-loglevel", "error",
            "-ss", f"{start_s:.3f}",
            "-to", f"{end_s:.3f}",
            "-i", str(wav_path),
            "-ac", "1", "-ar", "16000",
            "-c:a", "pcm_s16le",
            str(tmp),
        ],
        check=True,
    )
    return tmp


# -----------------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--wav", action="append", required=True, help="WAV to process (repeatable)")
    ap.add_argument("--trim", help="Trim spec, e.g. '17-65' or '0:17-1:05'")
    ap.add_argument("--reference-file", help="Path to text file with ground-truth transcript")
    ap.add_argument("--reference", help="Ground-truth transcript directly on CLI (alternative to --reference-file)")
    ap.add_argument(
        "--configs",
        default="3.0:0.3,3.0:0.6,5.0:1.0,10.0:1.0,20.0:1.0",
        help="Comma-separated chunk_s:overlap_s pairs",
    )
    args = ap.parse_args()

    # Load reference
    if args.reference_file:
        ref_text = Path(args.reference_file).read_text()
    elif args.reference:
        ref_text = args.reference
    else:
        print("Either --reference-file or --reference required", file=sys.stderr)
        return 1
    ref_words = normalize(ref_text)
    print(f"REFERENCE ({len(ref_words)} words): {' '.join(ref_words[:30])}{'…' if len(ref_words) > 30 else ''}")
    print()

    # Parse configs
    configs = []
    for c in args.configs.split(","):
        chunk, overlap = c.strip().split(":")
        configs.append((float(chunk), float(overlap)))

    # Trim inputs if asked
    trimmed_inputs: List[Tuple[str, Path]] = []
    for w in args.wav:
        wav_path = Path(w).resolve()
        if args.trim:
            start_s, end_s = parse_trim(args.trim)
            trimmed = trim_wav(wav_path, start_s, end_s)
            trimmed_inputs.append((wav_path.name, trimmed))
        else:
            trimmed_inputs.append((wav_path.name, wav_path))

    # Load ASR once
    print("Loading GigaAM model…", file=sys.stderr)
    asr = ASRModule()
    asr.load_model(quiet=True)

    # Header
    print(f"{'config':<14} {'file':<42} {'WER':>7} {'S':>4} {'I':>4} {'D':>4} {'N':>4}  transcript")
    print("-" * 140)

    try:
        for chunk_s, overlap_s in configs:
            for wav_label, path in trimmed_inputs:
                segs = asr.transcribe_simple(
                    str(path),
                    chunk_duration=chunk_s,
                    overlap=overlap_s,
                    quiet=True,
                )
                hyp_text = " ".join(s.text for s in segs)
                hyp_words = normalize(hyp_text)
                w, br = wer(ref_words, hyp_words)
                tag = f"{chunk_s}/{overlap_s}"
                preview = " ".join(hyp_words)
                if len(preview) > 80:
                    preview = preview[:77] + "…"
                print(
                    f"{tag:<14} {wav_label[:42]:<42} "
                    f"{w*100:>6.1f}% {br['S']:>4} {br['I']:>4} {br['D']:>4} {br['N']:>4}  {preview}"
                )
    finally:
        # Cleanup trimmed tmpfiles
        for label, tmp in trimmed_inputs:
            if args.trim and tmp.exists() and str(tmp).startswith("/var/") or str(tmp).startswith("/tmp/"):
                try:
                    if label != tmp.name:  # only delete if we created it
                        tmp.unlink(missing_ok=True)
                except Exception:
                    pass

    return 0


if __name__ == "__main__":
    sys.exit(main())
