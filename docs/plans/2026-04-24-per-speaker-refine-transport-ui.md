# Per-Speaker Refine Transport + UI (§6–§8)

**Date**: 2026-04-24
**Status**: Ready for ralphex execution
**Reference OpenSpec**: `openspec/changes/per-speaker-stream-refine/` — this plan covers §6 (WebSocket transport), §7 (UI — extension overlay), §8 (UI — webapp frontend). Fallback_mixed integration (§9), tests/benchmarks (§10–§11), rollout (§12) are separate follow-up plans.
**Base branch**: `gigaam-upstream-mps` (chain continues).

## Overview

Backend refine contour (§2–§5) is complete: PCM ring → gap/monolog trigger → `RefineWorker` → `transcribe_with_timestamps` → alignment → JSONL rewrite with `refined: true`. But no surface renders the refined text yet. This plan wires three transport/UI layers:

1. `KTalkBridge.send_refine(updates)` emits `transcript_refine` on `ws://localhost:8765`.
2. `frontend/server.py` subscribes to the bridge as a WS client and forwards `transcript_refine` on `ws://127.0.0.1:9080` (per design D10, avoiding the JSONL-tail rewrite-blind bug).
3. Extension overlay and webapp frontend handle `transcript_refine`: overlay does a plain in-place text swap with fade transition; webapp renders tier-1/tier-2 (opacity + dashed border → solid).

## Context (from discovery)

- Branch state: `gigaam-upstream-mps` has all backend changes (commits through `57a652c`). `_apply_refine` in `recog/live.py` calls `meeting_storage.update_segments([{segment_id, text, refined=True}])`, no downstream publishing.
- `KTalkBridge` has `send_transcript` at `ktalk_bridge.py:725+` — mirror its shape for `send_refine(updates: list[dict])`.
- `frontend/server.py:183-208` (`LiveWatcher._notify_new_segments`) is strictly forward-seek (`f.seek(self.file_position); new_lines = f.readlines(); self.file_position = f.tell()`). `MeetingStorage.update_segments` writes via `tmp_path.write_text(...) + os.replace(str(tmp_path), str(transcript_path))` — a full rewrite that invalidates `file_position`. This is the retroactive-correction bug D10 calls out; fix in this plan.
- `frontend/static/app.js:745-748` and `788-808` render segments from `this.segments` array, appended on `type:"segments"` WS events. `data-segment-id` not present today; add it in this plan.
- Extension overlay renders transcript bubbles somewhere in `kontur-diarize/ktalk-speaker-extension/src/` — discover actual render path during task 4.1.
- Extension rebuild: `cd kontur-diarize/ktalk-speaker-extension && npm run build`. Manifest version bump needed (currently `1.3.1`).

## Development Approach

- **Testing approach**: Regular — unit tests for bridge method + a manual UI smoke on webapp (ralphex will do this itself for the webapp via console logs; extension overlay requires live meeting — skip unless trivially testable).
- Commit after each logical step. Don't batch unrelated concerns.
- Feature-flagged throughout: `RECOG_PCM_REFINE=1` gates; `transcript_refine` messages only emitted when refine contour is live.
- Frontend is a separate launchd service from bridge. Frontend must connect to `ws://localhost:8765` as a client (in addition to its own WS server on `ws://127.0.0.1:9080`).

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: KTalkBridge.send_refine method

- [x] 1.1 In `ktalk_bridge.py`, add `send_refine(updates: list[dict])`. Each update dict has `segment_id: int`, `text: str`. The method constructs `{type: "transcript_refine", updates: [{segment_id, text, refined_at}], refined_at_batch: iso8601}` (or per-row `refined_at` — pick one; simpler: one `refined_at` at the envelope level for the batch). Broadcast via the same path as `send_transcript` (to all connected extension clients).
- [x] 1.2 Import `send_refine` callback wiring in `recog/live.py::_apply_refine`: after `meeting_storage.update_segments(updates)` succeeds, call `self._ktalk_bridge.send_refine([{"segment_id": u["segment_id"], "text": u["text"]} for u in updates])`.
- [x] 1.3 Unit test in `tests/test_ktalk_bridge.py`: `test_send_refine_broadcasts_correct_shape` — mock a connected client, call `send_refine`, assert the WS message is `{type: "transcript_refine", ...}` with expected fields. Also `test_send_refine_empty_list_is_noop`.
- [x] 1.4 Commit: `feat(bridge): send_refine broadcasts transcript_refine to extension clients`

### Task 2: Frontend subscribes to bridge as WS client

- [x] 2.1 In `frontend/server.py`, add a startup task: a `asyncio.Task` that connects to `ws://localhost:8765` as a WS client (using `websockets` library — already a dep). On receipt, filter messages by `type`: if `type == "transcript_refine"`, forward to all connected webapp clients on the webapp WS server (port 9080). If `type == "transcript"` — skip (webapp already reads transcripts from JSONL on tail). ⚠️ Bridge actually listens on 8766 (default in `KTalkBridge.__init__`); plan said 8765 — used 8766. URL is overridable via `RECOG_BRIDGE_WS_URL`.
- [x] 2.2 Reconnect loop: if the bridge client connection drops, wait with exponential backoff (start 1 s, double up to 30 s cap) and reconnect. Log each attempt at INFO; do NOT log every retry at INFO — only first after a success and every 60 s.
- [x] 2.3 Handle bridge-down gracefully: do not block frontend server startup if bridge is not yet up. Webapp should still serve transcripts; refine messages will resume when bridge connects.
- [x] 2.4 Unit test: simulate a mock WS bridge that sends `{type: "transcript_refine", updates: [...]}`; assert the frontend's internal `broadcast_to_clients` is called with the forwarded message. If writing async tests is awkward, a lighter integration test is fine.
- [x] 2.5 Commit: `feat(frontend): subscribe to bridge WS and forward transcript_refine to webapp`

### Task 3: Fix JSONL tail rewrite-blind bug (per design D10)

- [x] 3.1 In `frontend/server.py::LiveWatcher._notify_new_segments`, detect file rewrite by comparing `os.stat(path).st_size` to `self.file_position`. If `st_size < self.file_position`, the file shrank (likely `os.replace` of a rewrite): reset `self.file_position = 0` and diff against a seen-id watermark (`self._seen_segment_ids: set[int]`) before broadcasting, so we don't re-broadcast previously-seen segments to webapp clients. Extracted `LiveWatcher` to `frontend/live_watcher.py` so it is testable without the full FastAPI stack.
- [x] 3.2 Also handle inode change via `os.stat(path).st_ino` vs `self._file_inode` — covers the "same length, different inode" rewrite case. `_check_current_meeting` resets the watermark + inode on meeting switch so state does not leak across meetings.
- [x] 3.3 Unit test in `tests/test_frontend_live_watcher.py`: 7 cases covering initial read, pure append, size-shrink rewrite, same-size inode-change rewrite, rewrite+append combo, meeting switch clearing watermark, and rows missing `id`.
- [x] 3.4 Commit: `fix(frontend): detect JSONL rewrite via size-shrink, prevent re-broadcast`

### Task 4: Extension overlay handler for transcript_refine

- [x] 4.1 Discover the extension's transcript-bubble render path. Render paths: `ui.js::updateTranscriptUI` emits `.ktalk-msg > .ktalk-msg-text` (ktalk); `ui-vas3k.js::addVas3kTranscriptLine` emits `.jitsi-transcript-line > .jitsi-transcript-text` (vas3k/Jitsi). Both triggered from `handleTranscript`/`handleTranscriptHistory` in `ui.js` via `ws.js::onmessage`.
- [x] 4.2 Added `segment_id: int = 0` param to `send_transcript` in `ktalk_bridge.py`; emitted only when > 0. Reordered `live.py` call sites so `MeetingStorage.append_segment` runs before `send_transcript` (PCM per-speaker, mic, ktalk fallback paths); correction path already had `seg.meeting_segment_id`. Extension stashes `segment_id` on `state.transcriptLines` entries and renders `data-segment-id` on both `.ktalk-msg` (ui.js) and `.jitsi-transcript-line` (ui-vas3k.js). History path also pulls `id` field from JSONL rows.
- [x] 4.3 Added `handleTranscriptRefine(data)` in `ui.js` — iterates `updates`, rewrites text in place for both ktalk and vas3k paths, mirrors the change into `state.transcriptLines` so full re-renders preserve refined text. Wired via `ws.js::onmessage` on `type === 'transcript_refine'`. Missing bubbles are skipped silently (handles scroll-out / reconnect mid-meeting).
- [x] 4.4 Added `.refine-fade { transition: opacity 100ms; opacity: 0.3; }` in both stylesheets (ui.js for `.ktalk-msg-text`, ui-vas3k.js for `.jitsi-transcript-text`); class is added for 200 ms after each rewrite.
- [x] 4.5 Commit: `feat(extension): handle transcript_refine to rewrite bubbles in place`

### Task 5: Extension rebuild + manifest version bump

- [x] 5.1 `cd kontur-diarize/ktalk-speaker-extension && npm run build` — rebuilds `injected.js`. Verify new handler is present via `grep transcript_refine injected.js`. Bundle grew from 220531 → 224141 bytes; grep confirms `transcript_refine` (2), `handleTranscriptRefine` (3), `refine-fade` (7) occurrences.
- [x] 5.2 Bump `manifest.json` version `1.3.1 → 1.4.0` (minor bump — new feature: tier-2 refined text support).
- [x] 5.3 Commit: `chore(extension): rebuild + bump manifest to 1.4.0`

### Task 6: Webapp — data attributes on transcript rows

- [x] 6.1 Added `data-segment-id` and `data-refined` attributes to live transcript rows in `frontend/static/app.js::live.appendSegments`. `data-segment-id` set from `seg.id` (int) when present; `data-refined` set to `"true"`/`"false"` from `seg.refined` on non-event rows (system events — `event_type` set — skip `data-refined` so Task 7's tier-1/tier-2 CSS won't decorate them). WS payload already carries both fields: JSONL rows serialize from `TranscriptSegment` dataclass via `asdict()` (id int + refined bool default False), and `live_watcher._notify_new_segments` forwards rows unchanged to webapp clients. `get_meeting` (init + meeting_change) does the same. Meeting-view `renderTranscript` deliberately untouched — it groups contiguous same-speaker segments via `groupSegments`, which would make per-segment refine targeting ambiguous; the refine pipeline's UI target is the live view, and historical meeting rows already show refined text because JSONL rows carry the final text when read on page load.
- [x] 6.2 Verified `this.segments` carries `id` and `refined`: `init` msg calls `storage.get_meeting(id)` → `_load_meeting_from_path` reads JSONL rows directly (fields preserved). `segments` msg forwards JSONL rows through `live_watcher`. No surfacing needed — `seg.refined === true` compare defensively coerces `undefined` → `false` for legacy rows predating the field.
- [x] 6.3 Commit: `feat(webapp): add data-segment-id and data-refined on transcript rows`

### Task 7: Webapp — tier-1/tier-2 CSS

- [x] 7.1 Added `.transcript-segment[data-refined="false"]` (opacity 0.6, `border-left-style: dashed`) and `.transcript-segment[data-refined="true"]` (opacity 1, `border-left-style: solid`) in `frontend/static/style.css` (just before `/* --- Segment Audio Playback --- */`). Deviation from the plan template: only override `border-left-style` (not the full `border-left`), so the existing `.speaker-local` / `.speaker-other` `border-left-color` rules keep tinting the bar by speaker. Width stays at the default 3px from the base `.transcript-segment` rule. Transition uses the existing `--transition` (0.2s ease) var. CSS vars used: none new — theme already has `--speaker-local` / `--speaker-other` for the color.
- [x] 7.2 Added `div.title = 'Предварительная расшифровка — будет уточнена'` in `frontend/static/app.js::live.appendSegments` only when `div.dataset.refined === 'false'`. Tier-2 rows get no `title` (DOM default — equivalent to "clear"). System events (event_type rows) skip both `data-refined` and `title`. Task 8 will flip `div.title = ''` alongside the `data-refined="true"` promotion.
- [x] 7.3 Commit: `feat(webapp): tier-1 tier-2 CSS for refined transcript rows`

### Task 8: Webapp — transcript_refine WS handler

- [ ] 8.1 In `frontend/static/app.js`, add handler for WS message `type: "transcript_refine"`. For each entry in `updates`:
  - Find row by `[data-segment-id="{segment_id}"]`.
  - Update its text content with the new `text` (preserve speaker prefix / timestamp formatting if present).
  - Flip `data-refined="true"`.
  - CSS transition handles the visual promotion.
  - Update the in-memory `this.segments[i].text` and `this.segments[i].refined` so that scrolled-out-and-back rows render correctly.
- [ ] 8.2 Handle race: if `transcript_refine` arrives for a segment that isn't in DOM yet (deep scroll or slow render), stash into a `pendingRefines: Map<segment_id, {text, refined_at}>`. When `appendSegments` runs, consult the map and apply before first paint.
- [ ] 8.3 Commit: `feat(webapp): transcript_refine handler with tier promotion and pending-map`

### Task 9: Manual smoke — webapp

- [ ] 9.1 Start services: `./recog-service.sh restart` (with `RECOG_PCM_TRANSPORT=1 RECOG_PCM_REFINE=1` in .env). Open `http://127.0.0.1:9080` in a browser.
- [ ] 9.2 Join a KTalk meeting with at least one other participant; both speak short phrases for 2-3 minutes. Observe the webapp:
  - Live rows appear with dashed border + opacity 0.6.
  - After `SPEAKER_TURN_GAP ≈ 3 s` of silence, rows promote to tier-2 (solid border, opacity 1).
  - Text content may change between live and tier-2.
- [ ] 9.3 If the flow doesn't work end-to-end, diagnose via browser devtools console (add `console.log` in the WS handler temporarily). Once verified, remove diagnostics.
- [ ] 9.4 Note: ralphex may not have access to a real KTalk meeting. If so, ⚠️-flag 9.2 and leave a brief instruction in this file for the user to verify manually.
- [ ] 9.5 No commit unless temporary diagnostics were added and need cleanup.

### Task 10: OpenSpec mark §6–§8 complete

- [ ] 10.1 In `openspec/changes/per-speaker-stream-refine/tasks.md`, mark §6 (6.1–6.3), §7 (7.1–7.4), §8 (8.1–8.5) as `[x]`. Do NOT modify task bodies.
- [ ] 10.2 `openspec validate per-speaker-stream-refine` — must pass.
- [ ] 10.3 Commit: `plan(openspec): mark stream-refine §6-§8 complete (transport + UI)`

### Task 11: Push

- [ ] 11.1 Full unit suite: `venv-gigaam/bin/python -m pytest tests/ -v -m "not e2e"` — all green.
- [ ] 11.2 `git status` — tree clean, commits line up with plan.
- [ ] 11.3 `git push` to origin on the `gigaam-upstream-mps` branch.

## Post-Completion

- `transcript_refine` flows end-to-end: `_apply_refine` → `bridge.send_refine` → WS:8765 (extension) + WS:9080 (webapp via bridge-subscribe).
- Extension overlay rewrites bubble text on refine arrival.
- Webapp shows tier-1 (predварительное) → tier-2 (финальное) lifecycle.
- Bug fixed: JSONL tail watcher no longer stalls after `os.replace` rewrite (benefits both refine and retroactive-correction paths).
- Openspec §6-§8 marked `[x]`; §9 (fallback_mixed integration), §10-§11 (tests/benchmarks), §12 (rollout) remain for follow-up plans.
- Next plan: `2026-04-XX-per-speaker-refine-fallback-bench.md` covering §9 + §10 + §11 (fallback_mixed disables refine + integration tests + benchmarks).
