# Per-Speaker Refine Backend (§2–§5)

**Date**: 2026-04-24
**Status**: Ready for ralphex execution
**Reference OpenSpec**: `openspec/changes/per-speaker-stream-refine/` — this plan covers §2 (Refine worker foundations), §3 (Per-pid rolling PCM ring), §4 (Segment tracking for alignment), §5 (Alignment + replace). The WebSocket transport (§6), UI (§7–§8), fallback_mixed (§9), tests beyond unit level (§10–§11), and rollout (§12) are separate follow-up plans.
**Base branch**: `gigaam-upstream-mps` (not yet merged to main — feature-branch chain).

## Overview

Wire up the complete refine contour on the Python side — no UI, no WS broadcast yet. Goal: when `RECOG_PCM_REFINE=1` and `RECOG_PCM_TRANSPORT=1`, a per-pid rolling PCM buffer fires a `RefineTask` on reply-close (`SPEAKER_TURN_GAP = 3 s`) or monolog cap (`15 s`), a `RefineWorker` runs `transcribe_with_timestamps` on the window, aligned refined text replaces live `text` in `transcript.jsonl` with `refined: true` — all under a shared-model mutex with cooperative abort where upstream supports it.

Downstream surfaces (WS `transcript_refine`, webapp tier-1/tier-2 UI, extension overlay) are the next plan. This plan is complete when: a unit/integration test pipes PCM through `PerSpeakerIngest` → `RefineWorker` → `_apply_refine` → JSONL row rewritten with `refined: true`, all segment IDs preserved.

## Context (from discovery)

- Branch state: currently on `gigaam-upstream-mps` (11 commits ahead of main, pushed to origin). `ASRModule.transcribe_with_timestamps(...)` exists (added in commit `BASE_REVISION`), returns upstream `TranscriptionResult` with word-level timestamps in milliseconds.
- `PerSpeakerIngest` (`recog/per_speaker.py`) already owns per-pid ordering + batching + stats. Its public API is `enqueue`, `get_stats`, `drain`, `start/stop`, `last_drain_generation`. Ring buffer and gap/monolog detection are net-new.
- `LiveTranscriber._on_pcm_batch` (`recog/live.py:744+`) already calls `transcribe_batch` under `_state_lock`, holds `_pcm_prev_words` / `_pcm_pid_state` per-pid. New refine state (`_pcm_live_segments`) lives on `LiveTranscriber` per the design's state-ownership map (D11).
- `MeetingStorage.update_segments` (`meeting_storage.py:672+`) already rewrites JSONL lines by `segment_id` under `_segment_lock`. Extending it to accept `text` (and set `refined: true`) is a minimal diff.
- Timeline contract (design.md D1): live segments now store `pid_audio_ms_start` / `pid_audio_ms_end` in the per-pid frame (maintained in `PerSpeakerIngest._pid_audio_ms`); `audio_offset_ms` (mixed-wav frame) stays for playback but is NOT used for refine alignment.
- Spike on cooperative abort (task 2.3): upstream `gigaam/decoding.py` greedy RNN-T decoder needs inspection. If no step hook — ship `coarse` mode (live waits ≤ one refine window).

## Development Approach

- **Testing approach**: Regular — unit tests per module + a smoke integration test that exercises the full contour with a synthetic PCM stream.
- Commit after each logical step. Don't batch unrelated concerns.
- Feature-flagged: `RECOG_PCM_REFINE=1` default OFF; `pcm_refine_enabled()` returns False unless both `RECOG_PCM_TRANSPORT` and `RECOG_PCM_REFINE` are set. Live path unchanged when flag is off.
- When touching `recog/per_speaker.py` / `recog/live.py`, hold the respective lock consistently with surrounding code.

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Config constants and enable-gate

- [x] 1.1 In `recog/config.py`, add:
  - `PCM_REFINE_WINDOW_S: float = 20.0`
  - `PCM_REFINE_MONOLOG_CAP_S: float = 15.0`
  - `def pcm_refine_enabled() -> bool` reading `os.getenv("RECOG_PCM_REFINE", "0") == "1"`. MUST also require `pcm_transport_enabled()` — return False if either is off.
- [x] 1.2 Unit test in `tests/test_config.py` (create if missing): verify `pcm_refine_enabled()` returns False unless BOTH env flags are set to `"1"`. Test via monkeypatch.
- [x] 1.3 Commit: `feat(config): add PCM refine constants and enable-gate`

### Task 2: Spike on cooperative abort hook in upstream decoder

- [x] 2.1 Read `/Users/vkkovalev/projects/GigaAM/gigaam/decoding.py` — the greedy RNN-T decoder's `decode()` step loop (post-upstream, `6e4b027`). Identify whether there's a natural poll point per decoder step (e.g., an iteration over frames/timesteps).
- [x] 2.2 Prototype a monkey-patched `step_callback` approach: wrap the decoder's inner loop with a hook that returns early if a `should_abort()` callable returns True. Try it on a short wav, measure abort latency.
- [x] 2.3 Record outcome in `experiments/2026-04-24-refine-abort-spike.md`:
  - Section "Can we hook the decoder step loop?" — yes/no with evidence.
  - If yes: expected abort latency (ms), code snippet of the monkey-patch.
  - If no: justification (upstream API shape), recommend `coarse` mode.
- [x] 2.4 Choose `abort_mode` default for `RefineWorker`. Update `docs/plans/2026-04-24-per-speaker-refine-backend.md` (this file, task 4.1 below) to reflect the chosen default.
- [x] 2.5 Commit: `exp: spike on cooperative-abort hook for RefineWorker (mode=<cooperative|coarse>)`

### Task 3: RefineTask dataclass and recog/refine.py module scaffold

- [x] 3.1 Create new file `recog/refine.py` with:
  - Dataclass `RefineTask(pid: str, pcm_samples: np.ndarray, offset_base_ms: float, submitted_at: float)` — immutable, carries one pid's trailing window for refine.
  - Dataclass `RefineResult(task: RefineTask, text: str, words: list[Word])` — where `Word` is imported from gigaam.
  - Empty `RefineWorker` class with `__init__(model_lock, transcribe_fn, apply_fn, abort_mode: Literal["cooperative","coarse"] = "...")` (default from 2.4), `submit/pause/resume/stop` methods as stubs raising `NotImplementedError`. Flesh out in Task 4.
- [x] 3.2 Unit test scaffold `tests/test_refine.py`: import symbols from `recog.refine`, assert dataclass construction works.
- [x] 3.3 Commit: `feat(refine): add refine module scaffold with dataclasses`

### Task 4: RefineWorker implementation

- [x] 4.1 Implement `RefineWorker`:
  - Constructor stores `model_lock: threading.Lock`, `transcribe_fn: Callable[[np.ndarray], TranscriptionResult]`, `apply_fn: Callable[[RefineResult], None]`, `abort_mode: Literal["cooperative","coarse"] = "cooperative"` (decision from Task 2 spike: decoder loop is ~80% of inference cost on MPS — ~767ms saved per aborted call; see `experiments/2026-04-24-refine-abort-spike.md`), internal `_queue: queue.Queue`, `_paused: bool`, `_live_pending: threading.Event`, `_stopped: threading.Event`, `_thread: threading.Thread`.
  - `start()` spawns daemon worker thread.
  - `submit(task)` puts onto queue; no-op if paused or stopped.
  - `pause()` sets `_paused=True`, drains queue (drop pending tasks — they'll be re-triggered on next gap).
  - `resume()` clears `_paused`.
  - `stop()` sets `_stopped`, waits for thread to join.
  - `set_live_pending()` / `clear_live_pending()` flip the Event — exposed for `LiveTranscriber` to signal priority.
- [x] 4.2 Worker loop: pop task, acquire `model_lock` (blocking), call `transcribe_fn`. In `cooperative` mode, pass a `should_abort=lambda: self._live_pending.is_set()` closure (the concrete hook-patching lives behind the `transcribe_fn` caller). In `coarse` mode, just run to completion.
- [x] 4.3 On abort: release lock, re-enqueue the task.
- [x] 4.4 On success: release lock, call `apply_fn(result)` (outside lock).
- [x] 4.5 Swallow + log exceptions in worker loop — do NOT kill the thread.
- [x] 4.6 Commit: `feat(refine): implement RefineWorker with cooperative/coarse abort`

### Task 5: RefineWorker unit tests

- [x] 5.1 In `tests/test_refine.py`:
  - `test_submit_runs_transcribe_then_apply`: mock `transcribe_fn`, `apply_fn`; submit one task; assert both called in order.
  - `test_pause_drops_queued_tasks`: submit → pause → assert queue empty, worker idle.
  - `test_live_pending_aborts_cooperative`: in cooperative mode, simulate `transcribe_fn` that honours `should_abort`; set `live_pending`; assert task re-enqueued.
  - `test_coarse_mode_ignores_live_pending`: in coarse mode, `transcribe_fn` completes without checking abort; task NOT re-enqueued.
  - `test_stop_joins_cleanly`: `stop()` completes within 1 s.
- [x] 5.2 Run: `venv-gigaam/bin/python -m pytest tests/test_refine.py -v` — all green.
- [x] 5.3 Commit: `test(refine): RefineWorker submit/pause/resume/stop/abort coverage`

### Task 6: Per-pid rolling PCM ring in PerSpeakerIngest

- [x] 6.1 In `recog/per_speaker.py::PerSpeakerIngest.__init__`, add:
  - `self._pcm_rings: Dict[str, deque[np.ndarray]] = {}` — ring of Int16 sample arrays per pid. Ring capacity in seconds: `PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S + 3.0` (small head room); enforce size on append.
  - `self._silence_ms: Dict[str, float] = {}` — per-pid silence counter.
  - `self._continuous_speech_ms: Dict[str, float] = {}` — per-pid continuous-speech counter.
  - `self._pid_audio_ms: Dict[str, float] = {}` — per-pid monotonic audio counter (D1 timeline contract).
  - `self._refine_watermark_ms: Dict[str, float] = {}` — per-pid pid_audio_ms of the oldest un-refined sample. Starts at 0 on first chunk.
- [x] 6.2 In `enqueue(...)` (or the drain path — wherever samples are first committed to shared state), append raw samples to `_pcm_rings[pid]`, increment `_pid_audio_ms[pid]` by `len(samples) / sample_rate * 1000`, enforce ring capacity (drop-oldest). Reset `_silence_ms[pid]=0`, bump `_continuous_speech_ms[pid]` by chunk ms.
- [x] 6.3 Add `tick()` method (or inline into `_worker` drain loop) that on every `PCM_BATCH_INTERVAL_MS` iteration with no new chunks for a pid: increment `_silence_ms[pid]` by `PCM_BATCH_INTERVAL_MS`; if `_silence_ms[pid] >= SPEAKER_TURN_GAP * 1000`, reset `_continuous_speech_ms[pid]=0`.
- [x] 6.4 Add accessor `get_pid_audio_ms(pid: str) -> float` — returns current `_pid_audio_ms.get(pid, 0.0)` (used by `LiveTranscriber._on_pcm_batch` to stamp live segments with per-pid bounds).
- [x] 6.5 Thread safety: all reads/writes under `self._lock` (same lock that guards `_pending` today). Do NOT introduce a second lock.
- [x] 6.6 Unit test in `tests/test_per_speaker.py`:
  - `test_ring_caps_at_window_plus_head_room`: feed 30 s worth of samples; ring size <= capacity.
  - `test_pid_audio_ms_monotonic_per_pid`: two pids increment independently.
  - `test_silence_ms_resets_on_new_chunk`: counter increments under idle, resets on chunk.
- [x] 6.7 Commit: `feat(per_speaker): per-pid rolling PCM ring + silence/speech/audio counters`

### Task 7: get_refine_candidate trigger logic

- [x] 7.1 Add `PerSpeakerIngest.get_refine_candidate(pid: str) -> Optional[RefineTask]`:
  - Returns None if no un-refined audio (watermark == pid_audio_ms).
  - Gap trigger: if `_silence_ms[pid] >= SPEAKER_TURN_GAP * 1000`, assemble trailing window — PCM from watermark to pid_audio_ms, capped at `PCM_REFINE_WINDOW_S * 1000`. Compute `offset_base_ms = max(0, pid_audio_ms - samples_duration_ms)`. Advance watermark to pid_audio_ms. Return `RefineTask(pid, pcm_samples, offset_base_ms, submitted_at=time.time())`.
  - Monolog fallback: if `_continuous_speech_ms[pid] >= PCM_REFINE_MONOLOG_CAP_S * 1000`, same window computation, same watermark advance, plus reset `_continuous_speech_ms[pid]=0` to avoid re-firing next tick.
  - Under `self._lock` atomically (watermark advance + task creation must not interleave with `enqueue`).
- [x] 7.2 Unit test in `tests/test_per_speaker.py`:
  - `test_gap_trigger_returns_window_and_advances_watermark`.
  - `test_monolog_trigger_fires_at_cap`.
  - `test_no_candidate_when_no_unrefined_audio`.
  - `test_watermark_advance_prevents_double_fire` (call twice, second returns None).
- [x] 7.3 Commit: `feat(per_speaker): get_refine_candidate trigger logic (gap + monolog)`

### Task 8: Live-segment registry in LiveTranscriber

- [x] 8.1 In `LiveTranscriber.__init__`, add `self._pcm_live_segments: Dict[str, List[Tuple[int, float, float]]] = {}` — per-pid list of `(segment_id, pid_audio_ms_start, pid_audio_ms_end)`.
- [x] 8.2 In `_on_pcm_batch`, AFTER `append_segment(...)` returns a `segment_id`:
  - Read `pid_audio_ms_now = self._per_speaker_ingest.get_pid_audio_ms(pid)`.
  - Compute `chunk_duration_ms = len(pcm_samples) / sample_rate * 1000` (already available — this is the current chunk's length).
  - Append `(segment_id, pid_audio_ms_now - chunk_duration_ms, pid_audio_ms_now)` to `self._pcm_live_segments[pid]`.
- [x] 8.3 Periodic prune: after appending, drop entries whose `pid_audio_ms_end < pid_audio_ms_now - (PCM_REFINE_WINDOW_S + 5) * 1000`. Cap memory.
- [x] 8.4 All reads/writes under `self._state_lock` (the existing lock around per-pid state).
- [x] 8.5 Unit test in `tests/test_live_segment_registry.py`:
  - `test_registry_appends_on_emit`.
  - `test_registry_prunes_old_entries`.
  - `test_registry_partitioned_by_pid`.
- [x] 8.6 Commit: `feat(live): per-pid live-segment registry for refine alignment`

### Task 9: Alignment + _apply_refine

- [x] 9.1 Implement `LiveTranscriber._apply_refine(result: RefineResult)`:
  - Rebase every refined `Word` into the per-pid frame: `word_start_pid_ms = result.task.offset_base_ms + word.start` (same for end; words from upstream are in ms ints already per Task 4.1 from the previous plan).
  - Under `self._state_lock` + `_meeting_generation` check (drop if generation drifted):
    - For each `(segment_id, seg_start, seg_end)` in `self._pcm_live_segments[result.task.pid]` that intersects `[result.task.offset_base_ms, result.task.offset_base_ms + window_ms]`:
      - Collect words whose `[word_start_pid_ms, word_end_pid_ms]` lie inside `[seg_start, seg_end]`.
      - `new_text = " ".join(w.text for w in collected)`; normalise whitespace.
      - Skip if empty OR byte-identical to the segment's current `text` (look up from storage — keep a small cache of last N segment texts on LiveTranscriber to avoid JSONL re-reads).
      - Collect into `updates: list[dict]` with `segment_id`, `text`, `refined=True`.
  - Call `self._meeting_storage.update_segments(updates)` with a `meeting_path` snapshot.
- [x] 9.2 Wire `RefineWorker`'s `apply_fn` parameter to `self._apply_refine` in whatever setup method builds the worker (Task 11).
- [x] 9.3 Unit test in `tests/test_refine_alignment.py`:
  - `test_word_rebase_into_pid_frame`.
  - `test_empty_collection_skips_segment`.
  - `test_identical_text_skips_update`.
  - `test_segment_subset_produces_updates_list`.
  - `test_generation_mismatch_drops_result`.
- [x] 9.4 Commit: `feat(live): _apply_refine with word-timestamp alignment`

### Task 10: JSONL schema extension

- [ ] 10.1 In `meeting_storage.py`, add `refined: bool = False` to the `TranscriptSegment` dataclass. Default False — existing rows on reload will deserialise with `refined=False` if the field is missing (use `dataclasses.field(default=False)` and tolerate absent keys on read).
- [ ] 10.2 Extend `MeetingStorage.update_segments(updates, meeting_path=None)` to accept an optional `text` key per row. When present: set the segment's `text` field AND set `refined=True`. Other fields (speaker, uncertain, etc.) still honoured if present in the update dict.
- [ ] 10.3 Unit test in `tests/test_meeting_storage.py`:
  - `test_update_segments_rewrites_text_and_sets_refined_true`.
  - `test_update_segments_preserves_other_fields`.
  - `test_update_segments_without_text_key_still_works` (backwards-compat with speaker-correction path).
  - `test_reload_of_row_with_refined_true_roundtrips`.
- [ ] 10.4 Commit: `feat(storage): update_segments accepts text + refined flag`

### Task 11: Wire refine contour into LiveTranscriber lifecycle

- [ ] 11.1 In `LiveTranscriber._setup_per_speaker_ingest` (or a new sibling `_setup_refine_worker`):
  - Gate on `pcm_refine_enabled()` from `recog/config.py`. If False → no-op, do not build `RefineWorker`.
  - Build `RefineWorker(model_lock=self._asr_lock, transcribe_fn=self.asr.transcribe_with_timestamps, apply_fn=self._apply_refine, abort_mode=<from spike>)`.
  - Start it.
  - Store on `self._refine_worker` for later teardown.
- [ ] 11.2 In the existing `_worker` drain loop of `PerSpeakerIngest` (or wherever per-pid tick happens), add: for each active pid, call `get_refine_candidate(pid)` — if non-None and refine enabled, call `self._refine_worker.submit(task)`.
- [ ] 11.3 In `LiveTranscriber._on_pcm_batch` acquire `self._asr_lock` before `asr.transcribe_batch`; notify `refine_worker.set_live_pending()` before the attempt and `clear_live_pending()` after release. No-op if `self._refine_worker is None`.
- [ ] 11.4 In teardown (`_teardown_session`), call `self._refine_worker.stop()` if set.
- [ ] 11.5 Integration smoke test in `tests/test_refine_integration.py`:
  - Set `RECOG_PCM_TRANSPORT=1` and `RECOG_PCM_REFINE=1`.
  - Build a minimal `LiveTranscriber` with stubbed `asr.transcribe_with_timestamps` returning a fixed `TranscriptionResult`.
  - Feed a synthetic PCM stream via `PerSpeakerIngest.enqueue`, wait for batch + refine cycle, assert `meeting_storage.update_segments` called with `text=<refined>, refined=True`.
- [ ] 11.6 Commit: `feat(live): wire refine contour into LiveTranscriber (gated on RECOG_PCM_REFINE)`

### Task 12: Update OpenSpec tasks.md + tests pass + commit + push

- [ ] 12.1 In `openspec/changes/per-speaker-stream-refine/tasks.md`, mark §2 (2.1–2.6), §3 (3.1–3.7), §4 (4.1–4.4), §5 (5.1–5.6) as `[x]`. Do NOT modify task bodies.
- [ ] 12.2 `openspec validate per-speaker-stream-refine` — must pass.
- [ ] 12.3 Full unit suite: `venv-gigaam/bin/python -m pytest tests/ -v -m "not e2e"` — all green. If any regression surfaces in existing tests (meeting_storage, live, per_speaker), investigate before marking task complete.
- [ ] 12.4 Commit: `plan(openspec): mark stream-refine §2-§5 complete (refine backend)`
- [ ] 12.5 `git push` to origin on the new branch.

## Post-Completion

- `RECOG_PCM_REFINE=1` activates a working backend refine loop: PCM ring → gap/monolog trigger → RefineWorker → transcribe_with_timestamps → alignment → JSONL rewrite with `refined: true`.
- No UI surfaces yet — webapp still shows whatever it shows today (field `refined` is present in JSONL but nothing renders it differently). That's intentional; next plan adds the WS broadcast (§6) and UI (§7–§8).
- `openspec validate per-speaker-stream-refine` still green; §2–§5 boxes show `[x]`.
- Follow-up plan: `2026-04-XX-per-speaker-refine-transport-ui.md` for §6–§8 (WS broadcast + extension overlay + webapp tier-1/tier-2).
- Fallback_mixed integration (§9) and benchmarks/rollout (§10–§12) are later plans after UI lands and we can run live end-to-end.
