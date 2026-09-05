# Refine abort-hook spike — GigaAM RNN-T decoder

**Date:** 2026-04-24
**Plan:** `docs/plans/2026-04-24-per-speaker-refine-backend.md` — Task 2
**Question:** Can we interrupt `model.decoding.decode()` mid-flight so that the
live ASR path never waits more than a bounded fraction of the refine window
for the shared model lock? If yes, what is the expected abort latency?

## TL;DR

- **Yes** — the greedy RNN-T decoder has a natural poll point at the outer
  `for t in range(T):` loop in `gigaam/decoding.py:184`. A 40-line
  monkey-patched `decode()` that raises `RefineAborted` when a
  `should_abort()` closure returns True works end-to-end on MPS.
- **Abort latency (measured):** ~**187 ms** median on MPS (float16) for a
  30 s speech sample. That floor is the encoder forward pass — it is a single
  PyTorch op and cannot be pre-empted with a Python-level hook.
- **Savings vs. coarse mode:** ~**767 ms per aborted refine call**
  (~**80 % of full inference**). The greedy RNN-T decoder dominates
  end-to-end cost for 30 s windows on MPS, so early-exit really matters.
- **Decision:** default `RefineWorker.abort_mode = "cooperative"`. The
  implementation complexity (copying ~40 lines of upstream loop) is justified
  by the >4× reduction in worst-case live-path wait time.

## Can we hook the decoder step loop?

### Evidence — upstream code structure

`gigaam/decoding.py::RNNTGreedyDecoding.decode` (post-upstream merge, commit
`6e4b027`) is built around two nested loops:

```python
# gigaam/decoding.py:184-207 (paraphrased)
for t in range(T):                      # ← outer: encoder time steps
    active = (t < enc_len).nonzero(...).tolist()
    if not active:
        break
    for _ in range(self.max_symbols):   # ← inner: symbol emissions per step
        ...
        next_active = emit_batch(...)
        if not next_active:
            break
        active = next_active
```

Both loops are regular Python `for` statements with no C/CUDA kernels bridging
them. Polling a `should_abort()` callable at the top of the outer loop is
safe (no torch state that must be freed) and requires a single-line insertion:

```python
for t in range(T):
    if should_abort():
        raise RefineAborted(f"abort@t={t}/{T}")
    ...
```

Polling the inner symbol loop is possible but offers no extra latency savings
— a single time step with `max_symbols=10` inner iterations takes <1 ms.

### Evidence — runtime behaviour

The spike script `scripts/refine_abort_spike.py` monkey-patches
`model.decoding.decode` with an abortable variant whose body mirrors the
upstream loop exactly (bytewise-identical logic, one extra check). Running
with `should_abort = lambda: True` raises `RefineAborted` at `t=0/T` and
unwinds cleanly; the caller catches the exception and resumes normal control.

## Measured latencies (MPS, float16, 30 s audio)

Ran on: `speech_reference.wav` (30 s), M-series Mac, MPS backend, fp16,
1 warmup + 3 measured trials per scenario.

| Scenario | median (ms) | all trials (ms) |
|---|---|---|
| Baseline — encoder + decoder | **953.9** | 961.9 / 953.5 / 953.9 |
| Encoder only (no decoder) | **187.2** | 184.9 / 187.4 / 187.2 |
| Cooperative abort at `t=0` | **187.4** | 187.4 / 186.5 / 188.0 |

Derived:

- Decoder loop cost = `baseline − encoder_only` = **~767 ms** (~80 % of
  baseline). RNN-T greedy decoding is the dominant cost on this stack, not
  the encoder forward.
- Cooperative worst-case abort latency = encoder forward only = **~187 ms**.
  The hook cannot pre-empt `model.forward` itself — it is a single PyTorch
  op that runs until the MPS kernels finish.
- Coarse worst-case abort latency = full inference = **~954 ms**.

Raw numbers in `docs/experiments/2026-04-24-refine-abort-spike.json`.

### Scaling note

For a 20 s window (the configured `PCM_REFINE_WINDOW_S`), we expect the ratios
to hold (encoder and decoder both scale roughly linearly with audio length).
Extrapolating: ~130 ms encoder floor, ~510 ms decoder loop, ~640 ms full —
so cooperative saves ~510 ms / window-abort on the target window size.

## Monkey-patch design (prototype)

The prototype lives in `scripts/refine_abort_spike.py:43-110`.
Key shape:

```python
def make_abortable_decode(decoding, should_abort):
    tokenizer = decoding.tokenizer
    blank_id = decoding.blank_id
    max_symbols = decoding.max_symbols
    cat_states = decoding._cat_states
    split_state = decoding._split_state

    @torch.inference_mode()
    def decode(head, encoded, enc_len):
        x = encoded.transpose(1, 2)
        B, T, _ = x.shape
        device = x.device
        # ... hyps / token_frames / last_label / dec_state init, verbatim ...
        enc_len_cpu = enc_len.cpu()
        for t in range(T):
            if should_abort():
                raise RefineAborted(f"abort@t={t}/{T}")
            active = (t < enc_len_cpu).nonzero(as_tuple=False).squeeze(1).tolist()
            if not active:
                break
            for _ in range(max_symbols):
                # ... inner body verbatim ...
        return [(tokenizer.decode(h), h, tf) for h, tf in zip(hyps, token_frames)]

    return decode
```

**Integration shape** (deferred to Task 4):

```python
# in recog/refine.py or recog/asr.py
def transcribe_with_abort(asr, pcm, should_abort):
    model = asr.load_model(quiet=True)
    abortable = make_abortable_decode(model.decoding, should_abort)
    original = model.decoding.decode
    try:
        model.decoding.decode = abortable.__get__(model.decoding)  # or just: = abortable
        with asr._model_lock:
            return model.transcribe(..., word_timestamps=True)
    finally:
        model.decoding.decode = original
```

Alternative (cleaner): subclass `RNNTGreedyDecoding`, override `decode()`,
and replace `model.decoding` once at load time with an instance that carries
a `self._should_abort` attr the worker can toggle.

## Limitations / risks

1. **Encoder is not interruptible.** ~187 ms floor on MPS / 30 s. If live
   triggers abort right after `model.forward` starts, it still waits for the
   encoder to finish. Acceptable for UX (one-off <200 ms stall per abort).
2. **Monkey-patch copies ~40 lines of upstream decoder code.** Upstream API
   drift will silently desynchronise the copy. Mitigate by: (a) pinning
   `gigaam` to the known-good commit; (b) adding a CI assertion that the
   upstream `RNNTGreedyDecoding.decode` source hash matches the snapshot we
   vendored; (c) re-running this spike after any gigaam bump.
3. **Exception-based control flow** across a `with _model_lock:` block: the
   `try/finally` in the worker must release the lock before re-enqueueing
   (plan Task 4.3 — matches this).
4. **CTC-head models not covered.** `v3_e2e_rnnt` is our default, so the
   RNN-T hook is sufficient. If we ever switch to a CTC head, the shorter
   `CTCGreedyDecoding.decode` (no Python step loop — pure tensor ops) is
   already near-instant; coarse mode is fine there.

## Recommendation

**Default `abort_mode = "cooperative"`.** Benefits:

- 4–5× reduction in worst-case live-path wait time (from ~950 ms to ~190 ms
  on MPS).
- Implementation cost is self-contained (one helper in `recog/refine.py`,
  one `finally` block around `model.decoding.decode`).
- `coarse` remains available as an opt-in fallback (constructor kwarg) for
  hosts where the monkey-patch misbehaves (e.g., ONNX runtime path, where
  `decoding.py` is bypassed entirely).

## Reproduce

```bash
./venv-gigaam/bin/python scripts/refine_abort_spike.py
# → prints timings, writes docs/experiments/2026-04-24-refine-abort-spike.json
```
