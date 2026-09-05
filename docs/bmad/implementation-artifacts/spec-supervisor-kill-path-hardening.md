---
title: 'Supervisor kill-path hardening (DW-10, DW-12)'
type: 'bugfix'
created: '2026-07-25'
status: 'done'
review_loop_iteration: 0
baseline_revision: '8a335beaf5728e4ed14dd9a7bfa62b78e9603b41'
final_revision: '3bd771a7b4e4f8af3bf3ecb9607c3ac15f9619cd'
followup_review_recommended: false
context: []
warnings: []
---

<intent-contract>

## Intent

**Problem:** Two defects in the supervisor's Kill execution path (`recog/copilot_supervisor.py`). DW-10: `_grace_expire` computes `commands = self.lifecycle.timer_expired()` (state → IDLE, `Kill` queued) and then awaits `_execute(commands)`; a `meeting_started` frame processed while the kill is in flight takes the IDLE branch, spawns/adopts the still-live tmux session, and the stale queued `Kill` then destroys it — leaving SPAWNING with no session until the watchdog's `session_died` and a 5 s backoff. DW-12: `TmuxRunner.kill()` discards the `tmux kill-session` return code; since the machine moved to IDLE before the kill and `Watchdog.tick()` does not probe tmux in IDLE, a genuinely failed kill silently orphans a live Copilot session with no retry or visible failure.

**Approach:** Serialize command execution with an `asyncio.Lock` and have `_grace_expire` re-check `lifecycle.state is State.IDLE` under that lock immediately before executing the grace-expiry `Kill` — skipping the stale command if a spawn/adopt won the race. In `TmuxRunner.kill()`, check the return code: a non-zero rc with the session already gone stays a success (idempotent kill), a non-zero rc with the session still alive is retried a bounded number of times and finally raised as `RuntimeError`, which `_execute`'s existing per-command error path prints (surfaced).

## Boundaries & Constraints

**Always:**
- The DW-10 re-check must sit after a real suspension point (lock acquisition), so a concurrent spawn/adopt could actually have run before it; a purely synchronous check after `timer_expired()` is dead code and does not satisfy this spec.
- With the lock held by the grace-expiry kill, a concurrent frame-driven `Spawn` must wait and then observe the killed session (`alive()` False → fresh session), never adopt a session that is about to die.
- `kill()` stays idempotent: rc != 0 while `has-session` also fails (session gone / tmux server gone) returns success — no retry, no raise.
- Retry count and delay are class attributes on `TmuxRunner` so tests can shrink them.
- Preserve existing behavior: `_execute` per-command exception logging, `_spawn_in_flight` watchdog semantics, `SessionLifecycle` transition table, grace-timer sync in `_sync_grace_timer`.

**Block If:**
- The fix cannot be made without changing `SessionLifecycle`'s pure transition semantics (new states/inputs) or the bridge/webapp protocol.
- The execution lock provably deadlocks with `stop()` / respawn / watchdog paths and no lock-free ordering satisfies the Always rules.

**Never:**
- No re-adoption or state rollback after a persistently failed kill — the machine stays IDLE; surfacing means retry + raised error logged by `_execute`, not a new recovery state.
- No graceful stop of the Claude session (design decision D5 stands).
- Do not edit `.bmad-loop/` ledgers or run files.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Grace expiry, no interleave | GRACE timer fires, no concurrent frames | Kill executes, session killed, machine IDLE | No error expected |
| Spawn wins the race | `meeting_started` spawn holds the lock when the expired grace Kill tries to run (state now SPAWNING) | Stale Kill is skipped; runner receives no `kill()`; session survives under the new meeting | No error expected |
| Kill wins the race | Grace Kill holds the lock; `meeting_started` spawn arrives mid-kill | Spawn waits for the lock, then `alive()` is False → creates a fresh session for the new meeting | No error expected |
| Idempotent kill | `kill-session` rc != 0, `has-session` rc != 0 (session gone) | `kill()` returns normally, no retries beyond the alive check, no raise | No error expected |
| Failed kill, session alive | `kill-session` rc != 0 while `has-session` rc == 0, persisting across retries | Bounded retries, then `RuntimeError` raised | `_execute` catches and prints `command Kill() failed` |
| Transient kill failure | First `kill-session` fails with session alive, retry succeeds | `kill()` returns normally after the retry | No error expected |

</intent-contract>

## Code Map

- `recog/copilot_supervisor.py` -- all production changes: `TmuxRunner.kill()` (lines ~248-250), `Supervisor.__init__` (add lock), `Supervisor._execute` (lines ~589-612), `Supervisor._grace_expire` (lines ~709-713)
- `tests/test_copilot_supervisor.py` -- existing lifecycle/glue tests (FakeRunner at top records spawns/kills; `test_grace_expiry_kills_session` etc.); add new tests here following its given/when/then docstring style

## Tasks & Acceptance

**Execution:**
- `recog/copilot_supervisor.py` -- add `Supervisor._execute_lock = asyncio.Lock()` in `__init__`; split `_execute` into a locking entry (`async with self._execute_lock:` around the current body, keeping the `_spawn_in_flight` flag handling outside or inside consistently) and an unlocked inner helper -- so `_grace_expire` can hold the lock across its re-check + kill -- rationale: makes the DW-10 re-check race-free.
- `recog/copilot_supervisor.py` -- rewrite `_grace_expire`: after `timer_expired()` returns commands, return early on empty; otherwise acquire `_execute_lock`, re-check `self.lifecycle.state is State.IDLE`, skip execution when it is not (a spawn/adopt claimed the session), else run the inner executor -- rationale: DW-10.
- `recog/copilot_supervisor.py` -- rewrite `TmuxRunner.kill()`: loop up to `KILL_RETRIES` (default 3): run `kill-session`; rc == 0 → return; else if `not await self.alive()` → return (idempotent); else sleep `KILL_RETRY_DELAY_S` (default 0.5) between attempts; after the loop raise `RuntimeError` with rc and trimmed output -- rationale: DW-12.
- `tests/test_copilot_supervisor.py` -- add tests covering the I/O matrix rows: stale grace Kill skipped when a spawn holds the lock / has flipped state; spawn waiting out an in-flight kill gets a fresh session; kill idempotency (rc != 0, session gone); kill retry-then-success; kill persistent failure raises and `_execute` logs instead of crashing -- use injected fake `run` callables and zeroed retry delay.

**Acceptance Criteria:**
- Given a machine in GRACE whose timer has expired and queued a `Kill`, when a `meeting_started` spawn is processed before the kill executes (state left IDLE), then the runner records zero `kill()` calls and the session survives under the new meeting id.
- Given the grace-expiry kill in flight, when a `meeting_started` frame arrives mid-kill, then its spawn runs only after the kill completes and creates a fresh session (no adopt-then-destroy).
- Given `tmux kill-session` failing while `has-session` still succeeds, when `kill()` runs, then it retries up to its bounded budget and raises `RuntimeError` on exhaustion, and a `Kill` command routed through `_execute` logs the failure without crashing the supervisor.
- Given `tmux kill-session` failing while the session is already gone, when `kill()` runs, then it returns without raising and without extra retries.
- Given the full existing test suite, when it runs after the change, then every previously passing test still passes.

## Design Notes

The synchronous window between `timer_expired()` and `await self._execute(...)` contains no suspension point, so the re-check is only meaningful when placed after one. The lock provides it: whichever of {stale Kill, frame Spawn} acquires first fully settles before the other observes the world, collapsing the adopt-then-destroy interleave into two safe orders. `asyncio.Lock()` in `__init__` is loop-agnostic on Python 3.12. A task cancelled while awaiting the lock (e.g. `stop()`) propagates `CancelledError` without holding it.

## Verification

**Commands:**
- `source venv-gigaam/bin/activate && python -m pytest tests/test_copilot_supervisor.py -v` -- expected: all pass, including the new kill-path tests
- `python -m pytest tests/ -q` -- expected: no regressions across the suite
- `python -m compileall recog frontend` -- expected: clean compile

## Spec Change Log

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 3: (high 0, medium 0, low 3)
- defer: 4: (high 0, medium 3, low 1)
- reject: 9: (high 0, medium 0, low 9)
- addressed_findings:
  - `[low]` `[patch]` DW-10 guard test covered only the SPAWNING claim path (mutation narrowing the re-check to `is State.SPAWNING` survived the suite) — added `test_stale_grace_kill_skipped_when_claim_reaches_running`, mutation-checked to fail against the narrowed guard.
  - `[low]` `[patch]` Kill retry pacing was unpinned (mutations sleeping before every attempt or never both passed) — added `test_kill_retry_pacing_sleeps_only_between_attempts` recording `asyncio.sleep` calls: exactly attempts−1 delays of `KILL_RETRY_DELAY_S` on retry paths, zero on first-attempt success and the idempotent session-gone path.
  - `[low]` `[patch]` `kill()` docstring overclaimed ("verifying tmux actually did it" — only the rc != 0 path is verified) — narrowed to "a non-zero rc is verified against has-session before retrying or raising; a zero rc is trusted as-is".

Notable non-actioned triage: "spawn-wins race leaves meeting B adopting A's still-live session" rejected on the intent's own authority — DW-10 explicitly orders that a spawn/adopt processed at the boundary "is not destroyed", i.e. session survival over context purity. "IDLE + live orphan after exhausted kill retries with no status escalation" deferred, not an intent gap: the intent locates the DW-12 fix in `TmuxRunner.kill()` (rc check + surface/retry), and the ledger names the deficiency as "no retry or visible failure" — both delivered; lifecycle-level recovery is a separate decision (see deferred-work entries).

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 8: (high 0, medium 2, low 6)
- defer: 3: (high 2, medium 1, low 0)
- reject: 6: (high 1, medium 2, low 3)
- addressed_findings:
  - `[medium]` `[patch]` A frame-driven `Spawn` changed lifecycle to SPAWNING before acquiring `_execute_lock`, while `_spawn_in_flight` stayed false until after lock acquisition — moved queued-spawn accounting before the lock, used a counter for overlapping callers, and added a watchdog regression proving no respawn budget is consumed while the Spawn waits behind an in-flight Kill.
  - `[low]` `[patch]` The stale-grace guard was unpinned when a replacement meeting had already returned to GRACE — added a three-frame race test proving every non-IDLE claim skips the old Kill.
  - `[low]` `[patch]` Kill idempotency was tested only when the session disappeared after the first failure — added sequenced `has-session` results proving a later disappearance stops retries immediately.
  - `[low]` `[patch]` Retry policy tests self-referenced mutable production constants — pinned the specified defaults of three total attempts and a 0.5-second delay.
  - `[medium]` `[patch]` Race tests observed only `FakeRunner` counters and could not distinguish adoption from fresh creation — exercised real `TmuxRunner` behavior with stateful injected tmux runners for both spawn-wins survival and kill-wins `new-session`.
  - `[low]` `[patch]` Race tests used a fixed post-release sleep instead of awaiting the captured grace task — replaced it with explicit task completion and timeouts.
  - `[low]` `[patch]` The `_execute` error-path test asserted only the generic command prefix — pinned the surfaced return code and tmux diagnostic.
  - `[low]` `[patch]` Retry exhaustion and `_execute` logging were proved only as separate components — added the composite grace-expiry path through a real `TmuxRunner`, including bounded attempts, stderr details, and final IDLE state.

Notable non-actioned triage: the pre-existing `(Kill, Spawn)` continuation after a failed Kill and unbounded tmux subprocess wait were already recorded in `deferred-work.md`, so this pass did not duplicate or alter those orchestrator-owned entries. One newly surfaced pre-existing issue — `stop()` losing the in-flight grace-task reference before Kill completion — was appended as a new ledger entry only. Probe-error classification, cross-supervisor retry fencing, zero retry-budget mutation, and alternative wording-level readings were rejected as unsupported or outside the concrete runtime contract.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 2: (high 0, medium 1, low 1)
- defer: 0
- reject: 10: (high 5, medium 3, low 2)
- addressed_findings:
  - `[medium]` `[patch]` A queued replacement `Spawn` suppressed only an `OFFLINE` watchdog observation, so an `ONLINE`/`WORKING` pane from the old session could prematurely move the replacement lifecycle to `RUNNING` and report the old pane under the new meeting id — treat every watchdog observation as `SPAWNING` until the queued/executing Spawn settles, with a regression that pins readiness only after the flag clears.
  - `[low]` `[patch]` `_execute(())` acquired `_execute_lock` despite having no commands, making no-op lifecycle frames wait behind unrelated tmux I/O — return immediately for an empty command tuple and pin the non-blocking behavior while the lock is held.

Notable non-actioned triage: the failed-Kill continuation, IDLE orphan, unbounded tmux subprocess, stale respawn, and lost grace-task ownership findings already exist in the orchestrator-owned `deferred-work.md`, so this pass did not duplicate or alter them. Cross-meeting session survival in the spawn-wins order and the `has-session` non-zero classification are explicit intent choices; exception retries, a literal lock-owner interpretation of the race test, and an alternative retry-count reading are outside or resolved by the concrete contract.

## Auto Run Result

Status: done

Summary: Completed a fresh `baseline_revision..HEAD` review of the supervisor kill-path hardening. The pass prevents a queued replacement Spawn from being marked ready by the previous session's live pane and makes empty command tuples bypass the execution lock.

Files changed:
- `recog/copilot_supervisor.py` — keeps every watchdog observation at `SPAWNING` while a Spawn is queued/executing and returns immediately for empty command tuples.
- `tests/test_copilot_supervisor.py` — pins live-pane readiness fencing and non-blocking empty-command execution.

Review findings:
- Patches applied: 2 (high 0, medium 1, low 1).
- Items deferred: 0; `deferred-work.md` was not modified.
- Items rejected: 10 (high 5, medium 3, low 2), including explicit intent choices, unsupported alternative readings, and findings already owned by existing ledger entries.

Follow-up review recommendation: false — patched findings score `3 × 1 medium + 1 × 1 low = 4`, below the threshold of 5.

Verification:
- `venv-gigaam/bin/python -m pytest tests/test_copilot_supervisor.py -q` — 109 passed.
- `venv-gigaam/bin/python -m pytest tests/ -qq` — 735 passed, 2 deselected.
- `venv-gigaam/bin/python -m compileall -q recog frontend` — passed.
- `git diff --check` — passed.
- Commit `3bd771a7b4e4f8af3bf3ecb9607c3ac15f9619cd` contains both files in the reviewed diff.

Residual risks: Existing supervisor follow-ups remain in the orchestrator-owned deferred-work ledger; this pass created no new deferred entry and left every existing entry/status/resolution untouched. No residual worktree artifacts remain.
