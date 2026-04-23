# GigaAM Upstream Merge + MPS Backend

**Date**: 2026-04-24
**Status**: Ready for ralphex execution
**Reference OpenSpec**: `openspec/changes/per-speaker-stream-refine/` — this plan covers §0 (Pre-flight) and §1 (MPS) of that change. The refine contour (§2–§12) is a separate follow-up plan.

## Overview

Prerequisite phase for `per-speaker-stream-refine` (Option B, stream+refine). Consume upstream GigaAM `origin/main` to unlock word-level timestamps (PR #65, commit `d427e5a`) and the batched RNN-T decoder (PR #67, commit `a755fec`), then enable MPS (Metal) backend on Apple Silicon. The refine contour cannot be built without these — timestamps are the architectural foundation of 1:1 segment replacement (see `openspec/changes/per-speaker-stream-refine/design.md` §D1 "Timeline contract").

## Context (from discovery)

- Upstream GigaAM lives at `/Users/vkkovalev/projects/GigaAM` and is editable-installed into `venv-gigaam/` via `pip install -e ../GigaAM`. Current local main is at `0a3f103`; upstream `origin/main` is at `6e4b027` (9 commits ahead).
- BREAKING upstream change in commit `a755fec`: `model.decoding.decode(...)` now returns `List[Tuple[str, List[int], List[int]]]` (text + token_ids + token_frames) instead of `List[str]`. Our `recog/asr.py::ASRModule.transcribe_batch` calls this directly at one known site and will crash without a patch. Public signature `transcribe_batch(pcm_list) -> List[str]` must stay stable so callers in `recog/live.py::_on_pcm_batch` are unaffected.
- New API: `gigaam.load_model(...).transcribe(path, word_timestamps=True)` returns a `TranscriptionResult` with `.text` and `.words` (list of `Word(text, start, end)`). We'll wrap this as `ASRModule.transcribe_with_timestamps`.
- Grid-search baseline: `experiments/2026-04-23-gigaam-grid.md`. Configs 20/1.0 = 23.9% WER and 10/0.5 = 24.5% WER on take-2 reference (`experiments/reference_take2.txt`, 48 s speech window trim 17-65). Post-merge WER must stay within ±1% of these.
- Per-pid WAV fixtures exist at `/tmp/recog_exp/raw_take2/{d9e189e5__Example.wav, 8ee8e2d9__Example2.wav}` (local only, not in repo; regeneratable via `scripts/pcm_capture_reconstruct.py` if needed).
- MPS sanity: `torch.backends.mps.is_available()` returns True on this M-series MacBook. Grid-search already confirmed bit-identical transcripts CPU vs MPS on tested configs (10/0.5, 20/1.0, 10/2.0) on the OLD API. The `word_timestamps=True` path on MPS is untested.
- Not touching: anything in `openspec/changes/per-speaker-stream-refine/` tasks §2–§12. That's the refine contour itself, scope for the next ralphex plan.

## Development Approach

- **Testing approach**: Regular (existing unit suite + manual live smoke + grid-search regression).
- Commit after each logical step. Don't batch unrelated changes.
- Upstream merge is a foreign-repo operation (the `../GigaAM` checkout is outside this repo's working tree); it doesn't show up in `git status` here but affects `pip install -e` resolution at import time.

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Upgrade upstream GigaAM checkout

- [ ] 1.1 In `/Users/vkkovalev/projects/GigaAM`: `git fetch origin` then `git log --oneline 0a3f103..origin/main` — verify the 9 expected commits (cfef5e7, 33be2c4, e580e38, 7ce42d2, d427e5a, 7448b74, a0deb50, 5b5b4ad, a755fec, 6e4b027) are visible.
- [ ] 1.2 In `/Users/vkkovalev/projects/GigaAM`: `git merge --ff-only origin/main`. If ff fails, stop and ⚠️-flag (local unclean state — do not force).
- [ ] 1.3 Verify the editable-install still resolves the new version: `/Users/vkkovalev/projects/recog/venv-gigaam/bin/python -c "import gigaam; print(gigaam.__file__)"` — path should still point at `../GigaAM/gigaam/__init__.py`.
- [ ] 1.4 Quick API probe (path-based transcription only, no batch yet):
  ```bash
  /Users/vkkovalev/projects/recog/venv-gigaam/bin/python -c "
  import gigaam
  m = gigaam.load_model('v3_e2e_rnnt', device='cpu')
  r = m.transcribe('/tmp/recog_exp/raw_take2/d9e189e5__Example.wav', word_timestamps=True)
  print('type:', type(r).__name__)
  print('text[:80]:', r.text[:80])
  print('first 5 words:', [(w.text, w.start, w.end) for w in r.words[:5]])
  "
  ```
- [ ] 1.5 Acceptance: `r.words` is a non-empty list; each entry has `text` str, `start` and `end` floats or ints. Frame shift is ~10–40 ms per upstream audit — first word's `start` should be near 0, not negative. If the probe fails or the wav file is missing, try `example_5min.wav` in repo root.
- [ ] 1.6 Commit the upstream move in the `GigaAM` checkout is implicit (merge fast-forward). No commit here — this is a pure dependency bump tracked outside this repo.

### Task 2: Adapt ASRModule.transcribe_batch to new decoding.decode signature

- [ ] 2.1 Read `recog/asr.py::ASRModule.transcribe_batch` in full; also grep for any other call to `model.decoding.decode` or `.decoding.decode` across `recog/` and `scripts/`.
- [ ] 2.2 Update each call site: old `texts = model.decoding.decode(...)` (List[str]) → new `decoded = model.decoding.decode(...); texts = [t[0] for t in decoded]` (first tuple element is text). Token IDs and frames are unused; don't store them.
- [ ] 2.3 The public `ASRModule.transcribe_batch(pcm_list) -> List[str]` signature MUST stay stable. Callers in `recog/live.py::_on_pcm_batch` must not need edits. Verify by grep after the change: no call sites changed.
- [ ] 2.4 Run full unit suite: `cd /Users/vkkovalev/projects/recog && venv-gigaam/bin/python -m pytest tests/ -v -m "not e2e"` — all green.
- [ ] 2.5 Manual live smoke:
  - `./recog-service.sh restart`
  - Open a short KTalk meeting or run a quick `python mp4_notes.py live --list-devices` sanity check.
  - Verify `logs/live.log` shows no stack traces referencing `decoding.decode` or tuple unpacking.
  - 30 s is enough. Stop the service when done if it isn't run continuously.
- [ ] 2.6 Commit: `fix(asr): adapt transcribe_batch to new GigaAM decoding.decode tuple return`

### Task 3: Grid-search regression (confirm WER baseline)

- [ ] 3.1 Re-run the harness on take-2 fixtures for two key configs:
  ```bash
  ./scripts/pcm_experiment.py \
    --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav \
    --wav /tmp/recog_exp/raw_take2/8ee8e2d9__Example2.wav \
    --trim 17-65 \
    --reference-file experiments/reference_take2.txt \
    --configs "20:1.0,10:0.5"
  ```
  If `/tmp/recog_exp/raw_take2/` is gone (ephemeral), first regenerate via `scripts/pcm_capture_reconstruct.py` from the most recent capture under `/tmp/recog_pcm_capture/` — or ⚠️-flag and ask.
- [ ] 3.2 Expected numbers (from `experiments/2026-04-23-gigaam-grid.md`):
  - 20/1.0 → Example 23.9% / Example2 23.9% / avg 23.9%
  - 10/0.5 → Example 25.0% / Example2 23.9% / avg 24.5%
- [ ] 3.3 Acceptance: both configs stay within ±1% absolute WER of these numbers. If regression > 1%, stop and ⚠️-flag — upstream either changed the decoder defaults or the tuple unpacking dropped some text.
- [ ] 3.4 No commit — this is a verification step. Findings noted inline below task heading if anything interesting (e.g. slightly different WER).

### Task 4: Add transcribe_with_timestamps helper to ASRModule

- [ ] 4.1 Add new method `ASRModule.transcribe_with_timestamps(audio: np.ndarray | torch.Tensor | str, sample_rate: int = 16000) -> gigaam.TranscriptionResult`. If input is an array/tensor: write to a tempfile (16 kHz mono PCM WAV), call `self.model.transcribe(path, word_timestamps=True)`, delete tempfile. If input is a str path, pass through.
- [ ] 4.2 Before returning, normalise `word.start` and `word.end` on each `Word` to integer milliseconds. Upstream returns floats (seconds most likely); multiply by 1000 and round. Keep the same dataclass shape — we don't need a custom class yet.
- [ ] 4.3 Quiet option: add `quiet: bool = False` kwarg to suppress upstream progress bars (mirror pattern in existing `transcribe_simple`).
- [ ] 4.4 Unit test in `tests/test_asr.py` (create file if missing): synthesise a 1-second silence wav via numpy, call `transcribe_with_timestamps`, assert `result.text` is str (may be empty for silence) and `result.words` is a list (may be empty).
- [ ] 4.5 Commit: `feat(asr): add transcribe_with_timestamps helper (upstream word-level API)`

### Task 5: MPS auto-detect in ASRModule

- [ ] 5.1 In `ASRModule.__init__`, when the `device` kwarg is `None` (default), auto-detect: `device = "mps" if torch.backends.mps.is_available() else "cpu"`. Explicit `device="cpu"` / `device="mps"` override always wins — re-verify this path by reading current code.
- [ ] 5.2 Pass the resolved device to `gigaam.load_model(...)`. Verify upstream accepts `device="mps"` via quick repl check; if it rejects (e.g., whitelist of cuda/cpu only), apply a minimal local patch or move device transfer to post-load via `model.to("mps")` — whichever upstream supports cleanly.
- [ ] 5.3 Log the chosen device on model load (single line, not for every invocation).
- [ ] 5.4 Unit test: verify that `ASRModule(device=None)` picks "mps" on this machine (guard with `@pytest.mark.skipif(not torch.backends.mps.is_available())`).
- [ ] 5.5 Commit: `feat(asr): auto-detect MPS backend on Apple Silicon`

### Task 6: MPS vs CPU equivalence check (WER + text)

- [ ] 6.1 Re-run grid-search with explicit device flags:
  ```bash
  ./scripts/pcm_experiment.py --device cpu  --configs "20:1.0,10:0.5" --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav --trim 17-65 --reference-file experiments/reference_take2.txt > /tmp/wer_cpu.txt 2>&1
  ./scripts/pcm_experiment.py --device mps  --configs "20:1.0,10:0.5" --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav --trim 17-65 --reference-file experiments/reference_take2.txt > /tmp/wer_mps.txt 2>&1
  diff /tmp/wer_cpu.txt /tmp/wer_mps.txt
  ```
- [ ] 6.2 Acceptance: diff shows only expected differences (device field, wall-clock values). Transcribed text columns MUST be byte-identical across CPU and MPS for both configs. If not, ⚠️-flag with the specific config and the diff output — do NOT mark complete.
- [ ] 6.3 No commit — pure verification.

### Task 7: MPS batched-forward micro-bench

- [ ] 7.1 Create `scripts/mps_batch_bench.py`: load one `ASRModule` per device (CPU, MPS), synthesise batches of N ∈ {1, 4, 8} copies of a 10-second 16 kHz synthetic sine or silence, call `transcribe_batch`, time wall-clock (use `torch.mps.synchronize()` + `time.perf_counter()` on MPS; just `time.perf_counter()` on CPU). Dump a small markdown table.
- [ ] 7.2 Run and save output: `venv-gigaam/bin/python scripts/mps_batch_bench.py > experiments/2026-04-24-mps-batch.md`.
- [ ] 7.3 Acceptance: MPS wall ≤ CPU wall at B=8 (ideally ≤ 0.5×). If MPS is slower, not blocking for the plan — document in the file but flag with ⚠️ for follow-up.
- [ ] 7.4 Commit: `exp: MPS batched-forward micro-bench (B=1,4,8) results`

### Task 8: Update tasks.md progress in openspec change

- [ ] 8.1 In `/Users/vkkovalev/projects/recog/openspec/changes/per-speaker-stream-refine/tasks.md`, mark §0 (0.1–0.4) and §1 (1.1–1.4) as `[x]` — these are covered by the tasks above. Don't modify task bodies.
- [ ] 8.2 Run `openspec validate per-speaker-stream-refine` — must still say "is valid".
- [ ] 8.3 Commit: `plan(openspec): mark stream-refine §0 and §1 complete (upstream GigaAM + MPS)`

### Task 9: Push

- [ ] 9.1 `git status` — confirm working tree clean, only the expected commits ahead of origin/main.
- [ ] 9.2 `git log --oneline origin/main..HEAD` — sanity-check commit messages match the plan.
- [ ] 9.3 `git push` to origin/main.

## Post-Completion

- `openspec/changes/per-speaker-stream-refine/tasks.md` §0 and §1 show `[x]` green; `openspec validate` still passes.
- `experiments/2026-04-24-mps-batch.md` documents MPS throughput on this machine.
- `ASRModule.transcribe_with_timestamps(...)` is available and unit-tested; ready to be called from the future `RefineWorker`.
- If any WER regression (> ±1%) surfaces or MPS fails bit-identical → do NOT proceed to the refine contour plan; ⚠️-flag in a follow-up and diagnose first.
- Next ralphex plan: `2026-04-XX-per-speaker-stream-refine-contour.md` covering openspec §2 (refine worker foundations) through §8 (webapp UI). Scope it as a fresh plan; don't try to continue this one.
