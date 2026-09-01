---
title: 'Bridge copilot reply guards: meeting_id type check, terminal drop echo, manual-preempt invalidation'
type: 'bugfix'
created: '2026-07-25'
status: 'done'
baseline_revision: 'ead7068190fbcdb211ae5072140c61f79518c1c2'
final_revision: '1c7a540a91394b20d7281c0412294d1038e632bf'
review_loop_iteration: 2
followup_review_recommended: false
context: []
warnings: [multiple-goals, oversized]
---

<intent-contract>

## Intent

**Problem:** Copilot reply routing in `ktalk_bridge.py` misbehaves across staleness boundaries: (DW-5) `copilot_channel_reply` reads `meeting_id` with no type check, so a non-string value silently bypasses the cross-meeting guard and is relayed verbatim; (DW-8) both drop branches (stale-prefix and cross-meeting mismatch) `return` without any signal to the asking UI, which then waits forever; (DW-15) `_preempt_manual_copilot` never invalidates pending manual-chat request ids (stored with meeting id `""`), so a late manual reply is broadcast into the new conference chat after preemption.

**Approach:** Add a sibling-style type check for `meeting_id` in the `copilot_channel_reply` handler (None stays valid); broadcast a terminal `copilot_status {status: "error"}` frame when a *tracked* reply is dropped by either drop branch (both UIs already treat per-request `error` as terminal — no frontend change); mark `""`-valued pending entries `stale:` inside `_preempt_manual_copilot` when a preemption actually happens.

## Boundaries & Constraints

**Always:**
- `meeting_id` of `None` (and absent) remains valid in `copilot_channel_reply` — replies generated before any meeting lifecycle must still relay (existing contract, `recog/copilot_channel.py` declares `Optional[str]`).
- The terminal drop echo must go through `_broadcast_copilot_frame` (extension + subscriber fan-out) with the dropped `request_id`, `status: "error"`, and a non-empty human-readable `message` — same frame shape as the relayed `copilot_channel_status` path.
- Emit the drop echo ONLY when the request id was tracked in `_copilot_pending_requests` (check before popping). Untracked ids (proactive `proactive_<uuid>` replies) must NOT produce a status frame — both UIs append a spurious error bubble for unmatched per-request `error` frames.
- The stale-prefix branch fires only on tracked entries, so it always echoes; the cross-meeting branch echoes only if tracked.
- `_preempt_manual_copilot` keeps its early return when `_manual_copilot_active` is false; the invalidation runs only on an actual preemption, before/around the existing subscriber notice — ordering with the notice is not observable and either order is fine.
- Stale-marking for manual entries follows the existing convention: value becomes `f"stale:{mid}"` (i.e. `"stale:"` for `mid == ""`), which the existing `startswith("stale:")` checks already drop.

**Block If:** Making the drop echo terminal requires any frontend (webapp `app.js` or extension `ui.js`) change — the plan relies on both already treating per-request `status: "error"` as terminal; if inspection contradicts that, halt.

**Never:**
- Do not edit the deferred-work ledger (`_bmad-output/implementation-artifacts/deferred-work.md`) — the orchestrator records resolution.
- Do not change `copilot_channel_status` drop behavior, `_check_silence_split`, meeting_join/meeting_leave stale-marking, or frontend/extension code.
- Do not store originator websockets in `_copilot_pending_requests` or change its value type beyond the existing string convention.
- Do not add locking around `_copilot_pending_requests` (single asyncio loop; existing mutation sites are lock-free).

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Non-string meeting_id | `copilot_channel_reply` with `meeting_id: 123` (or list/dict) | Frame ignored: log to stderr, no broadcast | Log + return, sibling style |
| meeting_id None/absent | `copilot_channel_reply` with `meeting_id: null` or key absent | Relayed as today (guard falls through) | No error |
| Stale drop echo | tracked entry `"stale:m-prev"`, reply for that request_id arrives | No `copilot_reply`; ONE `copilot_status {request_id, status: "error", message}` broadcast; entry popped | Existing stderr log kept |
| Cross-meeting drop echo (tracked) | tracked entry `"m-prev"` (not stale-marked), reply carries `meeting_id: "m-prev"`, current meeting is `"m-new"` | No `copilot_reply`; ONE terminal `copilot_status` error broadcast; entry popped | Existing stderr log kept |
| Cross-meeting drop, untracked (proactive) | no pending entry, reply carries mismatching `meeting_id` | Dropped as today, NO `copilot_status` emitted | Existing stderr log kept |
| Manual preemption invalidates | `_manual_copilot_active` true, pending `{"req-m": ""}`, conference `meeting_join` runs `_preempt_manual_copilot` | Entry becomes `"stale:"`; a later reply for `req-m` is dropped by the stale branch (and echoes terminal error per DW-8) | No error |
| Preemption no-op | `_manual_copilot_active` false | Early return unchanged; pending entries untouched | No error |
| Non-manual entries survive preemption | pending `{"req-c": "m-cur"}` alongside manual entries during preemption | Only `""`-valued entries are stale-marked; `"m-cur"` and already-`stale:`-prefixed values untouched | No error |

</intent-contract>

## Code Map

- `ktalk_bridge.py:2922-3001` -- `copilot_channel_reply` handler: stale-prefix branch (2951-2958), `meeting_id` read (2959), cross-meeting guard (2968-2982). All three DW-5/DW-8 edits land here.
- `ktalk_bridge.py:1275-1310` -- `_preempt_manual_copilot`: DW-15 stale-marking goes here, after the `_manual_copilot_active` guard.
- `ktalk_bridge.py:1120-1148` -- `_broadcast_copilot_frame`: reuse for the drop echo.
- `ktalk_bridge.py:2029-2032, 2115-2117, 2423-2428, 2544-2546` -- existing stale-marking conventions (`f"stale:{mid}"`) to mirror.
- `tests/test_ktalk_bridge.py` -- `RecordingClient` fixture pattern; relevant existing tests: `test_late_copilot_reply_for_previous_meeting_is_dropped` (1739), `test_proactive_reply_for_different_meeting_is_dropped` (2660), `test_copilot_channel_reply_with_invalid_fields_is_ignored` (1520), preemption tests (4185-4260).
- `frontend/static/app.js:1452-1480`, `kontur-diarize/ktalk-speaker-extension/src/ui.js:1905-1931` -- read-only evidence that per-request `status: "error"` is terminal in both UIs (resolves pending, re-enables input).

## Tasks & Acceptance

**Execution:**
- `ktalk_bridge.py` -- In `copilot_channel_reply`, move the `meeting_id = data.get("meeting_id")` read up to sit directly after the `kind` check (BEFORE the stale-prefix branch, alongside the sibling validations) and validate it: if not `None` and not `str`, log `"[KTalk Bridge] copilot_channel_reply invalid meeting_id type — ignored"` to stderr and return without popping the pending entry (DW-5).
- `ktalk_bridge.py` -- In the stale-prefix drop branch, capture the tracked value BEFORE popping the entry; after the pop, broadcast via `_broadcast_copilot_frame` a frame `{"type": "copilot_status", "request_id": request_id, "status": "error", "message": <see message rule below>}` (DW-8).
- `ktalk_bridge.py` -- In the cross-meeting mismatch drop branch, capture and remove the entry in one operation with `tracked_value = self._copilot_pending_requests.pop(request_id, None)`; if the entry was tracked (`tracked_value is not None`), broadcast the same terminal error frame with the message chosen by the rule below (DW-8).
- `ktalk_bridge.py` -- **Message rule (both drop branches):** the message must state the actual cause, keyed off the tracked value's original scope. Strip the `stale:` prefix if present; if the remaining meeting id is empty (`""` — a manual/no-meeting request, e.g. one invalidated by `_preempt_manual_copilot`), use `"Ответ не доставлен: ручная сессия copilot была прервана."`; if it is non-empty (a conference-scoped request), use `"Ответ не доставлен: встреча, в которой был задан вопрос, уже завершилась."` in the stale-prefix branch and `"Ответ не доставлен: встреча сменилась до получения ответа."` in the cross-meeting branch. A manual-chat user must never be told a meeting ended when no meeting ever existed for their question.
- `ktalk_bridge.py` -- In `_preempt_manual_copilot`, after the `_manual_copilot_active` guard passes, iterate `list(self._copilot_pending_requests.items())` and rewrite entries whose value `== ""` to `"stale:"`; log the count to stderr when non-zero, mirroring the existing stale-marking log lines (DW-15).
- `ktalk_bridge.py` -- Split `_preempt_manual_copilot` state mutation from its subscriber notice without changing the default direct-call behavior: add a keyword-only `notify: bool = True` mode and return whether an actual preemption happened; with `notify=False`, the method must clear the mirror and stale-mark pending manual entries, then return without reaching any await or sending `manual_preempted`. Move the subscriber fan-out into a dedicated async helper that the default `notify=True` path still awaits.
- `ktalk_bridge.py` -- In the real `meeting_join` transition, call `_preempt_manual_copilot(notify=False)` before the first awaited lifecycle send, capture whether preemption happened, then await `_emit_lifecycle(meeting_started_frame)`, and only afterwards send the existing subscriber-only `manual_preempted` notice when preemption happened. This order guarantees stale state before any re-entry while keeping subscriber I/O from delaying or cancelling lifecycle delivery. Preserve the lifecycle frame shape and audiences.
- `ktalk_bridge.py` -- In `copilot_chat_message`, record `request_id -> (meeting_id or "")` after confirming at least one channel client but BEFORE the first awaited `client.send(outbound)`. There must be no await between choosing the request scope and publishing the pending entry. Do not add new all-send-failed cleanup in this story; preserve the pre-existing bookkeeping behavior.
- `tests/test_ktalk_bridge.py` -- Add async tests mirroring existing patterns for each I/O matrix row: non-string `meeting_id` ignored (extend or add beside `test_copilot_channel_reply_with_invalid_fields_is_ignored`); explicit-None AND absent `meeting_id` relay; stale drop emits exactly one terminal `copilot_status` error to extension and subscriber clients; tracked cross-meeting drop emits it; untracked (proactive) cross-meeting drop emits nothing; `_preempt_manual_copilot` stale-marks only `""` entries and leaves others; late manual reply after preemption is dropped with terminal echo (end-to-end of DW-15+DW-8). Message-rule coverage: the meeting-scoped stale-drop test asserts the meeting-ended message; the end-to-end preemption test asserts the manual-session message (`"прервана"`), NOT the meeting-ended one; the cross-meeting test asserts the meeting-changed message.
- `tests/test_ktalk_bridge.py` -- Add two deterministic re-entrant race regressions: (1) a lifecycle recipient processes the late manual reply while `meeting_join` is inside the `meeting_started` send, proving invalidation happened before that await; (2) a channel client's `copilot_user_message` send triggers `meeting_join` before returning, proving the pending entry was already visible, becomes `"stale:"`, is not overwritten when the chat relay resumes, and the later reply produces only the terminal error.
- `tests/test_ktalk_bridge.py` -- Add a blocking-subscriber ordering regression: hold the `manual_preempted` send open and prove channel/extension/supervisor lifecycle recipients already received `meeting_started` before the notice is released. Keep the existing direct `_preempt_manual_copilot()` notice-delivery test green through the default `notify=True` path.

**Acceptance Criteria:**
- Given a tracked pending request whose entry is stale-marked, when its `copilot_channel_reply` arrives, then subscriber AND extension clients each receive exactly one `copilot_status` frame with that `request_id`, `status: "error"`, and a non-empty `message`, and no `copilot_reply` frame.
- Given a proactive reply (untracked request_id) carrying a mismatching `meeting_id`, when it arrives, then no frame of any type is broadcast.
- Given `copilot_channel_reply` with `meeting_id` of a non-string, non-None type, when processed, then nothing is broadcast and the pending entry (if any) is not popped.
- Given an armed manual mirror with a pending `""`-valued request, when a conference `meeting_join` triggers preemption, then a subsequent reply for that request is NOT rendered as `copilot_reply` and the requester receives the terminal error status.
- Given a pending manual request and a conference join whose `meeting_started` delivery re-enters the bridge with that request's late reply, when the re-entrant reply is processed, then the request is already stale and no `copilot_reply` is broadcast.
- Given a manual chat relay whose channel `send` re-enters the bridge with a conference join before returning, when the chat relay resumes and its late reply later arrives, then the request remains stale, no `copilot_reply` is broadcast, and the terminal error is delivered.
- Given a subscriber whose `manual_preempted` send blocks, when a conference join reaches that blocked send, then channel, extension, and supervisor recipients have already received `meeting_started`.
- Given the full existing test suite, when run, then no previously passing test fails.

## Spec Change Log

### 2026-07-25 — Loopback 1: drop-echo message must state the actual cause

- **Triggering finding (bad_spec, medium):** after DW-15 the stale-prefix branch also terminates preempted *manual* chat requests (tracked value `"stale:"`), but the spec pinned a single meeting-ended message ("встреча… уже завершилась") for the whole branch — factually wrong for a question asked with no meeting active. Reported independently by the adversarial and intent-alignment reviewers.
- **Amendment:** replaced the two fixed-message task bullets with a "Message rule" bullet: both drop branches key the message off the tracked value's original scope (strip `stale:`; empty id → manual-session message `"Ответ не доставлен: ручная сессия copilot была прервана."`; non-empty id → the original meeting-ended / meeting-changed messages). Test task extended to assert the per-case messages.
- **Known-bad state avoided:** a manual webapp-chat user being told a meeting ended when no meeting ever existed for their question.
- **KEEP (must survive re-derivation):**
  - `meeting_id` type check placed with the sibling validations, BEFORE the stale-prefix branch; rejected frame leaves the pending entry untouched.
  - Cross-meeting echo gated on the entry being tracked (proactive `proactive_<uuid>` drops stay fully silent).
  - `_preempt_manual_copilot` stale-marks `""` entries only after the `_manual_copilot_active` guard passes, with a non-zero count stderr log.
  - The 8-test structure from attempt 1 (both-audience assertions via `_bridge_with_extension_and_subscriber`, end-to-end preemption test) — extend with per-case message assertions, do not discard; full suite was green at 688 passed before revert.

### 2026-07-25 — Loopback 2: close both preemption await windows

- **Triggering findings (bad_spec, high):** the plan only tested pre-populated pending entries and did not constrain two re-entrant await windows. First, `meeting_join` awaited the `meeting_started` lifecycle send before calling `_preempt_manual_copilot`, so a late manual reply could be processed before stale-marking. Second, `copilot_chat_message` recorded its pending entry only after awaiting channel sends, so a conference join inside that send saw no entry and the resumed relay then wrote a fresh `""` entry after preemption.
- **Amendment:** require manual preemption before the first lifecycle-send await; require pending bookkeeping before the first channel-send await; add deterministic re-entrant regressions for both orderings.
- **Known-bad state avoided:** a reply from the killed manual Copilot session being broadcast into the newly-started conference despite the DW-15 guard.
- **KEEP (must survive re-derivation):**
  - All Loopback 1 KEEP instructions and its per-scope message rule.
  - The cross-meeting branch uses one `pop(request_id, None)` to both capture and remove the tracked value; do not reintroduce a `get` followed by `pop`.
  - The cross-meeting branch uses `scope = tracked_value` directly; do not add a dead `stale:` strip after the earlier stale branch has returned.
  - `_preempt_manual_copilot` keeps the intent-mandated early return when `_manual_copilot_active` is false.
  - Pending bookkeeping moves before the send await without adding new delivery-failure cleanup or changing the stored string convention.

### 2026-07-25 — Loopback 3: keep subscriber I/O off the lifecycle critical path

- **Triggering finding (bad_spec, high):** Loopback 2 moved the whole async `_preempt_manual_copilot` call before `_emit_lifecycle`; its subscriber notice awaits could therefore stall, re-enter, or be cancelled before channel/extension/supervisor consumers received `meeting_started`, delaying the supervisor's manual-to-conference replacement and exposing the reversed notice order.
- **Amendment:** split state invalidation from notice delivery through a no-await `notify=False` path; perform state invalidation first, lifecycle fan-out second, and subscriber-only `manual_preempted` notification last; add a blocking-subscriber ordering regression.
- **Known-bad state avoided:** an unrelated slow/dead webapp subscriber preventing the supervisor from learning that the conference started while the bridge already cleared its manual mirror.
- **KEEP (must survive re-derivation):**
  - All Loopback 1 and Loopback 2 KEEP instructions.
  - Pending request bookkeeping remains published before the first channel-send await, with both re-entrant race regressions retained.
  - The default direct `_preempt_manual_copilot()` path still sends `manual_preempted`, so existing direct-call behavior and dead-subscriber resilience tests remain valid.
  - Explicit-None and absent `meeting_id` coverage plus stderr assertions for invalid/stale/mismatch rows remain in the matrix tests.
  - No subscriber notification occurs before `meeting_started`; only synchronous mirror clearing and stale-marking precede lifecycle delivery.

## Review Triage Log

### 2026-07-25 — Review pass 1
- intent_gap: 0
- bad_spec: 1: (high 0, medium 1, low 0)
- patch: 0
- defer: 5: (high 0, medium 3, low 2)
- reject: 5
- addressed_findings:
  - `[medium]` `[bad_spec]` Stale-drop echo message references a meeting for manual/preempted requests that never had one — spec amended with a per-scope message rule; code reverted for re-derivation.
- defer findings are moot this pass (code re-derivation); noted for continuity: preemption-time termination when the killed session never replies; terminal `copilot_channel_status` for a stale entry drops silently without echo/pop; `stale:` entries never garbage-collected if no reply arrives; `""` entries created while the manual mirror is desynced bypass `_preempt_manual_copilot`; broadcast drop echo renders an unmatched error bubble in non-originating UIs.

### 2026-07-25 — Review pass 2
- intent_gap: 0
- bad_spec: 0
- patch: 2: (high 0, medium 0, low 2)
- defer: 5: (high 0, medium 3, low 2)
- reject: 6
- addressed_findings:
  - `[low]` `[patch]` Cross-meeting drop branch did `get` then `pop` of the same key — collapsed to a single `pop(request_id, None)`.
  - `[low]` `[patch]` Dead `stale:`-prefix strip in the cross-meeting branch (stale entries always return at the earlier branch; no await between the dict reads) — removed, `scope = tracked_value` used directly with an explanatory comment.
- rejected (for the record): AUTOSPAWN=0 "lost genuine answer" is the intent's own mandated trade-off (invalidate on preemption, letter of DW-15; lingering session is a documented v1 limitation); false-cause manual message in the empty-scope cross-meeting corner (near-unreachable, both candidate texts are approximations); malformed `meeting_id` from the trusted local channel daemon stranding a tracked entry (sibling-style behavior the intent prescribed); production/test message-literal duplication (test-side duplication is deliberate pinning); `""`-sentinel "conflation" (by construction `""` means no-conference request — the documented convention); tests seeding private state (file-wide established pattern, recording path pinned elsewhere).

### 2026-07-25 — Review pass 3
- intent_gap: 0
- bad_spec: 2: (high 2, medium 0, low 0)
- patch: 2: (high 0, medium 0, low 2)
- defer: 6: (high 0, medium 4, low 2)
- reject: 4
- addressed_findings:
  - `[high]` `[bad_spec]` `meeting_join` awaited lifecycle delivery before manual invalidation, allowing a re-entrant late reply to bypass the new guard — ordering and a race regression added to the spec.
  - `[high]` `[bad_spec]` `copilot_chat_message` published pending bookkeeping only after awaiting channel sends, allowing a re-entrant conference join to miss and then be overwritten by the request — ordering and a race regression added to the spec.
- lower findings are moot this pass because code is re-derived. Existing ledger entries remain untouched; no deferred-work entry was appended.

### 2026-07-25 — Review pass 4
- intent_gap: 0
- bad_spec: 1: (high 1, medium 0, low 0)
- patch: 1: (high 0, medium 0, low 1)
- defer: 2: (high 0, medium 2, low 0)
- reject: 4
- addressed_findings:
  - `[high]` `[bad_spec]` Moving the full async preemption before lifecycle put subscriber I/O on the supervisor's critical path and reversed the visible notice order — the spec now separates no-await invalidation, lifecycle delivery, and post-lifecycle notification.
- lower findings are moot this pass because code is re-derived. Existing ledger entries remain untouched; no deferred-work entry was appended.

### 2026-07-25 — Review pass 5
- intent_gap: 0
- bad_spec: 0
- patch: 0
- defer: 4: (high 3, medium 1, low 0)
- reject: 1
- addressed_findings:
  - none
- deferred-work handling: three findings already had existing ledger entries and were left byte-for-byte untouched; one new multi-channel duplicate-reply finding was appended as a new entry only.

## Design Notes

- Status value `"error"` (not a new custom status): both UIs handle unknown statuses non-terminally (placeholder text mutation only, pending kept), so a custom status would NOT unblock the asker. `"error"` resolves the placeholder and re-enables input in webapp (`app.js:1452`) and extension (`ui.js:1910`) today.
- The echo is a broadcast, not originator-only: the originator socket is not stored in `_copilot_pending_requests`, and all per-request statuses already travel by broadcast (`copilot_channel_status` → `_broadcast_copilot_frame`). Spurious bubbles in unrelated tabs for tracked drops match today's behavior for channel-server `error` statuses.
- DW-5 rejects (log + return) instead of coercing to None: coercion would relay a frame whose `meeting_id` the guard could not evaluate; rejection matches the handler's sibling checks and keeps the broadcast payload well-typed.

## Verification

**Commands:**
- `source venv-gigaam/bin/activate && python -m pytest tests/test_ktalk_bridge.py -v` -- expected: all pass, including new tests
- `python -m pytest tests/ -v -x --ignore=tests/e2e` -- expected: no regressions
- `python -m compileall ktalk_bridge.py` -- expected: clean

## Auto Run Result

Status: done

### Summary

Hardened Copilot reply routing across malformed meeting ids, stale and cross-meeting drops, manual-session preemption, and re-entrant asyncio delivery windows. Pending manual requests are now visible before channel-send suspension; preemption state is applied before lifecycle re-entry; `meeting_started` reaches lifecycle consumers before the subscriber-only preemption notice.

### Files changed

- `ktalk_bridge.py` — validates reply meeting ids, emits terminal tracked-drop errors, invalidates manual pending requests, and separates no-await preemption state from post-lifecycle notice delivery.
- `tests/test_ktalk_bridge.py` — covers the full edge-case matrix, cause-specific messages, re-entrant send/join races, stderr behavior, and blocking-subscriber ordering.
- `deferred-work.md` — appended one new multi-channel duplicate-reply finding; all existing entries and statuses were left untouched.

### Review findings

- Patches applied in this pass: 0.
- Deferred in this pass: 4; three already existed in the ledger, one new entry appended.
- Rejected in this pass: 1.
- Follow-up review recommendation: false.
- Patched severity counts: high 0, medium 0, low 0; score `3 × 0 + 1 × 0 = 0`.

### Verification

- `source venv-gigaam/bin/activate && python -m pytest tests/test_ktalk_bridge.py -v` — 159 passed.
- `source venv-gigaam/bin/activate && python -m pytest tests/ -v -x --ignore=tests/e2e` — 691 passed.
- `source venv-gigaam/bin/activate && python -m compileall ktalk_bridge.py` — passed.
- `git diff --check` — clean.
- Matrix audit — every I/O and edge-case row has a covering test in the passing bridge suite, including explicit-None and absent `meeting_id`.
- Manual inspection — both existing UI consumers treat matching per-request `status: "error"` as terminal.

### Residual risks

- Multiple simultaneous Copilot channel clients can consume one shared pending tombstone and allow a later duplicate reply to become untracked; recorded as the single new deferred-work entry.
- Existing ledger items remain for stale terminal statuses, disarmed-mirror manual requests, subscriber fan-out stalls, and unmatched broadcast error bubbles.
- The live launchd services were not restarted or manually exercised during this review; deterministic bridge and websocket tests cover the changed routing.
