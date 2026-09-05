---
title: 'Supervisor manual lifecycle; autospawn gate narrowed'
type: 'feature'
created: '2026-07-24'
status: 'done'
baseline_revision: 'BASE_REVISION'
final_revision: 'eec08a0'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/docs/plans/20260724-webapp-copilot-live.md'
  - '{project-root}/_bmad-output/specs/spec-webapp-copilot-live/SPEC.md'
warnings: [oversized]
---

<intent-contract>

## Intent

**Problem:** The bridge already relays `copilot_spawn_request` / `copilot_stop_request` to `_supervisor_clients` (story 2), but the supervisor drops them: `handle_frame` knows only the three meeting frame types, and its blanket `if not self.autospawn: return` (`recog/copilot_supervisor.py:484`) would kill manual control under `RECOG_COPILOT_AUTOSPAWN=0` anyway. The same gate in `on_session_died` (`:605`) blocks crash-respawn of a manual session, and `supervisor_hello` carries no `current_meeting_id`, so the bridge's D5 re-arm branch (`ktalk_bridge.py:3247`) can never fire and a bridge restart mid-manual-session GRACE-kills the copilot.

**Approach:** Handle both manual frame types in `handle_frame` under the constant synthetic lifecycle id `"manual"`, reusing the existing `SessionLifecycle` transitions (same-id flap = idempotent double-click, different id = conference preemption). Narrow the autospawn gate to the three meeting-frame branches (D3), drop it from `on_session_died`, and add `current_meeting_id` to `supervisor_hello`.

## Boundaries & Constraints

**Always:**
- TDD: failing tests in `tests/test_copilot_supervisor.py` first; given–when–then docstrings as used throughout that file.
- Manual spawn, manual stop, and crash-respawn work with `autospawn=False`; conference meeting frames stay gated by it.
- Every lifecycle mutation goes through `_apply` (respawn-budget reset + `_sync_grace_timer`), never straight to `_execute` — a manual session must get the same grace timer and backoff budget as a conference one.
- The manual id is a module-level constant in `recog/copilot_supervisor.py` whose **value** matches `ktalk_bridge.MANUAL_MEETING_ID` (`"manual"`).
- Conference behavior with autospawn on stays byte-identical.

**Block If:**
- The manual lifecycle cannot be expressed with the existing `SessionLifecycle` inputs and would require a new state or a changed transition (conference flows would regress).

**Never:**
- No `recog/ktalk_bridge.py`, `recog/web/`, or JS changes — stories 4–6 own those surfaces.
- Do NOT import `ktalk_bridge` from `recog/copilot_supervisor.py` (top-level module, heavy deps, layering inversion) — duplicate the constant with a comment naming its twin.
- Do NOT modify `SessionLifecycle`: the machine already models flap, preemption, and adopt.
- No persistence of manual state across a supervisor process restart (D5: recovery is `supervisor_hello`-driven).
- No `request_id` interpretation, no reply frames to the bridge — the supervisor answers only through its existing `copilot_agent_status` reports.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Manual spawn | IDLE, any `autospawn`; `copilot_spawn_request {request_id}` | `lifecycle.meeting_started("manual")` → `runner.spawn("manual")`; state SPAWNING, `meeting_id == "manual"` | No error expected |
| Double click | RUNNING("manual"); a second `copilot_spawn_request` | No commands, no second spawn (idempotent same-id flap) | No error expected |
| Restart after stop | GRACE("manual"); `copilot_spawn_request` | GRACE→RUNNING, session revived without kill/spawn | No error expected |
| Manual stop | RUNNING("manual"); `copilot_stop_request` | `meeting_ended("manual")` → GRACE, grace timer armed; no kill yet | No error expected |
| Stop completes | GRACE("manual"); grace timer expires | `runner.kill()`, state IDLE | No error expected |
| Conference preemption | RUNNING("manual"), `autospawn=True`; `meeting_started {meeting_id: "B"}` | `Kill()` + `Spawn("B")`; machine tracks "B" | No error expected |
| Meeting frames, gate off | `autospawn=False`; `meeting_started` / `meeting_state` / `meeting_ended` | Inert: no commands, machine stays IDLE | No error expected |
| Manual frames, gate off | `autospawn=False`; spawn then stop then grace expiry | Spawn, then GRACE, then kill — the flag never blocks them | No error expected |
| Reconnect adopt | Machine IDLE, tmux session alive; `meeting_state {active: true, meeting_id: "manual"}` | `adopt("manual")` → RUNNING, no spawn, no kill | No error expected |
| Manual crash respawn | `autospawn=False`, RUNNING("manual"); `on_session_died()` | Respawns "manual" after the backoff delay (same ≤3 budget) | Budget exhaustion logs and stops, as for conference |
| Hello with live session | Connect while SPAWNING/RUNNING("manual") | `supervisor_hello {type, current_meeting_id: "manual"}` | Send failure handled by the existing reconnect loop |
| Hello while idle | Connect while IDLE | `supervisor_hello {type, current_meeting_id: null}` | No error expected |
| Hello during grace | GRACE("manual") after Stop; supervisor reconnects | `current_meeting_id: null` — a stopped session must not be re-armed and revived by the bridge reply | No error expected |
| Malformed manual frame | `copilot_spawn_request` that is not a dict / unknown type | Ignored exactly as today (`handle_frame` guards) | No error expected |

</intent-contract>

## Code Map

- `recog/copilot_supervisor.py:38-43` -- module constants; `MANUAL_MEETING_ID` is declared here
- `recog/copilot_supervisor.py:481-499` -- `handle_frame`: the blanket autospawn gate to narrow + the two new branches
- `recog/copilot_supervisor.py:511-542` -- `_reconcile`: already handles `meeting_id == "manual"` generically (adopt / spawn / flap) — no change expected
- `recog/copilot_supervisor.py:544-575` -- `_apply` / `_execute`: respawn reset, grace sync, spawn-in-flight flag
- `recog/copilot_supervisor.py:598-626` -- `on_session_died`: the `autospawn` early-return to remove
- `recog/copilot_supervisor.py:705-719` -- `run()`: the `supervisor_hello` send site
- `recog/copilot_supervisor.py:73-146` -- `SessionLifecycle`: unchanged; `_same_meeting` gives flap/preemption for free
- `ktalk_bridge.py:75` -- `MANUAL_MEETING_ID = "manual"` (the value this story must match)
- `ktalk_bridge.py:3119-3205` -- bridge relay: frames arrive verbatim, `request_id` already validated bridge-side
- `ktalk_bridge.py:3240-3252` -- consumer of `supervisor_hello.current_meeting_id` (re-arm before `_compute_meeting_state`)
- `tests/test_copilot_supervisor.py:27` `FakeRunner`, `:41` `FakeTmux`, `:219` `test_autospawn_off_is_noop`, `:353` `_start_mock_bridge`, `:359` `test_supervisor_sends_hello_on_connect`, `:659` `test_session_died_with_autospawn_off_is_noop`

## Tasks & Acceptance

**Execution:**
- `tests/test_copilot_supervisor.py` -- add a manual-lifecycle test group covering every I/O Matrix row (reuse `FakeRunner`, `FakeTmux`, `_start_mock_bridge`) -- failing tests first per TDD
- `tests/test_copilot_supervisor.py` -- split `test_autospawn_off_is_noop` (`:219`) into "meeting frames are inert" and "manual frames still work" -- it currently asserts a blanket no-op that D3 removes
- `tests/test_copilot_supervisor.py` -- update `test_supervisor_sends_hello_on_connect` (`:359`) for the added `current_meeting_id` field (exact-dict assertion), and add a reconnect case asserting `"manual"` is reported for a live manual session
- `tests/test_copilot_supervisor.py` -- re-point `test_session_died_with_autospawn_off_is_noop` (`:659`) at the mechanism that actually holds after the gate is gone (IDLE machine ⇒ `session_died()` yields no `Spawn`) -- otherwise its name claims a gate that no longer exists and it passes vacuously
- `recog/copilot_supervisor.py` -- declare `MANUAL_MEETING_ID = "manual"` beside the other module constants, with a comment naming `ktalk_bridge.MANUAL_MEETING_ID` as the twin that must not drift
- `recog/copilot_supervisor.py` -- `handle_frame`: add `copilot_spawn_request` → `lifecycle.meeting_started(MANUAL_MEETING_ID)` and `copilot_stop_request` → `lifecycle.meeting_ended(MANUAL_MEETING_ID)` branches (both through `_apply`), and move the `if not self.autospawn: return` gate down into the `meeting_state` / `meeting_started` / `meeting_ended` branches (D3)
- `recog/copilot_supervisor.py` -- remove the `autospawn` early-return from `on_session_died` so a crashed manual session respawns (D3)
- `recog/copilot_supervisor.py` -- send `current_meeting_id` in `supervisor_hello` via a small helper returning the lifecycle id only while SPAWNING/RUNNING (D5); update the module/`run()` docstring where it describes the hello frame

**Acceptance Criteria:**
- Given the pre-existing suite, when it runs after the change, then every conference-path assertion still holds; only the tests whose subject this change redefines may be edited (`test_autospawn_off_is_noop`, `test_supervisor_sends_hello_on_connect`, and the name/docstring of `test_session_died_with_autospawn_off_is_noop`) and no assertion is relaxed.
- Given `autospawn=False`, when a manual session is spawned, stopped, and the grace timer expires, then the runner recorded exactly one `spawn("manual")` and one `kill()`.
- Given the completed change, when `SessionLifecycle` is diffed against the baseline revision, then its body is unchanged — manual lifecycle reuses the existing transitions.

## Spec Change Log

## Review Triage Log

### 2026-07-24 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 4: (high 0, medium 2, low 2)
- defer: 3: (high 0, medium 1, low 2)
- reject: 12: (high 0, medium 4, low 8)
- addressed_findings:
  - `[medium]` `[patch]` The manual Start button became permanently inert once the ≤3 respawn budget was spent: the machine parks in `SPAWNING("manual")`, a fresh `copilot_spawn_request` is a same-id flap emitting no `Spawn`, so `_apply` never reset the budget and `on_session_died` kept returning early — no error, no session, recovery only via Stop plus the full 90 s grace. The spawn branch now resets the respawn budget (an explicit click is fresh intent, not a flap), so the watchdog's next `session_died` revives the session. Mutation-verified: dropping the reset fails only the new test.
  - `[medium]` `[patch]` Nothing pinned that a webapp Stop during a live conference is inert. The bridge deliberately relays `copilot_stop_request` without a conference guard and delegates the whole guard to the supervisor's id mismatch (`ktalk_bridge.py:3178-3181`), with its own test asserting the frame is relayed; mutating the stop branch to `meeting_ended(self.lifecycle.meeting_id)` left both suites green while turning any Stop click into a conference-session kill 90 s later. Added `test_manual_stop_during_conference_is_inert`, mutation-verified in isolation.
  - `[low]` `[patch]` `main()` still printed "RECOG_COPILOT_AUTOSPAWN is off — connected but not spawning sessions", which D3 makes false: manual spawn/stop and crash-respawn all work with the flag off. Reworded to name meeting-driven auto-spawn as the only thing disabled.
  - `[low]` `[patch]` The `_live_meeting_id` state table omitted the adopted-unknown session (`RUNNING`, `meeting_id=None`) — the one state where a wrong answer would arm the bridge's manual mirror for a session of unknown provenance. Row added.

Rejected on the authority of the intent itself. The largest cluster (four findings across three reviewers) attacks the `autospawn` gate remaining on `meeting_state` / `meeting_started`: with the flag off a manual session is not adopted after a supervisor restart, and a conference does not preempt it. SPEC Non-goals name this exact behavior — "Fixing the `AUTOSPAWN=0` + conference-preemption desync (documented v1 limitation)" — and the story description directs the gate into the meeting-frame branches (D3); CAP-3's "adopts (no kill)" holds either way for a supervisor whose process kept running, since an inert frame cannot kill either. Findings demanding a supervisor-side conference guard duplicate the defense CAP-4 and D5 explicitly place in the bridge (story 2, already reviewed); a spawn-failure reply bound to `request_id` contradicts the intent's "the supervisor answers only through `copilot_agent_status`" and plan Task 6, which drives the UI off the `agent_state` chip. The claim that `_live_meeting_id` should probe tmux rather than report the machine's intent inverts D5's single-owner model. `CLAUDE.md` / `README.md` drift is plan Task 8 (story 7) by `stories.yaml`. Remaining rejects were narrow and self-healing (manual spawn claiming an adopted-unknown orphan; manual Start preempting a conference session already in GRACE — the state machine's documented different-id behavior), test-shape preferences (an unreachable non-dict frame still exercises a real guard; wall-clock margins follow this file's pre-existing convention for the grace and backoff timers), or already litigated in story 2's triage (a Stop the user was told had failed surviving a reconnect).

### 2026-07-24 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 1: (high 0, medium 1, low 0)
- defer: 1: (high 1, medium 0, low 0)
- reject: 11: (high 4, medium 2, low 5)
- addressed_findings:
  - `[medium]` `[patch]` A repeated manual Start during an already-pending crash respawn cancelled the scheduled retry through `_reset_respawn_backoff()` and postponed recovery until a later watchdog tick plus a fresh backoff. The spawn branch now preserves an active respawn task while retaining the exhausted-budget reset for an explicit retry; a regression test failed before the guard and passes after it.

## Design Notes

**Why GRACE reports `current_meeting_id: null`.** The bridge re-arms `_manual_copilot_active` from this field and then answers `meeting_state {active: true, meeting_id: "manual"}`, which `_reconcile` turns into `meeting_started("manual")` — a GRACE→RUNNING revival. In GRACE the user has already pressed Stop, so reporting the id there would let any supervisor reconnect inside the 90 s grace defeat Stop, contradicting CAP-2 ("stop_request drives `meeting_ended` → GRACE → timer → Kill") and the SPEC success signal.

```python
def _live_meeting_id(self) -> Optional[str]:
    """`supervisor_hello.current_meeting_id` (D5): the id of a session the
    machine intends to keep alive. GRACE/IDLE report None — a session already
    scheduled to die must not be re-armed by the bridge's reconcile reply."""
    if self.lifecycle.state in (State.SPAWNING, State.RUNNING):
        return self.lifecycle.meeting_id
    return None
```

**`meeting_state` stays gated under `autospawn=False`.** Plan Task 3 states it explicitly ("`meeting_started`/`meeting_state` remain inert"), and it is the reading the invocation intent names. SPEC CAP-3's summary sentence ("all regardless of `RECOG_COPILOT_AUTOSPAWN`") is not contradicted by its own testable criterion: an inert `meeting_state` frame cannot kill either, so "adopts (no kill)" holds either way for a supervisor whose process kept running. The residual case — supervisor **process** restart with `AUTOSPAWN=0` leaving the manual tmux session unadopted — is the same class as the plan's documented `AUTOSPAWN=0` desync limitation and is out of scope here.

**No new supervisor-side state.** `"manual"` is just another meeting id: `meeting_started("manual")` twice is the existing same-id no-op (double-click safety), a conference id afterwards is the existing kill+spawn (preemption), and `_reconcile` needs no manual branch at all.

## Verification

**Commands:**
- `venv-gigaam/bin/python -m pytest tests/test_copilot_supervisor.py -v` -- expected: all pass, including the new manual-lifecycle group
- `venv-gigaam/bin/python -m pytest tests/ -v` -- expected: full suite green (bridge tests from stories 1–2 unaffected)
- `venv-gigaam/bin/python -m compileall recog frontend` -- expected: no syntax errors
- `git diff -- recog/copilot_supervisor.py | grep -c "class SessionLifecycle"` -- expected: `0` (the state machine is untouched)

## Auto Run Result

Status: done

Summary: Follow-up review hardened manual Start during crash recovery. A repeated Start now preserves an already-scheduled respawn instead of cancelling it and postponing recovery.

Files changed:
- `recog/copilot_supervisor.py` — keep an active respawn task and reset the budget only when no retry is pending.
- `tests/test_copilot_supervisor.py` — add a regression test for a repeated Start during respawn backoff.
- `_bmad-output/implementation-artifacts/deferred-work.md` — append one new pre-existing kill-failure finding; existing entries and their status/resolution fields were not modified.
- This story spec — record the review triage, verification, follow-up score, and final revision.

Review findings:
- Patches applied: 1 medium — pending respawn cancellation on repeated manual Start.
- Items deferred: 1 high — a failed `tmux kill-session` can leave a live Copilot session orphaned after lifecycle IDLE.
- Items rejected: 11 — intent-conflicting persistence/request-id proposals, explicit `autospawn=False` gate reversals, duplicate existing ledger findings, and non-actionable literal readings.
- Intent gaps: 0.
- Bad-spec findings: 0.

Follow-up review recommendation: false. Patched findings: high 0, medium 1, low 0; score = `3 × 1 + 1 × 0 = 3`, below the threshold of 5.

Verification:
- Regression test before the guard: 1 failed, proving the pending task was cancelled.
- Regression test after the guard: 1 passed.
- `venv-gigaam/bin/python -m pytest tests/test_copilot_supervisor.py -v`: 95 passed.
- `venv-gigaam/bin/python -m pytest tests/ -v`: 641 passed, 2 deselected.
- `venv-gigaam/bin/python -m compileall recog frontend`: passed.
- `git diff BASE_REVISION -- recog/copilot_supervisor.py | grep -c "class SessionLifecycle"`: 0.
- `git diff --check BASE_REVISION -- recog/copilot_supervisor.py tests/test_copilot_supervisor.py`: passed.
- Commit `eec08a0` contains both reviewed implementation paths; `git status --porcelain` is clean.

Residual risks:
- The newly deferred `TmuxRunner.kill()` return-code gap remains open in the append-only ledger.
- The story spec and deferred-work ledger are ignored workflow artifacts and are not part of commit `eec08a0`.
