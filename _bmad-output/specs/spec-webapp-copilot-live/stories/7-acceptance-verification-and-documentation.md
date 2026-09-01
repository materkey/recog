---
title: 'Acceptance verification and documentation'
type: 'chore'
created: '2026-07-25'
status: 'done'
baseline_revision: 'BASE_REVISION'
final_revision: 'b69c5f8'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/docs/plans/20260724-webapp-copilot-live.md'
  - '{project-root}/_bmad-output/specs/spec-webapp-copilot-live/SPEC.md'
warnings: [oversized]
---

<intent-contract>

## Intent

**Problem:** Stories 1–6 shipped the whole webapp-copilot pipeline, but the epic's closing pass is missing: no consolidated automated acceptance run against plan Task 7's edge-case checklist, and the docs still describe the pre-epic world — `CLAUDE.md`'s copilot section knows nothing of manual spawn/webapp chat, `openspec/specs/ktalk-copilot-chat/spec.md` lacks the new frames and subscriber fan-out, and the plan file still sits in `docs/plans/` with every checkbox unchecked.

**Approach:** Plan Tasks 7+8. Run the full automatable acceptance pass (pytest, compileall, `node --check`), verify each Task-7 edge case by naming and running the concrete test that pins it (or by code inspection where the case is architectural), then bring the three documentation surfaces up to date and move the completed plan to `docs/plans/completed/`. The manual smoke (launchd restart + real meeting) is explicitly NOT attempted — it remains a post-completion human step and must be stated as such in the run result.

## Boundaries & Constraints

**Always:**
- Verification first, docs second: the docs must describe a state proven green, not an intended one.
- Every Task-7 edge case gets an explicit disposition: a named test (run and passing) or a short code-inspection argument; no case is waved through silently.
- Plan checkbox bookkeeping: mark `[x]` every automatable item of Tasks 1–8; the Task-7 smoke item is annotated (⚠️ per the plan's own convention) as post-completion manual, not falsely checked.
- `CLAUDE.md` edits stay within the copilot-related sections: extend the "Auto-spawn Copilot supervisor" section with manual spawn/stop frames, webapp chat, and D3/D4 semantics, and correct only statements this epic made false (`RECOG_COPILOT_AUTOSPAWN` env description, `bridge_subscriber.py` / `ktalk_bridge.py` module lines).
- `openspec` spec keeps its existing format (Requirement / Scenario, WHEN/THEN bullets); additions cover the new frames (`copilot_spawn_request`, `copilot_stop_request`, `manual` lifecycle id, `conference_active` / `manual_preempted` / `supervisor_disconnected` / `bridge_disconnected` statuses, `supervisor_hello.current_meeting_id`) and the subscriber fan-out of `copilot_reply`/`copilot_status`; existing requirements stay untouched.
- Plan file moves via `git mv` so history follows.

**Block If:**
- The full pytest suite or a named edge-case test fails — that is a regression in an earlier story, not a docs matter; do not patch pipeline code from this story.

**Never:**
- Do NOT attempt the manual smoke (launchd restart, real meeting, browser interaction) — invocation directive; it is the human's post-completion step.
- No changes to `ktalk_bridge.py`, `recog/`, `frontend/`, or extension JS — this story touches only docs, the plan file, and the openspec spec.
- Do not rewrite unrelated `CLAUDE.md` sections or reformat the openspec file beyond the additions.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| No supervisor | spawn/stop with `_supervisor_clients` empty | `supervisor_disconnected` echo — `test_spawn_request_without_supervisor_replies_disconnected`, `test_stop_request_without_supervisor_still_clears_flag` | Test must pass |
| No channel client | chat with zero channel clients | `channel_disconnected` to originator — existing bridge tests | Test must pass |
| Bridge down | webapp frame while subscriber disconnected | `bridge_disconnected` echo — `tests/test_frontend_copilot_proxy.py`; `test_send_while_bridge_unavailable_returns_false` | Test must pass |
| Conference preemption | conference starts during manual session | flag cleared + `manual_preempted`; Kill+Spawn(B) — `test_conference_meeting_started_preempts_manual_session` (bridge), `test_conference_meeting_preempts_manual_session` (supervisor) | Test must pass |
| Spawn vs conference | spawn during active conference | rejected `conference_active`, never forwarded — `test_spawn_request_during_conference_is_rejected`, `test_spawn_request_is_not_armed_when_conference_starts_mid_relay` | Test must pass |
| Reconnect adopt | supervisor reconnects mid-manual | adopt, no kill — `test_reconcile_adopts_live_manual_session` | Test must pass |
| Service restart | bridge restart mid-manual-session | re-arm via `supervisor_hello.current_meeting_id` — `test_supervisor_hello_rearms_manual_flag_before_reply`, `test_supervisor_hello_reports_live_manual_session` | Test must pass |
| F5 of tab | browser reload during manual session | session survives — code inspection: D5, browser holds no session state, no stop frame on WS close | Inspection argument recorded |
| Orphan rotation | manual reply after silence-split rotation | reply not dropped — `test_manual_chat_reply_survives_meeting_rotation` | Test must pass |

</intent-contract>

## Code Map

- `docs/plans/20260724-webapp-copilot-live.md` -- the plan: Tasks 1–8 checkboxes to mark; file to `git mv` into `completed/`
- `CLAUDE.md:128-139` -- "Auto-spawn Copilot supervisor" section to extend (manual frames, webapp chat, D3/D4); `:150-155` module lines for `copilot_supervisor.py` / `ktalk_bridge.py` / `bridge_subscriber.py`; `:190` `RECOG_COPILOT_AUTOSPAWN` env description
- `openspec/specs/ktalk-copilot-chat/spec.md` -- add requirements: manual session lifecycle (spawn/stop frames, `manual` id, conference precedence, hello re-arm) and webapp subscriber fan-out + `/api/live/stream` proxy
- `tests/test_ktalk_bridge.py`, `tests/test_copilot_supervisor.py`, `tests/test_bridge_subscriber.py`, `tests/test_frontend_copilot_proxy.py` -- read-only: the named edge-case tests live here
- `frontend/static/app.js` -- read-only: `node --check` target; F5-inspection surface (`copilotState`, `copilotHandleWsClosed`)

## Tasks & Acceptance

**Execution:**
- run acceptance -- `venv-gigaam/bin/python -m pytest tests/ -v`, `venv-gigaam/bin/python -m compileall recog frontend`, `node --check frontend/static/app.js` -- the epic-wide green gate before any docs change
- verify edge cases -- run each named test from the I/O Matrix individually (single `pytest -k` sweep is fine) and write the F5 inspection argument -- explicit per-case disposition for the Auto Run Result
- `docs/plans/20260724-webapp-copilot-live.md` -- mark `[x]` all completed checkboxes in Tasks 1–8; annotate the Task-7 smoke item as ⚠️ post-completion manual; then `git mv` to `docs/plans/completed/` -- plan bookkeeping stories 1–6 explicitly deferred here
- `CLAUDE.md` -- extend the copilot supervisor section: manual spawn/stop via `copilot_spawn_request`/`copilot_stop_request` under synthetic id `"manual"`, webapp `#/live` chat panel + chip, D3 (`AUTOSPAWN=0` gates only meeting frames; manual + crash-respawn work regardless), D4 (no proactive ticks in manual sessions), D5 re-arm, known v1 preemption desync; correct the env-var line and the `bridge_subscriber.py`/`ktalk_bridge.py` module lines
- `openspec/specs/ktalk-copilot-chat/spec.md` -- append the new requirements/scenarios per the Boundaries list -- keeps the team spec in sync (plan Task 8)

**Acceptance Criteria:**
- Given the completed story, when `git diff --name-only <baseline>` runs, then it shows only `CLAUDE.md`, `openspec/specs/ktalk-copilot-chat/spec.md`, and the plan file rename (plus this story spec's artifacts) — no code files.
- Given the moved plan, when `docs/plans/` is listed, then `20260724-webapp-copilot-live.md` exists only under `completed/` and its Task 1–8 automatable items are `[x]` with the smoke item ⚠️-annotated.
- Given the Auto Run Result, when a human reads it, then the remaining manual steps (launchd restart smoke, real-meeting preemption check, restart-resilience check) are stated explicitly as not attempted.

## Spec Change Log

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 9: (high 0, medium 4, low 5)
- defer: 1: (high 0, medium 1, low 0)
- reject: 6: (high 0, medium 2, low 4)
- addressed_findings:
  - `[medium]` `[patch]` The new openspec "Manual spawn request" scenario reversed the load-bearing order — it said the bridge arms the manual flag and forwards, while the code deliberately forwards first and arms only after successful delivery plus a post-relay conference re-check (the order closes a documented mid-relay race). THEN rewritten to state forward-first.
  - `[medium]` `[patch]` The fan-out requirement (openspec) and the CLAUDE.md manual-sessions bullet overclaimed that "every `copilot_status` variant" reaches subscribers — per-request rejection echoes (`conference_active`, `supervisor_disconnected`, the no-channel chat `channel_disconnected`) are socket-targeted by design, `manual_preempted` is subscriber-only, and `bridge_disconnected` never leaves the frontend server. Both texts scoped to broadcast frames with the exceptions named.
  - `[medium]` `[patch]` "`current_meeting_id` (live session id or null)" misdescribed `_live_meeting_id()` — a GRACE session is physically alive yet reports null, deliberately, so a reconnect inside the grace window cannot defeat Stop. Both docs now define it as the id of a session the supervisor intends to keep alive (SPAWNING/RUNNING); GRACE/IDLE report null.
  - `[medium]` `[patch]` The "Manual stop request" scenario asserted an unconditional kill after grace, but `meeting_ended("manual")` no-ops on a mismatched lifecycle id. Precondition "while a manual session is live" added plus a new scenario pinning the safety invariant: a Stop during a conference session is relayed and clears the bridge flag but is inert at the supervisor.
  - `[low]` `[patch]` "never forwarded to the supervisor" overstated the `conference_active` guarantee — a conference starting mid-relay can see the frame already delivered (flag still not armed). Qualified in CLAUDE.md and the openspec rejection scenario.
  - `[low]` `[patch]` The same-id-flap description omitted that an explicit spawn click also resets the respawn backoff budget (reviving a session parked after 3 exhausted respawns) — the exact nuance story 3's review had to patch in. One clause added to both docs.
  - `[low]` `[patch]` CLAUDE.md D4 bullet said `_copilot_tick_state` is "seeded only by conference `meeting_started`", but the same-meeting reconnect branch re-seeds it without emitting `meeting_started`. Reworded to "conference join paths … never by manual frames".
  - `[low]` `[patch]` The "Whitelisted inbound frame is relayed" scenario carried an AND clause about inputs its WHEN excludes; the rejection got its own "Non-whitelisted inbound frame is dropped" scenario.
  - `[low]` `[patch]` The appended CLAUDE.md "See …" cross-reference created a double-"and" run-on; split into two sentences.

Deferred (pre-existing, surfaced by the verification-gap lens): the F5/tab-close edge case has no pinning regression test — it holds by absence of code; test authoring was outside this story's docs-only intent (ledger entry appended). Rejected on the authority of the intent itself: filing the plan under `completed/` with the smoke ⚠️-annotated is the invocation's explicit directive ("manual smoke is post-completion for the human — do not attempt it"); the agent-state primer staleness and its missing invalidation were litigated and rejected in story 5's triage (self-healing by design); a missing-`request_id` scenario and the hello-during-conference inverse scenario are coverage preferences for a spec whose addition list the intent enumerates (malformed frames are already covered by the existing Feature-isolation requirement); the `supervisor_disconnected`-with-a-connected-supervisor label nuance requires a double race (conference starts mid-relay then ends before the re-check) and is retryable; the un-pinned "with a log" clause would need a test change this story's intent forecloses.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 9: (high 1, medium 6, low 2)
- defer: 2: (high 0, medium 2, low 0)
- reject: 7: (high 1, medium 4, low 2)
- addressed_findings:
  - `[high]` `[patch]` The docs claimed that a manual session survives full `./recog-service.sh restart`, but that command restarts the supervisor: its fresh IDLE lifecycle sends `current_meeting_id: null`, so the surviving tmux session enters GRACE. CLAUDE.md and the completed plan now limit D5 to bridge-only restart and identify full-service recovery as deferred.
  - `[medium]` `[patch]` CLAUDE.md's canonical `meeting_state.active` definition still excluded the new manual branch. It now defines active as either a live conference or the armed manual mirror.
  - `[medium]` `[patch]` CLAUDE.md's status-merge description still limited sessions-zero eligibility and agent-state delivery to active conferences/extensions. It now covers manual liveness and subscriber/webapp delivery.
  - `[medium]` `[patch]` The openspec fan-out scenario contradicted its own subscriber-only `manual_preempted` exception. The shared-audience guarantee and exception are now explicit.
  - `[medium]` `[patch]` The openspec conference-precedence requirement was unconditional despite the documented `AUTOSPAWN=0` desync. It now distinguishes enabled preemption from the disabled-mode bridge-only notice and lingering manual tmux limitation.
  - `[medium]` `[patch]` The completed plan said Task-7 smoke verified the UI while also stating that smoke was not attempted. It now records `node --check` as the automated UI gate and keeps browser behavior as a post-completion manual smoke.
  - `[medium]` `[patch]` The reviewed diff lacked the required consolidated evidence record and omitted `node --check` from Task 7. The plan and this Auto Run Result now record the command and every edge-case disposition.
  - `[low]` `[patch]` The openspec primer scenario did not define a renderable `agent_state` value. It now lists the five supported states.
  - `[low]` `[patch]` CLAUDE.md's spawn/stop notation visually attached `{request_id}` only to Stop. Both frame forms now state the required field explicitly.

Deferred as new ledger entries only: a pending manual reply can cross conference preemption because empty meeting ids are not marked stale, and the originator-only no-channel `channel_disconnected` contract lacks a negative-recipient assertion. The already-recorded full-restart and F5 findings were left untouched. Rejected on the authority of the intent: executing the manual smoke or adding browser automation; expanding openspec with a full UI specification; changing primer invalidation; editing code comments solely to repair moved-plan backlinks; imposing a new bridge-client authorization rule; pinning the non-whitelisted-frame log; and replacing the explicitly permitted F5 code-inspection argument with a new test.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 2: (high 0, medium 2, low 0)
- defer: 1: (high 0, medium 1, low 0)
- reject: 15: (high 2, medium 8, low 5)
- addressed_findings:
  - `[medium]` `[patch]` The completed plan retained pre-review task wording that contradicted the verified protocol: it still said every status was broadcast, every Copilot status used the shared fan-out, a mid-relay conference race could never deliver spawn, and GRACE counted as a live `current_meeting_id`. Task 1–3 wording now matches the originator/shared audiences, the post-relay conference re-check, and the SPawning/RUNNING keep-alive contract.
  - `[medium]` `[patch]` The story required a durable, per-edge-case acceptance record but had no `Auto Run Result` section. This pass adds the exact command outcomes, named test/inspection disposition for every matrix row, manual steps not attempted, review score, and residual risks.

Deferred as one NEW ledger entry only: the documented D4 guarantee that manual sessions do not seed proactive transcript ticks lacks a lifecycle regression test. Existing full-restart, F5, pending-manual-reply, and negative-recipient entries were left byte-for-byte untouched. Rejected findings either contradicted the explicit docs-only/manual-smoke intent, duplicated existing ledger work, challenged intentional shared-session routing, or were disproved by the current delivery guards, UI conference Stop disablement, schema tests, and race coverage.

## Verification

**Commands:**
- `venv-gigaam/bin/python -m pytest tests/ -v` -- expected: full suite green (≥680 passed, 2 deselected)
- `venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py tests/test_copilot_supervisor.py -k "manual or spawn or stop or preempt or hello or rotation or conference" -v` -- expected: every named edge-case test passes
- `venv-gigaam/bin/python -m compileall recog frontend` -- expected: no errors
- `node --check frontend/static/app.js` -- expected: exit 0
- `git status --porcelain` after commit -- expected: clean; plan file shows as renamed (R) into `completed/`

## Auto Run Result

### Summary

Completed a fresh follow-up review of the story-7 documentation diff. The completed plan now uses the same audience, mid-relay, and lifecycle-id semantics as `CLAUDE.md` and the OpenSpec contract. No production code or tests changed.

### Files

- `CLAUDE.md` — reviewed the canonical manual-session, status-merge, and restart-recovery documentation from the baseline diff; no additional edit was required in this pass.
- `docs/plans/completed/20260724-webapp-copilot-live.md` — corrected stale Task 1–3 wording for targeted statuses, the conference mid-relay re-check, and `current_meeting_id`.
- `openspec/specs/ktalk-copilot-chat/spec.md` — reviewed the manual lifecycle and subscriber fan-out requirements from the baseline diff; no additional edit was required in this pass.
- `_bmad-output/implementation-artifacts/deferred-work.md` — appended one new entry for the missing manual-lifecycle/no-proactive-tick regression test; all existing entries were preserved.
- This story spec — recorded the review triage, acceptance evidence, follow-up score, and residual risks.

### Review findings

- Patches applied: 2 medium.
- Deferred: 1 medium, appended as one new ledger entry.
- Rejected: 15 (2 high, 8 medium, 5 low), including duplicates of existing ledger entries and findings contrary to the explicit intent.
- Follow-up review recommendation: `true` — patched high 0, medium 2, low 0; score `3 × 2 + 1 × 0 = 6`.

### Verification outcomes

- `venv-gigaam/bin/python -m pytest tests/ -v` — PASS: 680 passed, 2 deselected in 33.03 s.
- `venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py tests/test_copilot_supervisor.py -k "manual or spawn or stop or preempt or hello or rotation or conference" -v` — PASS: 87 passed, 156 deselected in 3.24 s.
- `venv-gigaam/bin/python -m compileall recog frontend` — PASS.
- `node --check frontend/static/app.js` — PASS.
- `git diff --check BASE_REVISION --` — PASS.
- Diff scope — PASS: only `CLAUDE.md`, the plan rename, and `openspec/specs/ktalk-copilot-chat/spec.md`; no production code files.
- Plan placement/checklist — PASS: the plan exists only under `docs/plans/completed/`, has no unchecked task boxes, and keeps the smoke marked as a post-completion manual step.

### Edge-case dispositions

- No supervisor — PASS: `test_spawn_request_without_supervisor_replies_disconnected` and `test_stop_request_without_supervisor_still_clears_flag`.
- No channel client — PASS in the full suite: `test_copilot_chat_message_without_channel_returns_disconnected_status`.
- Bridge down — PASS in the full suite: `test_send_failure_replies_bridge_disconnected` and `test_send_while_bridge_unavailable_returns_false`.
- Conference preemption — PASS: `test_conference_meeting_started_preempts_manual_session` and `test_conference_meeting_preempts_manual_session`.
- Spawn versus conference — PASS: `test_spawn_request_during_conference_is_rejected` and `test_spawn_request_is_not_armed_when_conference_starts_mid_relay`.
- Reconnect adopt — PASS: `test_reconcile_adopts_live_manual_session`.
- Bridge-only restart recovery — PASS: `test_supervisor_hello_rearms_manual_flag_before_reply` and `test_supervisor_hello_reports_live_manual_session`.
- F5/tab close — PASS by code inspection as permitted by the intent: `copilot_stop_request` is emitted only by `copilotStopClick`; there is no `beforeunload`/`pagehide` sender, and the server disconnect path only unregisters the browser socket.
- Orphan rotation — PASS: `test_manual_chat_reply_survives_meeting_rotation`.

### Manual steps not attempted and residual risks

- NOT attempted by explicit intent: launchd/service restart smoke, real-browser interaction, and real-meeting preemption.
- Full `./recog-service.sh restart` recovery remains a known deferred gap because it restarts the supervisor as well as the bridge.
- Browser behavior remains covered by syntax, backend protocol tests, and the permitted F5 inspection rather than a browser automation suite.
- The new deferred item is the absence of a regression test proving manual lifecycle frames never seed proactive transcript ticks.
