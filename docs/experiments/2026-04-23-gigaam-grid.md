# GigaAM grid-search results (take-2)

**Date**: 2026-04-23
**Material**: `/tmp/recog_exp/raw_take2/{d9e189e5__Example,8ee8e2d9__Example2}.wav`
**Trim**: 17-65 s (48 s of speech)
**Reference**: 4× зайчик-погулять, 92 words (`docs/experiments/reference_take2.txt`)
**Model**: GigaAM v3_e2e_rnnt (unchanged across all runs)

## Methodology

Two reconstructed per-pid WAVs (16 kHz mono 16-bit, no gaps) from a
real KTalk meeting where one user spoke into two browser tabs
simultaneously. Two independent Opus encode + jitter-buffer paths ≈
two near-identical streams. Trimmed to the speech window, normalised
by `normalize()` (lowercase, ё→е, punctuation stripped), scored
against the reference by word-level Levenshtein (S/I/D/WER).

Harness: `scripts/benchmarks/pcm_experiment.py`. Supports `--configs
chunk:overlap,…`, `--no-dedup`, `--longform` (pyannote VAD),
`--device cpu|mps`, `--trim start-end`.

## Full results (dedup enabled, CPU)

| Config (chunk/overlap s) | Example WER | Example2 WER | Avg | Notes |
|---|---|---|---|---|
| 2/0.0 | 35.9% | 35.9% | 35.9% | Too short — drops context |
| 3/0.0 | 37.0% | 35.9% | 36.5% | No overlap, too short |
| 3/0.3 (old prod) | 32.6% | 31.5% | 32.1% | |
| **3/0.6 (current prod)** | **33.7%** | **32.6%** | **33.2%** | |
| 3/1.0 | 35.9% | 34.8% | 35.4% | Overlap too wide, dedup drops more |
| 5/0.0 | 26.1% | 25.0% | 25.6% | **No overlap, surprisingly strong** |
| 5/0.5 | 31.5% | 30.4% | 31.0% | |
| 5/1.0 | 33.7% | 32.6% | 33.2% | |
| 5/2.0 | 44.6% | 45.7% | 45.2% | Dedup struggles |
| 10/0.0 | 25.0% | 25.0% | 25.0% | |
| 🥉 10/0.5 | 25.0% | 23.9% | **24.5%** | **Best live-latency compromise** |
| 10/1.0 | 29.3% | 28.3% | 28.8% | |
| 🥇 10/2.0 | 27.2% | **22.8%** | 25.0% | Abs min on one track |
| 10/5.0 | 58.7% | 60.9% | 59.8% | Overlap too wide, cascades |
| 20/0.0 | 23.9% | 25.0% | 24.5% | |
| 🥈 **20/1.0** | **23.9%** | **23.9%** | **23.9%** | **Most stable, avg minimum** |
| 30/0.0 | 26.1% | 25.0% | 25.6% | |
| full/0 (one-shot 48 s) | 27.2% | 27.2% | 27.2% | GigaAM not trained on this length |
| longform (pyannote VAD) | 29.3% | 29.3% | 29.3% | Worse than large fixed chunks |

## Dedup ablation (`--no-dedup`)

| Config | With dedup | Without dedup | Δ |
|---|---|---|---|
| 3/0.3 | 32.1% | 33.2% | +1% |
| 3/0.6 | 33.2% | **46.7%** | **+13%** |
| 5/1.0 | 33.2% | 44.1% | +11% |
| 10/1.0 | 28.8% | 32.1% | +3% |
| 20/1.0 | 23.9% | 26.1% | +2% |

**Takeaway**: `_remove_duplicate_words` is critical whenever `overlap ≥ 0.5 s`.
Disabling it on high-overlap configs produces duplicate words and balloons
insertions. At `overlap = 0` it's a no-op.

## MPS (Metal) run

Same WER as CPU (bit-identical transcripts on tested configs 10/0.5, 20/1.0,
10/2.0). Useful purely for throughput.

## Interpretation

- **WER floor on this material is ~22-24 %**. Given the input is
  double-Opus-encoded near-identical streams of the same speaker, the
  residual errors cluster around known weak spots (`пиф-паф` onomatopoeia,
  deletion of `раз` at repetition boundaries, substitution of the same
  word pairs). These are model-level, not pipeline-level.
- **Current prod (3/0.6) is ~10% absolute WER worse** than the best
  achievable configs. There's headroom without changing the model.
- **`_remove_duplicate_words` dedup is net-positive** and cannot be
  disabled at today's overlap.
- **pyannote VAD longform does not help** here — GigaAM's internal context
  dominates over boundary alignment on this material.
- **Full-file one-shot is worse than 20 s chunks**. GigaAM seems trained
  on ~20 s windows — that's the sweet spot.

## Paths forward

### Option A — bump chunk duration in prod (1-line change)
Switch `PCM_CHUNK_DURATION_S` from 3.0 → 10.0, `PCM_CHUNK_OVERLAP_MS`
from 600 → 500. Live WER drops ~8% abs. UI first-line latency rises
from ~3 s to ~10 s. Trade-off is user-visible.

### Option B — stream + refine (architectural)
Keep the live 3 s pipeline as-is (instant feedback at 33% WER).
Add a parallel 20 s buffer that retranscribes on a slower cadence and
**replaces** the UI line with the corrected text when ready. Drops
final WER to ~24% without live-latency cost. Requires changes in
`recog/live.py`, `recog/ktalk_bridge.py` (new message type `transcript_replace`),
and the frontend WebSocket handler + transcript.jsonl format.

**Decision**: pursue Option B.

## Reproducing

```bash
./scripts/benchmarks/pcm_experiment.py \
  --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav \
  --wav /tmp/recog_exp/raw_take2/8ee8e2d9__Example2.wav \
  --trim 17-65 \
  --reference-file docs/experiments/reference_take2.txt \
  --configs "2:0,3:0,3:0.3,3:0.6,3:1,5:0,5:0.5,5:1,5:2,10:0,10:0.5,10:1,10:2,10:5,20:0,20:1,30:0,full:0"
```

Add `--no-dedup` to reproduce dedup ablation. Add `--device mps` to run
on Metal. Use `--longform` to try pyannote-VAD mode.
