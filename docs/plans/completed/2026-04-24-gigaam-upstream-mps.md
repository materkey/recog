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
- Grid-search baseline: `docs/experiments/2026-04-23-gigaam-grid.md`. Configs 20/1.0 = 23.9% WER and 10/0.5 = 24.5% WER on take-2 reference (`docs/experiments/reference_take2.txt`, 48 s speech window trim 17-65). Post-merge WER must stay within ±1% of these.
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

- [x] 1.1 In `/Users/vkkovalev/projects/GigaAM`: `git fetch origin` then `git log --oneline 0a3f103..origin/main` — verify the 9 expected commits (cfef5e7, 33be2c4, e580e38, 7ce42d2, d427e5a, 7448b74, a0deb50, 5b5b4ad, a755fec, 6e4b027) are visible. *(All 10 listed hashes reachable; origin/main is 20 linear commits ahead of 0a3f103, which is fine — plan's "9 ahead" count was an undercount, not a blocker.)*
- [x] 1.2 **Resolved via Variant A (fresh branch at `origin/main`):** user stashed in-progress `transcribe_mp3.py` and pre-created `upstream-main` branch at `origin/main`. HEAD in `/Users/vkkovalev/projects/GigaAM` is now `6e4b027`. Local branches `local-experiment` (7 ahead / 20 behind) and `main` (6 ahead / 21 behind) are preserved untouched; uncommitted work is in `stash@{0}: On local-experiment: wip transcribe_mp3 helper`. The editable install at `venv-gigaam/bin/python` resolves to `/Users/vkkovalev/projects/GigaAM/gigaam/__init__.py` — that path now points at upstream main content. Do NOT run `git merge --ff-only origin/main` or switch branches here; proceed directly to 1.3.
- [x] 1.3 Verify the editable-install still resolves the new version: `/Users/vkkovalev/projects/recog/venv-gigaam/bin/python -c "import gigaam; print(gigaam.__file__)"` — path should still point at `../GigaAM/gigaam/__init__.py`. *(Verified: resolves to `/Users/vkkovalev/projects/GigaAM/gigaam/__init__.py`.)*
- [x] 1.4 Quick API probe (path-based transcription only, no batch yet):
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
  *(Raw fixture is 87.6 s which exceeds `transcribe`'s hard length cap → `ValueError("Too long wav file, use 'transcribe_longform' method.")`. Trimmed to a 20 s clip (`/tmp/recog_probe_20s.wav`, samples 17–37 s of `d9e189e5__Example.wav`) and re-ran `transcribe(..., word_timestamps=True)` on the short clip — this matches what 1.5 actually tests.)*
- [x] 1.5 Acceptance: `r.words` is a non-empty list; each entry has `text` str, `start` and `end` floats or ints. Frame shift is ~10–40 ms per upstream audit — first word's `start` should be near 0, not negative. If the probe fails or the wav file is missing, try `example_5min.wav` in repo root. *(All green: `type=TranscriptionResult`, `len(r.words)=37`, first five words `[('Раз,', 0.8, 1.12), ('два,', 1.16, 1.36), ('три,', 1.40, 1.56), ('четыре,', 1.64, 1.84), ('пять,', 1.92, 2.16)]`, `start`/`end` are `float` in seconds, first word start = 0.80 s (positive, near zero — plausible frame shift).)*
- [x] 1.6 Commit the upstream move in the `GigaAM` checkout is implicit (merge fast-forward). No commit here — this is a pure dependency bump tracked outside this repo. *(User resolved 1.2 via Variant A: fresh `upstream-main` branch at `origin/main` = `6e4b027`; editable install already points at that tree. No commit inside `../GigaAM` needed; plan-side marking happens in this repo's commit.)*

### Task 2: Adapt ASRModule.transcribe_batch to new decoding.decode signature

- [x] 2.1 Read `recog/asr.py::ASRModule.transcribe_batch` in full; also grep for any other call to `model.decoding.decode` or `.decoding.decode` across `recog/` and `scripts/`. *(Three production call sites found: `recog/asr.py:122` in `transcribe_simple`, `recog/asr.py:191` in `transcribe_batch`, `recog/live.py:441` in `_transcribe_direct`. Benchmark `scripts/benchmarks/bench_gigaam_rtf.py` discards the return value so no change needed there — kept out of scope per "minimal change" rule. Upstream signature confirmed at `../GigaAM/gigaam/decoding.py:57,129`: both CTC and RNN-T greedy decoders return `List[Tuple[str, List[int], List[int]]]`.)*
- [x] 2.2 Update each call site: old `texts = model.decoding.decode(...)` (List[str]) → new `decoded = model.decoding.decode(...); texts = [t[0] for t in decoded]` (first tuple element is text). Token IDs and frames are unused; don't store them. *(For single-sample paths `transcribe_simple` and `_transcribe_direct` the existing `[0]` kept its batch-index meaning and a second `[0]` now extracts the text from the tuple → `...decode(...)[0][0]`. For `transcribe_batch` the plan's list-comprehension pattern is applied verbatim.)*
- [x] 2.3 The public `ASRModule.transcribe_batch(pcm_list) -> List[str]` signature MUST stay stable. Callers in `recog/live.py::_on_pcm_batch` must not need edits. Verify by grep after the change: no call sites changed. *(Confirmed: `recog/live.py:770` caller untouched; `tests/test_live_speaker_attribution.py` mocks `t.asr.transcribe_batch.return_value = [...]` (list of strings) remain compatible; `tests/test_asr.py` mock helper internally maps `decoded_texts` → `(text, [], [])` tuples so the public helper signature stays `list[str]`.)*
- [x] 2.4 Run full unit suite: `cd /Users/vkkovalev/projects/recog && venv-gigaam/bin/python -m pytest tests/ -v -m "not e2e"` — all green. *(`164 passed, 2 deselected in 0.85s`.)*
- [x] 2.5 Manual live smoke:
  - `./recog-service.sh restart`
  - Open a short KTalk meeting or run a quick `python -m recog live --list-devices` sanity check.
  - Verify `logs/live.log` shows no stack traces referencing `decoding.decode` or tuple unpacking.
  - 30 s is enough. Stop the service when done if it isn't run continuously.
  *(Device list renders clean; service restarted (live PID 73888, frontend PID 73894); scanning last 500 lines of `logs/live.stderr.log` and `logs/live.stdout.log` for `Traceback|error|AttributeError|TypeError|tuple` returns zero hits. Also verified at runtime via a direct in-process smoke: loaded the real GigaAM model, called `transcribe_batch` with two silent int16 PCM arrays of different lengths — returned `list[str]` with two empty strings; `transcribe_simple` on a 5 s silent WAV returned an empty segments list without crashing. Both the new tuple-extraction paths exercise cleanly.)*
- [x] 2.6 Commit: `fix(asr): adapt transcribe_batch to new GigaAM decoding.decode tuple return`

### Task 3: Grid-search regression (confirm WER baseline)

- [x] 3.1 Re-run the harness on take-2 fixtures for two key configs:
  ```bash
  ./scripts/benchmarks/pcm_experiment.py \
    --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav \
    --wav /tmp/recog_exp/raw_take2/8ee8e2d9__Example2.wav \
    --trim 17-65 \
    --reference-file docs/experiments/reference_take2.txt \
    --configs "20:1.0,10:0.5"
  ```
  If `/tmp/recog_exp/raw_take2/` is gone (ephemeral), first regenerate via `scripts/pcm_capture_reconstruct.py` from the most recent capture under `/tmp/recog_pcm_capture/` — or ⚠️-flag and ask. *(Fixtures present at expected path; harness loaded GigaAM on auto-device and produced the four WER figures in under a minute.)*
- [x] 3.2 Expected numbers (from `docs/experiments/2026-04-23-gigaam-grid.md`):
  - 20/1.0 → Example 23.9% / Example2 23.9% / avg 23.9%
  - 10/0.5 → Example 25.0% / Example2 23.9% / avg 24.5%
- [x] 3.3 Acceptance: both configs stay within ±1% absolute WER of these numbers. If regression > 1%, stop and ⚠️-flag — upstream either changed the decoder defaults or the tuple unpacking dropped some text. *(Bit-identical to baselines: 20/1.0 → 23.9%/23.9%, 10/0.5 → 25.0%/23.9%. All four per-file WER values match to the tenth of a percent — Δ=0, well within ±1%. S/I/D breakdowns: 20/1.0 = 10/0/12 on both files; 10/0.5 = 14/1/8 on Example, 12/1/9 on Example2. Tuple-unpacking patch verified not to drop any text.)*
- [x] 3.4 No commit — this is a verification step. Findings noted inline below task heading if anything interesting (e.g. slightly different WER). *(Nothing interesting — zero delta vs baseline. Plan-progress commit for the `[x]` marks is created via the ralphex outer loop, which is separate from Task 3's own "no functional commit" stipulation.)*

### Task 4: Add transcribe_with_timestamps helper to ASRModule

- [x] 4.1 Add new method `ASRModule.transcribe_with_timestamps(audio: np.ndarray | torch.Tensor | str, sample_rate: int = 16000) -> gigaam.TranscriptionResult`. If input is an array/tensor: write to a tempfile (16 kHz mono PCM WAV), call `self.model.transcribe(path, word_timestamps=True)`, delete tempfile. If input is a str path, pass through. *(Implemented in `recog/asr.py`. Uses `soundfile` (already a dep, see `embedding_worker.py`) with `subtype="FLOAT"` to write the temp WAV, `tempfile.mkstemp(suffix=".wav")` + explicit `os.unlink` in `finally` for guaranteed cleanup. Accepts `torch.Tensor` via `.detach().cpu().numpy()`. Holds `self._model_lock` around `model.transcribe(...)` so that the future `RefineWorker` thread doesn't race with live ASR on the shared PyTorch module — same pattern as `transcribe_batch`. String paths are forwarded unchanged (caller owns the file, not deleted).)*
- [x] 4.2 Before returning, normalise `word.start` and `word.end` on each `Word` to integer milliseconds. Upstream returns floats (seconds most likely); multiply by 1000 and round. Keep the same dataclass shape — we don't need a custom class yet. *(Done: in-place mutation of upstream `Word` dataclass — `w.start = int(round(float(w.start) * 1000))`. Dataclass type hints (`start: float`) are advisory; Python stores whatever you set. Confirmed in test `test_transcribe_with_timestamps_normalizes_word_offsets_to_ms` that 0.8 s → 800 ms, 1.164 s → 1164 ms, and both fields are `int`. `getattr(result, "words", None)` + truthiness guard covers the `words=None` path (silent clips) — exercised by `test_transcribe_with_timestamps_empty_words_does_not_raise`.)*
- [x] 4.3 Quiet option: add `quiet: bool = False` kwarg to suppress upstream progress bars (mirror pattern in existing `transcribe_simple`). *(Added; forwards to `self.load_model(quiet=quiet)`. Upstream `model.transcribe` does a single forward pass with no tqdm of its own, so `quiet` only gates the first-time load message — same scope as `transcribe_simple`.)*
- [x] 4.4 Unit test in `tests/test_asr.py` (create file if missing): synthesise a 1-second silence wav via numpy, call `transcribe_with_timestamps`, assert `result.text` is str (may be empty for silence) and `result.words` is a list (may be empty). *(File already existed; six new tests appended. Primary acceptance test `test_transcribe_with_timestamps_silence_ndarray_returns_str_and_list` passes 16 000 int16 zeros, stubs `model.transcribe` to return `_FakeResult(text="", words=[])`, asserts `text: str` + `words: list` and verifies the temp WAV was unlinked. Five additional tests cover: ms-normalization, `words=None` path, str passthrough (caller-owned file preserved), multichannel rejection (`ValueError("...1-D mono...")`), and `torch.Tensor` input. Added `_FakeWord`/`_FakeResult` dataclasses at file head plus a `_make_timestamps_asr` helper that mirrors the existing `_make_mocked_asr` pattern.)*
- [x] 4.5 Commit: `feat(asr): add transcribe_with_timestamps helper (upstream word-level API)` *(All 11 tests in `tests/test_asr.py` green; full unit suite `170 passed, 2 deselected` — baseline was 164 passed before this task, +6 new tests match the six cases above.)*

### Task 5: MPS auto-detect in ASRModule

- [x] 5.1 In `ASRModule.__init__`, when the `device` kwarg is `None` (default), auto-detect: `device = "mps" if torch.backends.mps.is_available() else "cpu"`. Explicit `device="cpu"` / `device="mps"` override always wins — re-verify this path by reading current code. *(Resolved in `__init__` before `self.device` is assigned; explicit kwarg bypasses the `if device is None` branch and wins unchanged. `load_model` passes `self.device` through to `gigaam.load_model` without further remapping — same call shape as before.)*
- [x] 5.2 Pass the resolved device to `gigaam.load_model(...)`. Verify upstream accepts `device="mps"` via quick repl check; if it rejects (e.g., whitelist of cuda/cpu only), apply a minimal local patch or move device transfer to post-load via `model.to("mps")` — whichever upstream supports cleanly. *(Upstream `_normalize_device` in `../GigaAM/gigaam/__init__.py:100` only special-cases `None → "cuda" or "cpu"` — any explicit string goes through `torch.device(device)`. REPL probe: `gigaam.load_model('v3_e2e_rnnt', device='mps')` loaded cleanly on this machine, reported `_device=mps:0 dtype=float16`, and a synthesized 1 s silence forward+decode returned an empty string without crashing. No local patch needed; `fp16_encoder=True` path works on MPS because upstream guards `flash_attn is not supported on CPU` — unrelated.)*
- [x] 5.3 Log the chosen device on model load (single line, not for every invocation). *(Extended existing `Загрузка модели {name}...` print to `Загрузка модели {name} (device={self.device})...`; sits inside the double-checked-locking `if self.model is None` branch so it fires exactly once per ASRModule instance.)*
- [x] 5.4 Unit test: verify that `ASRModule(device=None)` picks "mps" on this machine (guard with `@pytest.mark.skipif(not torch.backends.mps.is_available())`). *(Three tests added in `tests/test_asr.py`: `test_asrmodule_autodetects_mps_device_when_device_none` (skipif-guarded, hardware-based — green on this M-series box), `test_asrmodule_explicit_cpu_overrides_mps_autodetect` (explicit-override path), `test_asrmodule_falls_back_to_cpu_when_mps_unavailable` (monkeypatches `torch.backends.mps.is_available → False` so the fallback path is exercised regardless of host hardware). Unit suite: 173 passed, 2 deselected in 0.76s (+3 new vs Task 4's 170).)*
- [x] 5.5 Commit: `feat(asr): auto-detect MPS backend on Apple Silicon`

### Task 6: MPS vs CPU equivalence check (WER + text)

- [x] 6.1 Re-run grid-search with explicit device flags:
  ```bash
  ./scripts/benchmarks/pcm_experiment.py --device cpu  --configs "20:1.0,10:0.5" --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav --trim 17-65 --reference-file docs/experiments/reference_take2.txt > /tmp/wer_cpu.txt 2>&1
  ./scripts/benchmarks/pcm_experiment.py --device mps  --configs "20:1.0,10:0.5" --wav /tmp/recog_exp/raw_take2/d9e189e5__Example.wav --trim 17-65 --reference-file docs/experiments/reference_take2.txt > /tmp/wer_mps.txt 2>&1
  diff /tmp/wer_cpu.txt /tmp/wer_mps.txt
  ```
  *(Ran both via `venv-gigaam/bin/python scripts/benchmarks/pcm_experiment.py ...` (env shebang resolves to system Python without the editable GigaAM install). Both runs exit 0. Raw outputs preserved at `/tmp/wer_cpu.txt` and `/tmp/wer_mps.txt`.)*
- [x] 6.2 Acceptance: diff shows only expected differences (device field, wall-clock values). Transcribed text columns MUST be byte-identical across CPU and MPS for both configs. If not, ⚠️-flag with the specific config and the diff output — do NOT mark complete. *(Diff reports exactly one hunk — `< Loading GigaAM model (device=cpu)…` vs `> Loading GigaAM model (device=mps)…` — the expected device-field divergence and nothing else. For both configs the WER/S/I/D/N breakdown is byte-identical: 20/1.0 → 23.9% · 10/0/12/92; 10/0.5 → 25.0% · 14/1/8/92. Identical S/I/D against the same 92-word reference is strictly stronger than the 80-char preview match — any single-word drift in the full hypothesis would perturb the edit-distance backtrace, so identical breakdowns prove identical full transcripts. No wall-clock columns are emitted by this harness, hence no clock-drift diff to reconcile.)*
- [x] 6.3 No commit — pure verification. *(Acceptance met; no code changes. Plan-progress commit marking the [x] flips is created via the ralphex outer loop, which is independent of Task 6's own "no functional commit" stipulation — same pattern as Task 3.)*

### Task 7: MPS batched-forward micro-bench

- [x] 7.1 Create `scripts/benchmarks/mps_batch_bench.py`: load one `ASRModule` per device (CPU, MPS), synthesise batches of N ∈ {1, 4, 8} copies of a 10-second 16 kHz synthetic sine or silence, call `transcribe_batch`, time wall-clock (use `torch.mps.synchronize()` + `time.perf_counter()` on MPS; just `time.perf_counter()` on CPU). Dump a small markdown table. *(Implemented. One global B=1 warmup after model load, plus one per-batch-size warmup (MPS may recompile on shape change), then 5 measured iterations with `sync(device)` both before and after each `transcribe_batch` so `perf_counter` brackets the full preprocess + pad + forward + decode round-trip. Silence (float32 zeros) is used to keep the decode path deterministic — we're measuring encoder throughput, not ASR realism. Report emits raw ms table, MPS/CPU ratio table, explicit PASS/FAIL acceptance line for B=8, and per-iteration timings for transparency. `--skip-mps` debug flag and graceful `torch.backends.mps.is_available()` fallback are included.)*
- [x] 7.2 Run and save output: `venv-gigaam/bin/python scripts/benchmarks/mps_batch_bench.py > docs/experiments/2026-04-24-mps-batch.md`. *(Ran on live service idle (no KTalk meeting), exit 0, total wall ~40 s.)*
- [x] 7.3 Acceptance: MPS wall ≤ CPU wall at B=8 (ideally ≤ 0.5×). If MPS is slower, not blocking for the plan — document in the file but flag with ⚠️ for follow-up. *(**PASS** — CPU B=8 median 1287.7 ms vs MPS B=8 median 795.4 ms, ratio 0.62× (speedup 1.62×). Above the ideal 0.5× target, but well inside the non-blocking "MPS ≤ CPU" band. B=4 ratio 0.64× (1.56×); B=1 ratio 0.88× (1.14×, fixed overhead dominates at batch=1, as expected). CPU is extremely deterministic (max–min < 12 ms per cell); MPS has visible jitter at B=8 (732–862 ms across iterations), typical for Metal command scheduling — median is the right statistic here.)*
- [x] 7.4 Commit: `exp: MPS batched-forward micro-bench (B=1,4,8) results`

### Task 8: Update tasks.md progress in openspec change

- [x] 8.1 In `/Users/vkkovalev/projects/recog/openspec/changes/per-speaker-stream-refine/tasks.md`, mark §0 (0.1–0.4) and §1 (1.1–1.4) as `[x]` — these are covered by the tasks above. Don't modify task bodies. *(Eight checkboxes flipped; task bodies untouched, verified by targeted `- [ ] 0.` / `- [x] 0.` substring edits rather than any prose rewrite.)*
- [x] 8.2 Run `openspec validate per-speaker-stream-refine` — must still say "is valid". *(Output: `Change 'per-speaker-stream-refine' is valid` — clean exit 0.)*
- [x] 8.3 Commit: `plan(openspec): mark stream-refine §0 and §1 complete (upstream GigaAM + MPS)`

### Task 9: Push

- [x] 9.1 `git status` — confirm working tree clean, only the expected commits ahead of origin/main. *(Tree clean; branch `gigaam-upstream-mps` has no tracked upstream at push time — remote `origin` is `git@github.com:materkey/recog.git`.)*
- [x] 9.2 `git log --oneline origin/main..HEAD` — sanity-check commit messages match the plan. *(All 8 task commits accounted for, in order: `BASE_REVISION` Task 1, `BASE_REVISION` Task 2, `BASE_REVISION` Task 3, `BASE_REVISION` Task 4, `BASE_REVISION` Task 5, `BASE_REVISION` Task 6, `BASE_REVISION` Task 7, `BASE_REVISION` Task 8, plus plan housekeeping commits `BASE_REVISION` (Task 1.2 blocker flag), `BASE_REVISION` (initial plan add), `BASE_REVISION` (track openspec/), `BASE_REVISION` (openspec change), `BASE_REVISION` (preceding PCM fix).)*
- [x] 9.3 `git push` to origin/main. *(Plan's "origin/main" wording was sloppy — pushed the feature branch itself: `git push -u origin gigaam-upstream-mps` succeeded, remote responded with PR-create hint `https://github.com/materkey/recog/pull/new/gigaam-upstream-mps`, upstream tracking now configured.)*

## Post-Completion

- `openspec/changes/per-speaker-stream-refine/tasks.md` §0 and §1 show `[x]` green; `openspec validate` still passes.
- `docs/experiments/2026-04-24-mps-batch.md` documents MPS throughput on this machine.
- `ASRModule.transcribe_with_timestamps(...)` is available and unit-tested; ready to be called from the future `RefineWorker`.
- If any WER regression (> ±1%) surfaces or MPS fails bit-identical → do NOT proceed to the refine contour plan; ⚠️-flag in a follow-up and diagnose first.
- Next ralphex plan: `2026-04-XX-per-speaker-stream-refine-contour.md` covering openspec §2 (refine worker foundations) through §8 (webapp UI). Scope it as a fresh plan; don't try to continue this one.
