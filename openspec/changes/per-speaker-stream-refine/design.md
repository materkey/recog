## Context

Grid-search on two reconstructed per-pid WAVs from a real KTalk meeting (`experiments/2026-04-23-gigaam-grid.md`) established:

- Current prod config `chunk=3s, overlap=0.6s` ⇒ **33.2%** avg WER.
- Best offline configs: `chunk=20s, overlap=1.0s` ⇒ 23.9%; `chunk=10s, overlap=2.0s` ⇒ 25.0% avg (22.8% on one track).
- `_remove_duplicate_words` dedup is net-positive and mandatory whenever `overlap ≥ 0.5 s`.
- pyannote longform VAD does **not** beat large fixed chunks on this material — segment selection isn't the lever.
- Full-file one-shot (48 s) is **worse** than 20-s chunks — GigaAM's training distribution is centred on ~20 s windows.
- MPS (Metal) produces bit-identical transcripts to CPU on tested configs (10/0.5, 20/1.0, 10/2.0); the win is purely throughput.

Two concrete paths reduce WER from 33% to 24%:

- **Option A** (rejected in the grid-search doc): bump live `chunk_duration` to 10–20 s. Trade-off: first-paint grows from ~3 s to ~10 s — a visible regression.
- **Option B** (this change): keep live 3-s path as-is; add a parallel refinement pass with 20-s context; replace live text after refine completes.

Option B needs **word-level timestamps** to safely align refined 20-s text back onto individual live-segment boundaries. Without them, alignment is Levenshtein heuristics on word sequences and risks introducing errors that cancel the gain. Upstream GigaAM PR #65 (commit `d427e5a`, now in `origin/main`) added exactly this. This change is **predicated** on consuming that upstream.

**Constraints:**
- Must not regress live-path first-paint latency.
- Must not break AI summary, anchor-response, or audio-offset playback (all keyed on `segment_id`).
- Single MacBook, shared `GigaAM` instance (1.6 GB RSS); no model doubling.
- Opt-in via feature flag for the first week; default flips only after empirical validation.

## Goals / Non-Goals

**Goals:**
- WER drops from ~33% to ~24% on the take-2 benchmark without touching first-paint.
- `segment_id` is stable across refine — downstream consumers unchanged.
- Tier-1/tier-2 UI so users know which text is provisional.
- Graceful degradation: refine failure surfaces live text (tier-1) without regression vs today.

**Non-Goals:**
- Changing live chunk duration.
- Streaming/partial hypotheses from RNN-T (decoder-internal, would conflict with tier-1/tier-2 model).
- ONNX / CoreML export.
- Changes to AI summary, anchor, increment — they read `text` from JSONL and refined updates flow naturally.

## Decisions

### D1. Unit of replacement: 1:1 with live segments
**Choice:** each refine pass emits at most one `(segment_id, new_text)` tuple per live segment that falls inside the refine window.

**Why:** preserves `segment_id` as a stable anchor for AI summary, anchor-response, playback offsets. Word-level timestamps from upstream make this feasible.

**Timeline contract (load-bearing).** Two clocks coexist and MUST NOT be mixed:
- `audio_offset_ms` — offset inside the mixed `audio_system.wav` writer (populated by `self._wav_writer_system.get_offset_ms()` at `recog/live.py:782-837`). Consumed by playback / enrollment. **Not used for refine alignment.**
- `pid_audio_ms` — per-pid monotonic counter of successfully-ingested PCM samples for that pid, incremented by `samples / 16000 * 1000` on every `_pcm_rings[pid].extend(...)` in `PerSpeakerIngest`. Zero at first chunk for a given (meeting, pid) pair.

Each live segment stores both: `audio_offset_ms` (unchanged, mixed wav) and a new `pid_audio_ms_start` / `pid_audio_ms_end` pair (per-pid frame).

`RefineTask` carries `offset_base_ms` = `_pid_audio_ms[pid]` at the moment the trailing N-sample window is snapshotted (i.e., the `pid_audio_ms` of the ring's *oldest* remaining sample). Refined words arrive with `start_ms`/`end_ms` relative to the ring buffer (0 = first sample passed to `transcribe_with_timestamps`); callers rebase via `word_pid_ms = offset_base_ms + word_start_ms`. Alignment to a live segment tests `segment.pid_audio_ms_start <= word_pid_ms <= segment.pid_audio_ms_end`.

For each live segment `{pid_audio_ms_start, pid_audio_ms_end}`, collect refined words whose rebased `pid_audio_ms` falls inside `[pid_audio_ms_start, pid_audio_ms_end]` and join them as the new `text`.

**Alternative — N→1 coalesce** (collapse multiple live segments into one refined segment): rejected. Invalidates downstream IDs, forces schema changes in every consumer (AI summary, anchor-response, audio playback).

**Alternative — N→M coalesce along `cont=True` groups**: rejected as default. Kept as a minor fallback behaviour (D8): if a live segment ends up with an empty word list post-alignment, it stays tier-1 rather than getting a synthesised empty replacement.

### D2. Audio source: per-pid in-memory ring, not `audio_system.wav`
**Choice:** maintain `deque` of raw Int16 samples per active pid inside `PerSpeakerIngest`, capacity ≈ `PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S ≈ 23 s`.

**Why:** `audio_system.wav` is the mixed BlackHole/ScreenCap signal — refining it would reintroduce cross-talk garbling that the per-speaker pipeline was built to avoid. Rings add ~60 KB × 20 s × 5 pids ≈ 6 MB RAM; trivial. In `fallback_mixed` mode per-speaker audio doesn't exist, so refine is disabled there (D7).

### D3. Trigger: gap-driven with monolog fallback
**Choice:**
1. **Gap trigger:** when a pid has been silent ≥ `SPEAKER_TURN_GAP = 3.0 s` AND has un-refined audio in its ring, run refine on the just-closed reply (all PCM since the previous refine boundary or pid activation, clipped to 20 s).
2. **Monolog fallback:** if a pid speaks continuously ≥ `PCM_REFINE_MONOLOG_CAP_S = 15.0 s` without a `SPEAKER_TURN_GAP`-sized pause, run refine on the trailing 20-s window even without a gap.

**Why gap-driven:** refine is most valuable at reply boundaries. GigaAM's context-hungry mistakes happen on edge words; one refine per reply gives predictable load and a semantically coherent replacement unit.

**Why 15 s monolog cap:** an uninterrupted speaker would otherwise starve refine. At 15 s the tier-1 text is already stale; a preemptive 20-s refine is worth the occasional mid-reply replace.

**Single parameter reuse:** same `SPEAKER_TURN_GAP` that governs `is_continuation` and UI-bubble boundaries. One source of truth for "end of reply." If operational data later shows the concepts need to diverge, we factor out `PCM_REFINE_TRIGGER_GAP` in a follow-up — not now (YAGNI).

### D4. JSONL schema: inline `text` rewrite + `refined: bool`
**Choice:** extend `MeetingStorage.update_segments` to accept a `text` key; add `refined: bool = False` field to `TranscriptSegment`. Refine applies via inline JSONL line rewrite for the given `segment_id`. `id`, `ts`, `speaker`, `audio_offset_ms`, `cont` stay untouched.

**Why:** `update_segments` already exists and is proven safe under concurrent access (`_retroactively_correct_unknown_speakers` uses it; it holds `_segment_lock` across a read-rewrite-write cycle). Reusing a hardened path beats introducing a new append-with-superseded schema.

**`refined: bool` purpose:**
- Lets UI tell tier-1 from tier-2 without session-level state.
- Enables per-tier WER metrics in benchmarking.
- Surfaces in AI-summary prompts if we ever want the LLM to know "this was auto-corrected".

**Alternative — additive with `superseded_by`**: rejected. Complicates every reader, doubles JSONL size, no user scenario calls for showing the pre-refine text.

**Alternative — append-only `refined.jsonl` alongside `transcript.jsonl`**: rejected. UI would always show 33%-WER text unless it also reads the refined file; two sources of truth for one logical transcript.

### D5. UI: tier-1 / tier-2 visual lifecycle
**Choice:** webapp frontend renders tier-1 rows (`refined: false`) with opacity 0.6 and a dashed accent border; on `transcript_refine` the row rewrites in place, flips to `data-refined="true"`, animates opacity to 1.0, border becomes solid. Extension overlay receives the same message but renders a plain fade-out/fade-in text swap without tier markers (space-constrained widget; tier styling would add noise).

**Why:** silent in-place rewriting ("magic correction") is the alternative. Rejected: users already absorbed the first (wrong) text and won't re-read. An explicit cue signals "still provisional" without being an error banner.

**Tooltip copy:** tier-1 rows carry "Предварительная расшифровка — будет уточнена" (or localisation equivalent). Users who don't hover still infer from the opacity that these are different.

### D6. GigaAM instance management: one model, mutex, live-priority
**Choice:** one shared `GigaAM` instance (already loaded for live). `RefineWorker` acquires `threading.Lock` before each inference. Live-batch acquisition is non-blocking: it sets a `live_pending` flag that the refine worker polls cooperatively between encoder frames; on flag set, refine aborts, releases the lock, and re-enqueues its task.

**Why not two instances:** doubling RAM (~3.2 GB combined) is real on M1 Pro 16 GB machines and risks swap. Mutex serialisation is simpler and the bench shows refine wall-time of 300–600 ms on MPS — an acceptable worst-case live wait.

**Why cooperative abort rather than preempt:** PyTorch/MPS have no built-in inference preemption. Cooperative abort requires hooking into the upstream RNN-T decoder's step loop. If upstream doesn't expose this, we settle for coarse-grained priority: refine holds the lock for one full 20-s inference (~0.6 s MPS, ~3 s CPU) and live waits. Live wait of ≤ 3 s every 10 s is a visible but bounded regression in worst case; practice is usually under 1 s. Iterate in follow-up if needed.

### D7. Fallback_mixed: refine disabled
**Choice:** when `PerSpeakerIngest._degraded == True`, refine stops emitting tasks; any queued tasks are dropped. On recovery, refine resumes from the next gap trigger.

**Why:** in `fallback_mixed` the only audio available is mixed BlackHole/ScreenCap — refining it would reintroduce overlap garbling. Users already see a `[PCM silence → mixed fallback]` system marker; no new UX signal needed for "refine off." Tier-1 rows written during fallback simply never promote to tier-2.

### D8. Alignment edge cases
- **Empty word list post-alignment:** refined result had no words falling inside the live segment's audio range. Leave segment tier-1; log a counter — persistent empties indicate alignment drift.
- **Refined text identical to live `text`:** no-op; do not emit `transcript_refine`, do not flip `refined`. Avoids UI flicker and write amplification.
- **Refined text is a subset/superset of live text:** emit. Tier-2 signals "verified with context," even if the change is small.
- **Meeting switch between refine dispatch and apply:** drop the refine. Same `_meeting_generation` snapshot pattern as `_on_pcm_batch`.
- **Refine arrives after segment was already speaker-corrected by `_retroactively_correct_unknown_speakers`:** both updates commute under `_segment_lock`. Only `text` and `refined` change; `speaker` / `uncertain` are untouched.

### D9. Ordering
Refines may complete out of order across pids (pid A's result may land before pid B's older result). `update_segments` is idempotent at the `id` level — no ordering requirement for correctness. UI rows are keyed by `segment_id`; tier promotion is stable regardless of arrival order.

### D10. Webapp transport: bridge-originated push, not JSONL tail
**Choice:** `frontend/server.py` subscribes to `ktalk_bridge` on startup (via a new `ws://localhost:8766` client connection, reusing the WS server the extension already binds) and forwards `transcript_refine` and `transcript` messages to webapp clients on `ws://127.0.0.1:9080`. The existing JSONL tail watcher remains the source of truth for **append** events during cold-start replay on reconnect, but is NOT used for refine propagation.

**Why:** `frontend/server.py:183-208` tails `transcript.jsonl` with a strictly forward `f.seek(self.file_position); readlines(); self.file_position = f.tell()`. `MeetingStorage.update_segments` writes via `tmp_path.write_text(...) + os.replace(...)` (meeting_storage.py:726-728) — a full file rewrite. After a rewrite, `file_position` points past EOF or mid-line of the new file, so `_notify_new_segments` silently no-ops. This is a latent bug on the existing retroactive-speaker-correction path; refine would hit it every few seconds and the UI would never see tier-2.

The bridge already holds the refine payload in memory before writing JSONL (sequence: `_apply_refine` → `MeetingStorage.update_segments` → `bridge.send_refine`). Forwarding from the bridge is lossless, ordered with `send_transcript`, and decoupled from file-system semantics.

**Cross-process note:** `ktalk_bridge.py` and `frontend/server.py` run in separate processes under launchd. The subscription is a standard WS client inside `frontend/server.py`'s startup path; no shared-memory dance. Reconnect loop (exponential backoff) handles bridge restarts.

**Alternative — switch the tail watcher to watermark-comparison over segment IDs:** rejected. Would require re-reading the whole file after every rewrite (O(N) per refine), and the retroactive-correction path would still be on the same fragile primitive. A dedicated push channel is simpler and fixes both paths at once.

**Alternative — keep the tail watcher, detect rewrite via inode change and re-read from `file_position = 0`:** rejected. Still O(N) and the `os.replace` happens atomically — by the time we notice the new inode, some prefix has already been rebroadcast to clients (duplicate `segments` messages).

### D11. State-ownership map for the refine contour
**Choice:** state added by this change is partitioned by owning class along these lines — no shared mutation:

- **`PerSpeakerIngest`** owns per-pid ring + trigger counters. Fields: `_pcm_rings`, `_silence_ms`, `_continuous_speech_ms`, `_pid_audio_ms` (from D1 Timeline contract). Exposed through `get_refine_candidate(pid) -> Optional[RefineTask]` and `get_pid_audio_ms(pid) -> float`. Never reads from `LiveTranscriber`.
- **`LiveTranscriber`** owns the live-to-refine segment bridge. Fields: `_pcm_live_segments`, `_pcm_prev_words`, `_pcm_pid_state`. Populated by `_on_pcm_batch` (live path), read by `_apply_refine` (refine path). Both under `_state_lock`.
- **`RefineWorker`** owns queue + worker thread. Does not import `LiveTranscriber`; receives `transcribe_fn` and `apply_fn` callables via constructor. `apply_fn` is `LiveTranscriber._apply_refine` bound at wiring time; the worker sees only "give me a transcribe function and a result sink".
- **`MeetingStorage`** owns JSONL I/O. `_apply_refine` calls `update_segments` (already hardened for concurrent access). No refine-specific code path inside `MeetingStorage`.
- **`KTalkBridge`** owns WS egress. Gains `send_refine(updates)`. Never reads `LiveTranscriber` state.

**Why:** multiple per-pid dicts across two classes (5 total) is a noise threshold for "implicit contract creep." Writing down the ownership map keeps follow-up changes from accidentally snaking refine state into components that shouldn't know about it. The surface area is narrow — three callables (transcribe, apply, bridge.send_refine) and one accessor (`get_pid_audio_ms`) — and stays that way.

### D12. Runtime guard rails
**Choice:** two small additions guard against user-visible refine misbehaviour without requiring a service restart:

1. **Sanity filter in `_apply_refine`.** Before calling `update_segments`, compare refined `new_text` to the live segment's current `text`:
   - If `abs(len(new_text.split()) - len(live_text.split())) > max(3, len(live_text.split()))` (3× word-count delta heuristic), reject.
   - If `Levenshtein.distance(new_text, live_text) > max(len(live_text), len(new_text)) * 0.8`, reject.
   - Rejections increment `refine.rejected_sanity` counter (visible via `/api/pcm-diagnostics`). Segment stays tier-1.
2. **Runtime toggle.** Extend `/api/pcm-diagnostics` to accept `POST {"refine_enabled": bool}`. When `false`, the live `RefineWorker.pause()` is called and no new tasks are submitted. When `true`, `resume()` plus a one-shot `pcm_refine_enabled()` re-check. Env flag remains the cold-start source of truth; toggle is in-memory only (resets on restart).

**Why:** the offline benchmark (task 11.2) catches gross regressions on a fixed fixture, but real-world meetings can still surface bad decodes. The sanity filter is one-line-of-code cheap; the runtime toggle reuses an endpoint that already exists (commit `69423c3`) and matches how we'd operationally handle a live incident.

**Thresholds rationale:** 3× word-count and 0.8× Levenshtein are deliberately loose — they reject only catastrophes (e.g., refine produces 100 words for a 10-word live segment, or two completely unrelated strings). Normal refinements (word substitutions, boundary-word fixes) pass trivially.

## Risks / Trade-offs

- **[Refine load starves live under multi-pid storm]** → D6 cooperative abort + benchmark with 5 simulated pids (task 11.3). Acceptance threshold: first-paint p95 regression ≤ 100 ms.
- **[MPS numerical drift on new `word_timestamps=True` API]** → grid-search confirmed bit-identical text on old API. New API is untested on MPS. Task 1.2 compares MPS vs CPU on take-2 fixture before enabling MPS default.
- **[Word-timestamp precision on short chunks]** → upstream reports `frame_shift ≈ 10–40 ms`. We align against 3.6 s live segments; precision margin is 2 orders of magnitude. Not a practical concern.
- **[Users read tier-1 styling as a bug/loading state]** → UX copy via tooltip. Iterate if feedback shows confusion.
- **[Upstream GigaAM merge regression]** → task 0.3 runs full unit suite + manual live smoke before refine contour is wired.
- **[JSONL race between refine-rewrite and speaker correction]** → both paths funnel through `update_segments` under `_segment_lock`; operations are field-disjoint and commute.
- **[Refine emits text that is WORSE than live]** → theoretically possible if 20-s decode hits an outlier. Acceptance test (10.3) measures refined-WER; if refined-WER > live-WER on benchmark, we do not ship default-on. Additionally, `_apply_refine` applies a per-segment sanity filter before emitting `transcript_refine` (see D12): refinements whose word-count differs from the live segment's by > 3× OR whose Levenshtein distance from live `text` exceeds `max(len(live), len(refined)) * 0.8` are rejected (segment stays tier-1, metric counter increments). Offers a last-resort guard against catastrophic decoder drift.
- **[No runtime kill-switch once refine is default-on]** → `.env` edit + `./recog-service.sh restart` is slow. `POST /api/pcm-diagnostics {"refine_enabled": false}` (D12) toggles at runtime without restart; flips `RefineWorker.pause()` / `resume()`.

## Migration Plan

1. **Phase 0 — upstream merge:** pull `origin/main` in `../GigaAM`; adapt `ASRModule.transcribe_batch`; full unit suite green; manual live smoke on real KTalk meeting.
2. **Phase 1 — MPS:** auto-detect in `ASRModule.__init__`; re-run grid-search to confirm no WER regression. Default CPU until MPS is confirmed stable on this machine.
3. **Phase 2 — refine contour behind `RECOG_PCM_REFINE=1`:** user opts in for one week. Default OFF.
4. **Phase 3 — default flip:** if one week of real meetings shows measurable WER drop and no first-paint regression, flip default to ON. Flag remains for rollback.
5. **Rollback:** `unset RECOG_PCM_REFINE` → refine contour is a no-op, live path runs as today. Zero data migration.

## Open Questions

1. **Local microphone refine:** today the local mic goes through `SingleDeviceCapture`, not the PCM worklet. Refining it would need a parallel in-memory ring sourced from the mic capture callback. Proposed: out of scope — local mic stays tier-1 forever. Follow-up change can add this if the noise ratio on meetings makes it worthwhile.
2. **"Show original" affordance:** a tooltip or right-click "show pre-refine text" would preserve auditability. Not shipped initially; JSONL does not preserve the old text either (D4). If users ask, consider adding `text_live` alongside `text`.
3. **Cooperative abort granularity:** D6 permits two modes, selected by the task 2.3 spike. If upstream exposes a decoder step hook, we ship `cooperative` mode (live waits ≤ 500 ms). Otherwise we ship `coarse` mode (live waits ≤ 3 s CPU / ≤ 0.7 s MPS). Both are acceptable operating points; the spike only decides which is initial. Follow-up change can add cooperative if upstream adds a hook later.
4. **Refine on reconnect:** if the extension reloads mid-meeting, the ring buffer (server-side) survives; per-pid tracks that vanish have their rings discarded. No user-facing anomaly expected, but worth a manual scenario in task 7.3.
