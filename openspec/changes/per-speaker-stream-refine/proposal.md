## Why

The per-speaker path (`RECOG_PCM_TRANSPORT=1`, ADR-0018) achieves ~33% WER on the grid-search benchmark `experiments/2026-04-23-gigaam-grid.md`. On 20-second chunks GigaAM reaches ~24% WER — the model wants more context to resolve boundary words. Bumping `chunk_duration` to 10–20 s (Option A) trades user-visible first-paint latency for quality: unacceptable. This change (Option B) keeps the live 3-s path untouched and adds a parallel rolling-window refinement pass that re-transcribes each recent reply with 20 s of context, then replaces the live text in both the UI and `transcript.jsonl`. Target: drop overall WER from ~33% to ~24% (~9% absolute) without changing first-paint latency.

Upstream GigaAM (`salute-developers/GigaAM origin/main` through commit `6e4b027`) now provides **word-level timestamps** (PR #65, `d427e5a`) and a faster **batched RNN-T decoder** (PR #67, `a755fec`). Word-level timestamps are the architectural prerequisite: they let us align a 20-s refined transcript back onto the live-segment boundaries without heuristic Levenshtein matching. This change is gated on that upstream merge.

## What Changes

- **Upstream GigaAM upgrade** to latest `origin/main`. Adapt `ASRModule.transcribe_batch` to the new `decoding.decode()` return signature.
- **Metal (MPS) backend** for GigaAM inference — auto-detected where available. Grid-search already verified bit-identical WER on CPU vs MPS; this change consumes the freed CPU for refine.
- **Per-pid rolling PCM ring buffer** (~20 s) inside `PerSpeakerIngest`, shadowing the live path without blocking it.
- **`RefineWorker` background thread** (new module `recog/refine.py`) sharing the live `GigaAM` instance under a mutex; live-batch has priority via cooperative abort.
- **Trigger**: gap-driven on `SPEAKER_TURN_GAP = 3 s` with a fallback timer (`PCM_REFINE_MONOLOG_CAP_S = 15 s`) for continuous monologues.
- **Word-level alignment**: map each live segment's `(audio_offset_ms, duration_ms)` onto the refined word timeline; the collected words become the new `text`.
- **Inline JSONL rewrite** via `MeetingStorage.update_segments` (already used by retroactive speaker correction); `text` field added to the update API; new boolean `refined` on `TranscriptSegment`.
- **New WS message type** `transcript_refine` with payload `{segment_id, text, refined_at}`. Broadcast on both `ws://localhost:8765` (extension overlay) and `ws://127.0.0.1:9080` (webapp).
- **Tier-1 / tier-2 UI**: live-only rows render with opacity 0.6 and a dashed accent; on refine arrival the row rewrites in place and promotes to solid (tier-2). Extension overlay gets a simpler fade-in/out with no tier marker (space-constrained).
- **Feature flag** `RECOG_PCM_REFINE=1`, default OFF. No-op when `RECOG_PCM_TRANSPORT=0`.
- **Fallback_mixed mode**: refine is disabled (per-speaker audio does not exist there).

## Capabilities

### Modified Capabilities

- `per-speaker-transcription`: gains rolling-window refinement, word-timestamp alignment, inline JSONL rewrite semantics, `transcript_refine` WS message, and tier-1/tier-2 visual lifecycle. The existing live 3-s behaviour is unchanged; it becomes the initial (tier-1) state of segments that may later transition to tier-2.

### New Capabilities

None.

## Impact

**Code:**
- `recog/asr.py` — adapt `transcribe_batch` to new upstream return type; new `transcribe_with_timestamps()` helper.
- `recog/per_speaker.py` — per-pid rolling PCM ring; refine-candidate detector (gap + monolog).
- `recog/refine.py` (new) — `RefineWorker` with cooperative-abort mutex.
- `recog/live.py` — wire refine worker, new `_apply_refine` method, `_pcm_live_segments` per-pid segment registry.
- `recog/config.py` — `PCM_REFINE_WINDOW_S`, `PCM_REFINE_MONOLOG_CAP_S`, `pcm_refine_enabled()`.
- `meeting_storage.py` — extend `update_segments` to accept `text`; add `refined: bool` on `TranscriptSegment`.
- `ktalk_bridge.py` — `send_refine(updates)`, `transcript_refine` WS type.
- `frontend/server.py` — forward `transcript_refine` to webapp clients.
- `frontend/static/` — tier-1/tier-2 CSS + JS handler; `data-segment-id` / `data-refined` attributes.
- `kontur-diarize/ktalk-speaker-extension/src/` — overlay UI handler for `transcript_refine`.
- `scripts/refine_benchmark.py` (new) — replays per-pid WAVs through the full contour, measures live-WER vs refined-WER.
- `tests/` — unit tests for alignment + trigger + replace; e2e on scripted overlap dialogue.

**APIs:**
- WS:8765 and WS:8766 gain `transcript_refine` (additive; unknown-type handlers already drop).
- `transcript.jsonl` gains `refined: bool` field on each row (additive; dataclass-based readers tolerate).
- `update_segments` accepts an optional `text` key (backwards-compatible).

**Dependencies:**
- Pin `gigaam` to `origin/main @ 6e4b027` or later. No new PyPI packages.

**Operational:**
- Memory: shared `GigaAM` instance (no doubling). Per-pid rings ≈ 60 KB × 20 s × 5 pids ≈ 6 MB peak.
- CPU: refine load ≈ 5 × (20 s × RTF 0.03) = 3 s wall every 10 s (~30% of one core on CPU). On MPS substantially lower — benchmarked in tasks.
- First-paint latency: **unchanged in cooperative-abort mode; bounded regression in coarse mode.** In cooperative mode (task 2.3 spike confirms decoder hook) live waits ≤ 500 ms for refine to abort. In coarse mode live waits up to one refine window (≤ 3 s CPU, ≤ 0.7 s MPS). Refined text typically arrives within `SPEAKER_TURN_GAP + refine_wall ≈ 3.5 s` after a reply ends.

**Non-goals:**
- Changing live `chunk_duration` (stays at 3.0 s).
- Alternative ASR backends (Whisper, etc.).
- Streaming RNN-T decoder with partial hypotheses (a future Option C).
- GigaAM fine-tuning on Russian meetings.
- Triton / remote ASR workers.
- Re-architecting `transcript.jsonl` into an event log (considered and rejected — see design.md D4).
- Refine for local microphone (stays tier-1 forever; follow-up change if needed — see design.md Open Question 1).
