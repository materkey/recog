# Rust runtimes for GigaAM v3 e2e_rnnt

Step 1 of [the move to Rust](../plans/2026-11-07-rust-migration.md). The question is
whether a Rust runtime can replace recog's PyTorch GigaAM, so that Python can go
entirely.

**Outcome.** On macOS, yes: `wavo` 0.4.0 on the Metal GPU.

- Its WER matches PyTorch within 0.06 points.
- It disagrees with PyTorch on 0.3-0.9 % of the words; `gigastt`, the other runtime,
  on 1-3 %.
- It returns the same word timings.
- It is 6× faster on the live 3 s windows and 12× faster on refine windows. A 20 s
  refine takes 60-100 ms, which makes the decoder abort hook unnecessary.

On Linux, not decided: the Z270XX GPU was not measured (below).

## Runtimes

| | `wavo` 0.4.0 | `gigastt-core` 2.22.0 | today |
|---|---|---|---|
| Model | `gigaam-v3` = e2e_rnnt, GGUF Q8_0 | `e2e_rnnt`, ONNX, INT8 encoder | `v3_e2e_rnnt`, PyTorch |
| Engine | pure Rust, wgpu: Metal, Vulkan; Core ML for the Neural Engine | ONNX Runtime: CPU, Core ML, CUDA | torch: MPS, CUDA, CPU |
| Word timings | token start times (40 ms frames) | words with start, end and confidence | `transcribe(word_timestamps=True)` |
| Batch of clips | none | none (a pool of sessions) | one padded forward |
| Abort of a decode under way | none: only between the pieces of a long recording | cooperative, at token/frame boundaries; not during the encoder | cooperative decoder hook (`make_abortable_decode`) |
| Licence | MIT | MIT | MIT |
| Age on 2026-11-07 | crate one month old, 5 releases | 7 months, 32 releases | — |

`gigaam-v3-runtime`, named in the plan's sources, is not on crates.io.

## Method

The harness is in `scripts/benchmarks/rust_asr/`.

**Recordings corpus** (`prep.py`, `sources.example.json`): windows in the shapes recog
feeds the model, with silent windows dropped below recog's VAD threshold.

- `.local/examples` (36 minutes) and 15 minutes each of the microphone and system track
  of one meeting.
- 1512 live windows of 3 s every 2.5 s (75.6 minutes).
- 207 refine windows of 20 s (68.1 minutes).
- The texts stay local. Only the aggregate numbers below are recorded.

**Labelled corpus** (`prep_labelled.py`): utterances of 1 to 25 s, 600 utterances,
155.7 minutes in all.

- 300 from ToneWebinars validation: webinar lectures, punctuated references,
  Apache-2.0.
- 300 from FLEURS ru_ru test: read sentences, CC-BY-4.0.
- Golos is left out: GigaAM was trained on it.

Agreement with PyTorch shows only how close a runtime is to today's model, not how
accurate it is. Both models can be wrong together, and a runtime can be right where
PyTorch is not. Accuracy is the WER against these checked references. A few minutes of
our own meetings transcribed by hand would be the better test; that needs the user.

**Reference** (`ref.py`): recog's own `ASRModule`.

- Live windows go through `transcribe_chunk`, then `transcribe_batch` in groups of 8
  (`PCM_MAX_BATCH_SIZE`).
- Refine windows go through `transcribe_with_timestamps`.
- The Rust runtimes run the same windows through `harness/` (`asrbench`).
- A group of 8 is eight calls in a row there, since neither runtime batches.

**Abort**: a decode of each of the 10 longest refine windows is asked to stop 100 ms in.
The figure is the time from that request until the call returns.

**Scoring** (`score.py`):
- WER on words, lowercased, ё→е, punctuation dropped;
- RTF (real-time factor), the processing time divided by the audio duration;
- peak RSS (resident set size, physical memory taken) from `/usr/bin/time`.

**Hosts.**
- MacBook Pro M3 Pro, macOS 27.0.1. The live service ran its ASR remotely during the
  runs, so the GPU was free and the CPU lightly loaded.
- Z270XX: 32 cores, CPU only, `nice`, 8 threads.

## Results: Mac

Recordings corpus; agreement is with PyTorch on MPS:

| run | live p50 ms | live RTF | 8 clips ms | refine p50 ms | refine RTF | peak RSS MB | abort ms | words differing, live / refine % | refine text identical % | word start Δ p95 ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| PyTorch MPS | 126 | 0.043 | 339 | 832 | 0.043 | 2078 | — | — | — | — |
| wavo Metal | 22 | 0.0074 | 176 | 96 | 0.0058 | 1107 | none | 0.7-0.9 / 0.6-0.8 | 76 | 0 |
| wavo Neural Engine | 31 | 0.010 | 247 | — | 0.0037 | 842 | none | 1.8-2.3 / 6.2-7.2 | 8 | 40 |
| gigastt Core ML | 108 | 0.038 | 873 | 583 | 0.030 | 1215 | 452 | 2.3-2.8 / 2.0-2.8 | 43 | 40 |
| gigastt CPU, 1 thread | 266 | 0.090 | 2151 | — | 0.090 | 1183 | 1631 | same as Core ML | 43 | 40 |

Labelled corpus, WER %:

| run | FLEURS ru | ToneWebinars | refine RTF | refine p50 ms | words differing from PyTorch % |
|---|---:|---:|---:|---:|---:|
| PyTorch MPS | 4.21 | 14.02 | 0.051 | 734 | — |
| wavo Metal | 4.22 | 14.08 | 0.0042 | 61 | 0.27 |
| wavo Neural Engine | 4.31 | 14.70 | 0.0036 | — | 3.13 |
| gigastt Core ML | 4.22 | 14.10 | 0.038 | — | 1.07 |
| gigastt CPU, 6 threads | 4.22 | 14.10 | 0.029 | — | 1.07 |

- ToneWebinars' 14 % is the same for every runtime, PyTorch included, so it comes from
  that set (its audio or how its references are written), not from a runtime. I did
  not look further.
- The labelled "live" figures are left out: that shape holds 8 long utterances there,
  not 3 s windows.

## Results: Z270XX, CPU

Labelled corpus, 8 threads:

| run | FLEURS ru | ToneWebinars | refine RTF | peak RSS MB | abort ms |
|---|---:|---:|---:|---:|---:|
| PyTorch CPU | 4.21 | 14.03 | 0.020 | 2843 | — |
| gigastt CPU | 4.28 | 14.08 | 0.022 | 1559 | 397 |

The GPU was not measured: vLLM holds 90 of its 98 GB and the remote GigaAM backend that
serves the live service sits on the rest. The CUDA build of the harness is ready
(`--features wavo,gigastt,cuda`). `wavo` has no CPU backend; on Linux it needs Vulkan.

## What it means for the plan

- **Quality.** `wavo` on Metal is today's model to within quantization noise. Its WER
  differs from PyTorch's by 0.01-0.06 points, and the timings are identical. The
  Neural Engine trades about 0.7 WER points on webinars for little speed: it runs a
  fixed 15 s window, so refine windows are cut at pauses. Use the GPU backend.
- **Refine abort.** It exists because a refine takes ~800 ms on MPS. With a 60-100 ms
  refine, waiting for it to finish costs less than today's abort latency (~190 ms,
  `2026-04-24-refine-abort-spike.md`). No abort hook is needed, and `wavo` has none.
- **Batching.** It is not needed: eight clips in a row on `wavo` take half the time of
  one batched PyTorch forward.
- **Memory.** Peak memory falls from 2.1 GB to 1.1 GB, and torch leaves the
  dependencies.
- **Risk.** `wavo` is a month old, with one maintainer and 87 downloads. It is MIT and
  pure Rust, so pin it exactly, keep its source vendored or forked, and repeat this run
  before any upgrade. `gigastt` is older and has a real abort, but it is slower than
  PyTorch on the Mac and its INT8 encoder drifts further (1-3 % of words).
- **Linux** stays on PyTorch until a GPU run on Z270XX compares `wavo` (Vulkan) with
  `gigastt` (CUDA). On CPU, `gigastt` matches PyTorch at about half the memory.

## Reproduce

```bash
B=scripts/benchmarks/rust_asr
(cd $B/harness && cargo build --release --features wavo,gigastt,coreml)   # cuda on Linux
gigastt download --model-variant e2e_rnnt --model-dir .local/rust_asr/gigastt
venv-gigaam/bin/python $B/prep.py my-sources.json .local/rust_asr/corpus   # from sources.example.json
python $B/prep_labelled.py .local/rust_asr/corpus-labelled 300             # needs `datasets`
GIGASTT_MODEL_DIR=.local/rust_asr/gigastt $B/run.sh .local/rust_asr/corpus .local/rust_asr/out
```
