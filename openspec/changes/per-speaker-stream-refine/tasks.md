## 0. Pre-flight: upstream GigaAM merge

- [ ] 0.1 Upgrade `../GigaAM` checkout to upstream `origin/main` (commits through `6e4b027`); verify `import gigaam` + `gigaam.load_model("v3_e2e_rnnt").transcribe(wav_path, word_timestamps=True)` returns a `TranscriptionResult` with `.words` list of `Word(text, start, end)`
- [ ] 0.2 Adapt `recog/asr.py::ASRModule.transcribe_batch` to the new `decoding.decode()` return type (was `List[str]`; after commit `a755fec` is `List[Tuple[str, List[int], List[int]]]`). Keep public method signature stable — callers unaffected
- [ ] 0.3 Full unit suite green (`python -m pytest tests/ -v`); manual live smoke: `./recog-service.sh restart` + short KTalk meeting; confirm no regression on live 3-s path
- [ ] 0.4 Grid-search regression: re-run `scripts/pcm_experiment.py` against take-2 fixtures with configs 20:1.0 and 10:0.5; WER must stay within ±1% of the baseline in `experiments/2026-04-23-gigaam-grid.md`

## 1. MPS (Metal) backend

- [ ] 1.1 Add MPS auto-detect to `ASRModule.__init__` when `device` arg is `None`: `"mps" if torch.backends.mps.is_available() else "cpu"`. Explicit override (`device="cpu"`) always honoured
- [ ] 1.2 Manual smoke: `scripts/pcm_experiment.py --device mps --configs 20:1.0,10:0.5 --wav ...` produces text bit-identical to CPU run on same input (compare stdout line-by-line)
- [ ] 1.3 Benchmark batched forward on MPS with B ∈ {1, 4, 8} and variable-length inputs padded to `T_max`; record wall-clock per batch in `experiments/2026-04-XX-mps-batch.md`; confirm no regression vs CPU
- [ ] 1.4 Default device stays `None` (auto-detect) — no CLI / env changes needed by users

## 2. Refine worker foundations

- [ ] 2.1 New constants in `recog/config.py`: `PCM_REFINE_WINDOW_S = 20.0`, `PCM_REFINE_MONOLOG_CAP_S = 15.0`; new `pcm_refine_enabled()` reading `RECOG_PCM_REFINE` (returns `False` unless `pcm_transport_enabled()` is also `True`)
- [ ] 2.2 New method `ASRModule.transcribe_with_timestamps(pcm: Tensor | np.ndarray) -> TranscriptionResult` wrapping upstream `transcribe(word_timestamps=True)`; word start/end normalised to milliseconds (ints)
- [ ] 2.3 **Spike (must precede 2.4):** determine whether upstream GigaAM's RNN-T decoder exposes a per-step hook suitable for cooperative abort. Concretely: inspect `gigaam.decoding.RNNTGreedyDecoder.decode` post upstream merge; attempt a monkey-patched `step_callback` prototype; record outcome in `experiments/2026-04-XX-refine-abort-spike.md`. Outcome drives the abort mode in 2.4 and the worded guarantee in the spec (§"Live batch preempts refine")
- [ ] 2.4 New module `recog/refine.py` with `RefineWorker(model_lock: threading.Lock, transcribe_fn: Callable, apply_fn: Callable, abort_mode: Literal["cooperative", "coarse"])`. `abort_mode` defaults to spike outcome; background thread; API: `submit(task: RefineTask)`, `pause()`, `resume()`, `stop()`. In `cooperative` mode, the transcribe_fn honours a `should_abort()` callable polled between decoder steps; in `coarse` mode, refine holds the lock until one full window finishes (≤ 3 s CPU worst case)
- [ ] 2.5 Model-lock integration in `LiveTranscriber._on_pcm_batch`: acquire `model_lock` before `transcribe_batch`; set a `live_pending` flag that `RefineWorker` polls (cadence per spike outcome recorded in 2.3)
- [ ] 2.6 Unit test `tests/test_refine_worker.py`: task submission, pause/resume, abort-on-live semantics for BOTH `cooperative` and `coarse` modes with a mock transcribe_fn

## 3. Per-pid rolling PCM ring

- [ ] 3.1 Extend `PerSpeakerIngest` with `_pcm_rings: Dict[pid, deque[np.ndarray]]` sized to `PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S` worth of Int16 samples; append in lockstep with every successful `enqueue`
- [ ] 3.2 Add per-pid `_silence_ms[pid]` counter: increments by `PCM_BATCH_INTERVAL_MS` on each tick with no new chunk; resets to 0 on any new chunk
- [ ] 3.3 Add per-pid `_continuous_speech_ms[pid]` counter: increments by chunk duration on each new chunk when `_silence_ms` was already 0; resets to 0 on silence gap `>= SPEAKER_TURN_GAP * 1000`
- [ ] 3.4 Maintain per-pid monotonic `_pid_audio_ms: Dict[pid, float]` in `PerSpeakerIngest`; increment by `len(samples) / 16000 * 1000` on every successful ring append (same call site as 3.1). Exposed via `get_pid_audio_ms(pid) -> float`
- [ ] 3.5 New method `PerSpeakerIngest.get_refine_candidate(pid) -> Optional[RefineTask]`: returns `RefineTask(pid, pcm_samples, offset_base_ms)` when `_silence_ms >= SPEAKER_TURN_GAP*1000` (gap) OR `_continuous_speech_ms >= PCM_REFINE_MONOLOG_CAP_S*1000` (monolog). `offset_base_ms` = `_pid_audio_ms[pid] - len(pcm_samples) / 16000 * 1000` (i.e., the pid-frame time of the first sample in the snapshot). Returns `None` if no un-refined audio; advances the un-refined watermark atomically
- [ ] 3.6 Integrate `get_refine_candidate` call in the existing `_worker` drain loop; on candidate, call `refine_worker.submit(task)`
- [ ] 3.7 Unit tests in `tests/test_per_speaker.py`: gap trigger fires once per reply; monolog trigger fires at 15 s cap; watermark advances on successful apply

## 4. Segment tracking for alignment

- [ ] 4.1 Add per-pid `_pcm_live_segments: Dict[pid, List[Tuple[segment_id, pid_audio_ms_start, pid_audio_ms_end]]]` to `LiveTranscriber` (times are in the per-pid frame defined in D1); populate inside `_on_pcm_batch` after each `append_segment` returns an id. `pid_audio_ms_start = per_speaker_ingest.get_pid_audio_ms(pid) - chunk_duration_ms`; `pid_audio_ms_end = per_speaker_ingest.get_pid_audio_ms(pid)`. `audio_offset_ms` (mixed-wav frame) continues to be stored on the segment for playback and is NOT used for refine alignment
- [ ] 4.2 When a refine task returns, snapshot the subset of `_pcm_live_segments[pid]` whose `[offset, offset+duration]` intersects the refine window; this is `task.segment_ids`
- [ ] 4.3 Prune `_pcm_live_segments[pid]` entries older than `PCM_REFINE_WINDOW_S + 5 s` to cap memory
- [ ] 4.4 All reads/writes of `_pcm_live_segments` under `_state_lock` (consistent with sibling per-pid dicts)

## 5. Alignment + replace

- [ ] 5.1 Implement `LiveTranscriber._apply_refine(result: RefineResult)`: rebase each refined `Word` into the per-pid frame via `word_pid_ms = result.offset_base_ms + word.start_ms` (same for end); then for each `segment_id` in `result.segment_ids`, select words whose rebased `[word_pid_ms_start, word_pid_ms_end]` lie inside `[pid_audio_ms_start, pid_audio_ms_end]` (from `_pcm_live_segments[pid]`); join their `.text` to form `new_text`; skip if empty (D8) or identical to current `text` (D8). `audio_offset_ms` on the JSONL row is untouched (mixed-wav frame is independent)
- [ ] 5.2 Extend `MeetingStorage.update_segments` to accept an optional `text` key per update row; when present, also set `refined: true` on that row. Preserves all other fields untouched
- [ ] 5.3 Add `refined: bool = False` to `TranscriptSegment` dataclass; `asdict(segment)` now includes the flag on every row
- [ ] 5.4 After `update_segments` returns, call `bridge.send_refine([{segment_id, text, refined_at: now_iso}])` for the applied changes
- [ ] 5.5 Wrap the whole apply path in a `_meeting_generation` check (same pattern as `_on_pcm_batch`); drop results whose generation no longer matches
- [ ] 5.6 Unit test `tests/test_refine_alignment.py`: synthetic `Word` list + segment boundaries ⇒ expected per-segment `new_text`; coverage of empty-list, identical-text, subset, and straddle-boundary cases

## 6. WebSocket transport

- [ ] 6.1 Add `KTalkBridge.send_refine(updates: list[dict])` mirroring `send_transcript`; broadcasts `{type: "transcript_refine", updates: [{segment_id, text, refined_at}]}` to all connected extension clients on `ws://localhost:8765`
- [ ] 6.2 In `frontend/server.py`, add a WS-client subscription to `ws://localhost:8765` (the bridge) on startup that forwards `transcript_refine` messages to webapp clients on `ws://127.0.0.1:9080` (per D10). Reconnect loop with exponential backoff (1s → 30s cap). The existing JSONL tail watcher remains for cold-start / reconnect replay of `segments` only; it does NOT forward refines
- [ ] 6.3 Fix `_notify_new_segments` rewrite-blind bug (`frontend/server.py:183-208`): when the tailer observes `os.stat(path).st_size < self.file_position`, reset `self.file_position = 0` and diff against a seen-id watermark before re-broadcasting. Prevents the retroactive-speaker-correction path from stalling. Unit test in `tests/test_frontend_server.py`
- [ ] 6.4 Unit test in `tests/test_ktalk_bridge.py`: `send_refine` produces correct JSON shape; sequential `send_transcript` → `send_refine` do not interleave on a single client

## 7. UI — extension overlay

- [ ] 7.1 In the extension UI module that renders transcript bubbles, add a handler for `{type: "transcript_refine"}` messages; locate bubble by `data-segment-id` attribute and replace its text content
- [ ] 7.2 100 ms fade-out / fade-in CSS transition on the text element to make the swap visible but not jarring
- [ ] 7.3 Rebuild extension (`npm run build`); bump `manifest.json` version to `1.4.0`
- [ ] 7.4 Manual smoke: live meeting, observe at least 3 bubbles refine within 5 s of reply-end

## 8. UI — webapp frontend

- [ ] 8.1 In the transcript-row rendering code (`frontend/static/`), add `data-segment-id={id}` and `data-refined={refined}` attributes to each row
- [ ] 8.2 CSS rules: `[data-refined="false"] { opacity: .6; border-left: 2px dashed var(--muted); }` and `[data-refined="true"] { opacity: 1; border-left: 2px solid var(--accent); transition: opacity 200ms, border-color 200ms; }`
- [ ] 8.3 JS handler for `transcript_refine`: update row textContent, set `data-refined="true"`, relying on the CSS transition for the visual
- [ ] 8.4 Tooltip / title attribute on tier-1 rows: "Предварительная расшифровка — будет уточнена"
- [ ] 8.5 Manual smoke on `http://127.0.0.1:9080`: visual tier transitions are smooth, no DOM reflow; scroll position preserved across refine

## 9. Fallback_mixed integration

- [ ] 9.1 When `PerSpeakerIngest._degraded` flips `True`, call `refine_worker.pause()` which cancels queued tasks and drops new submissions; when flips back to `False`, call `refine_worker.resume()`
- [ ] 9.2 Wire the pause/resume to the same code path that writes the `[PCM silence → mixed fallback]` marker (already in `_emit_pcm_transition_marker`)
- [ ] 9.3 Integration test `tests/test_refine_fallback.py`: simulate watchdog trip via forced `_degraded = True`; assert no `transcript_refine` emitted until `_degraded = False`

## 10. Tests

- [ ] 10.1 `tests/test_refine_alignment.py` (unit): coverage from task 5.6
- [ ] 10.2 `tests/test_refine_trigger.py` (unit): `get_refine_candidate` returns on gap, returns on monolog cap, returns `None` when no un-refined audio, watermark advances correctly
- [ ] 10.3 `tests/test_per_speaker_refine.py` (e2e, marker `needs_audio_loopback`): scripted 15-s dialogue with 5-s overlap on two synthetic guests; after 20 s of idle, assert ≥ 50% of tier-1 segments promoted to `refined: true` AND aggregate WER against reference drops by ≥ 5% absolute
- [ ] 10.4 `tests/test_meeting_storage.py`: new coverage for `update_segments({text, refined: true})`
- [ ] 10.5 Regression: full suite `python -m pytest tests/ -v -m "not e2e"` stays green

## 11. Benchmarking + validation

- [ ] 11.1 New harness `scripts/refine_benchmark.py`: replays recorded per-pid WAVs through `PerSpeakerIngest` + `RefineWorker`; outputs per-segment live-WER and refined-WER against a reference transcript, plus overall avg
- [ ] 11.2 Run on take-2 fixture; acceptance: overall refined-WER ≤ 25% (vs live 33% baseline); write results to `experiments/2026-04-XX-stream-refine.md`
- [ ] 11.3 Latency measurement: `scripts/refine_benchmark.py --latency-profile` with 4 simulated pids in parallel; first-paint p50/p95 must regress ≤ 100 ms vs baseline live-only
- [ ] 11.4 If 11.2 or 11.3 fail acceptance thresholds, flag in `experiments/2026-04-XX-stream-refine.md` and do NOT proceed to default-flip task 12.6; iterate design

## 12. Rollout + docs

- [ ] 12.1 Sanity filter in `_apply_refine` (per D12): word-count-delta and Levenshtein guards with reject counters; unit test in `tests/test_refine_alignment.py` covering pass/reject on synthetic inputs
- [ ] 12.2 Runtime toggle in `/api/pcm-diagnostics` (per D12): `POST {"refine_enabled": bool}` wired to `RefineWorker.pause()` / `resume()`; `GET` reports current enabled state and `refine.rejected_sanity` counter
- [ ] 12.3 Write `adr/ADR-0019-stream-refine.md` summarising decisions D1–D12 and benchmark results
- [ ] 12.4 Update `CLAUDE.md` Architecture section: refine contour, tier-1/tier-2 UI, feature flag
- [ ] 12.5 `.env.example`: add `RECOG_PCM_REFINE=0` with one-line explanation
- [ ] 12.6 After one week of opt-in on at least 5 real meetings, flip default to `RECOG_PCM_REFINE=1` in `recog/config.py::pcm_refine_enabled()`; keep env override for rollback
- [ ] 12.7 `openspec archive per-speaker-stream-refine` once tasks 0–11 are complete and manual smoke passes
