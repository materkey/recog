"""Spike: cooperative abort hook for GigaAM RNN-T decoder (Task 2 of
2026-04-24-per-speaker-refine-backend plan).

Goal: determine whether we can interrupt `model.decoding.decode()` mid-flight
when the live path wants the shared model lock, and measure the resulting
abort latency vs. letting refine run to completion ("coarse" mode).

Approach:
  1. Time baseline `model.forward` + `model.decoding.decode` on a speech sample.
  2. Monkey-patch decoding.decode with a copy that polls a `should_abort()`
     closure at the top of the outer time-step loop and raises `RefineAborted`
     on True.
  3. Time the abort path: encoder forward completes, decoder raises at t=0.
     Difference vs. baseline = "cooperative savings" (decoder loop cost).

Run:
  ./venv-gigaam/bin/python scripts/refine_abort_spike.py
"""
from __future__ import annotations

import statistics
import sys
import time
from pathlib import Path

import torch

PROJECT_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_ROOT))

AUDIO_30S = PROJECT_ROOT / "tests/e2e/fixtures/speech_reference.wav"
TRIALS = 3
WARMUP = 1


class RefineAborted(Exception):
    """Raised by the patched decoder when should_abort() returns True."""


def make_abortable_decode(decoding, should_abort):
    """Return a drop-in replacement for RNNTGreedyDecoding.decode that polls
    should_abort() between encoder time steps.

    The body mirrors gigaam/decoding.py::RNNTGreedyDecoding.decode verbatim
    except for the added `if should_abort(): raise RefineAborted()` at the top
    of the outer `for t in range(T):` loop.
    """
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

        hyps = [[] for _ in range(B)]
        token_frames = [[] for _ in range(B)]
        last_label = [None] * B
        dec_state = [None] * B

        def emit_batch(batch_idx, t, fresh):
            idx = torch.tensor(batch_idx, device=device, dtype=torch.long)
            f = x[idx, t:t + 1, :]
            if fresh:
                g, hidden = head.decoder.predict(None, None, batch_size=len(batch_idx))
            else:
                labels = torch.cat([last_label[i] for i in batch_idx], dim=0)
                state = cat_states([dec_state[i] for i in batch_idx])
                g, hidden = head.decoder.predict(labels, state, batch_size=len(batch_idx))

            k = head.joint.joint(f, g)[:, 0, 0, :].argmax(dim=-1)
            emit = k.ne(blank_id)
            if not emit.any():
                return []

            hidden_parts = split_state(hidden)
            out = []
            for p in emit.nonzero(as_tuple=False).squeeze(1).tolist():
                bi = batch_idx[p]
                tok = int(k[p])
                hyps[bi].append(tok)
                token_frames[bi].append(t)
                last_label[bi] = k[p:p + 1].view(1, 1)
                dec_state[bi] = hidden_parts[p]
                out.append(bi)
            return out

        enc_len_cpu = enc_len.cpu()
        for t in range(T):
            if should_abort():
                raise RefineAborted(f"abort@t={t}/{T}")
            active = (t < enc_len_cpu).nonzero(as_tuple=False).squeeze(1).tolist()
            if not active:
                break
            for _ in range(max_symbols):
                if not active:
                    break
                fresh = [i for i in active if dec_state[i] is None]
                stateful = [i for i in active if dec_state[i] is not None]

                next_active = []
                if fresh:
                    next_active.extend(emit_batch(fresh, t, fresh=True))
                if stateful:
                    next_active.extend(emit_batch(stateful, t, fresh=False))

                if not next_active:
                    break
                active = next_active

        return [(tokenizer.decode(h), h, tf) for h, tf in zip(hyps, token_frames)]

    return decode


def sync(device):
    if device.type == "mps":
        torch.mps.synchronize()
    elif device.type == "cuda":
        torch.cuda.synchronize()


def main():
    from gigaam.preprocess import SAMPLE_RATE, load_audio

    from recog.asr import ASRModule

    print(f"[spike] audio: {AUDIO_30S}")
    assert AUDIO_30S.exists(), AUDIO_30S

    asr = ASRModule()
    t0 = time.perf_counter()
    model = asr.load_model(quiet=False)
    t_load = time.perf_counter() - t0
    print(f"[spike] device={model._device} dtype={model._dtype} load={t_load:.2f}s")

    wav = load_audio(str(AUDIO_30S))
    duration_s = wav.shape[0] / SAMPLE_RATE
    wav_tensor = wav.to(model._device).to(model._dtype).unsqueeze(0)
    length = torch.full([1], wav.shape[-1], device=model._device)
    print(f"[spike] audio duration: {duration_s:.2f}s")

    # warmup
    for _ in range(WARMUP):
        with torch.inference_mode():
            enc, elen = model.forward(wav_tensor, length)
            _ = model.decoding.decode(model.head, enc, elen)
        sync(model._device)

    # --- baseline: encoder + full decoder loop ---
    baseline_ms = []
    for _ in range(TRIALS):
        t0 = time.perf_counter()
        with torch.inference_mode():
            enc, elen = model.forward(wav_tensor, length)
            _ = model.decoding.decode(model.head, enc, elen)
        sync(model._device)
        baseline_ms.append((time.perf_counter() - t0) * 1000)

    # --- encoder-only (no decoder) ---
    encoder_only_ms = []
    for _ in range(TRIALS):
        t0 = time.perf_counter()
        with torch.inference_mode():
            enc, elen = model.forward(wav_tensor, length)
        sync(model._device)
        encoder_only_ms.append((time.perf_counter() - t0) * 1000)

    # --- cooperative abort: patched decoder raises at t=0 ---
    def always_abort():
        return True

    abortable_decode = make_abortable_decode(model.decoding, always_abort)
    abort_immediate_ms = []
    for _ in range(TRIALS):
        t0 = time.perf_counter()
        try:
            with torch.inference_mode():
                enc, elen = model.forward(wav_tensor, length)
                _ = abortable_decode(model.head, enc, elen)
        except RefineAborted as e:
            sync(model._device)
            abort_immediate_ms.append((time.perf_counter() - t0) * 1000)
        else:
            raise AssertionError("expected RefineAborted to fire")

    # --- cooperative abort: flip flag AFTER encoder, measure abort-from-decode-start ---
    # Same shape as abort_immediate, but asserts we actually entered the patched decode.

    def med(xs):
        return statistics.median(xs)

    base_med = med(baseline_ms)
    enc_med = med(encoder_only_ms)
    abort_med = med(abort_immediate_ms)
    dec_delta = base_med - enc_med

    print("\n[spike] === results ===")
    print(f"  baseline (encoder + decoder): median={base_med:.1f}ms  all={[f'{x:.1f}' for x in baseline_ms]}")
    print(f"  encoder-only:                 median={enc_med:.1f}ms  all={[f'{x:.1f}' for x in encoder_only_ms]}")
    print(f"  cooperative-abort-at-t=0:     median={abort_med:.1f}ms  all={[f'{x:.1f}' for x in abort_immediate_ms]}")
    print(f"  decoder loop cost (baseline - encoder_only): ~{dec_delta:.1f}ms"
          f"  ({100 * dec_delta / base_med:.0f}% of baseline)")
    print(f"  cooperative worst-case abort latency: ~{enc_med:.0f}ms (encoder forward cannot be interrupted)")
    print(f"  coarse worst-case abort latency:      ~{base_med:.0f}ms (run to completion)")
    print(f"  savings by going cooperative: ~{dec_delta:.0f}ms per aborted refine call")

    # summary JSON for the markdown report
    import json
    summary = {
        "device": str(model._device),
        "dtype": str(model._dtype),
        "audio_duration_s": round(duration_s, 2),
        "trials": TRIALS,
        "baseline_ms": baseline_ms,
        "encoder_only_ms": encoder_only_ms,
        "cooperative_abort_ms": abort_immediate_ms,
        "baseline_median_ms": round(base_med, 1),
        "encoder_only_median_ms": round(enc_med, 1),
        "cooperative_abort_median_ms": round(abort_med, 1),
        "decoder_loop_median_ms": round(dec_delta, 1),
        "decoder_loop_pct_of_baseline": round(100 * dec_delta / base_med, 1),
    }
    out = PROJECT_ROOT / "docs/experiments/2026-04-24-refine-abort-spike.json"
    out.write_text(json.dumps(summary, indent=2, ensure_ascii=False))
    print(f"\n[spike] summary → {out}")


if __name__ == "__main__":
    main()
