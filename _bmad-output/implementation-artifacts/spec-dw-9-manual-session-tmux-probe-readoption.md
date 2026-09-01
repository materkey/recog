---
title: 'DW-9: Manual-session tmux-probe re-adoption at supervisor startup'
type: 'bugfix'
created: '2026-07-25'
status: done
review_loop_iteration: 0
baseline_revision: '709982ef67964ce76d6890261620bc38f1ad0c88'
final_revision: 'd0e23724ac6a1d1c09473efd2643c2ca12309834'
followup_review_recommended: false
context: []
warnings: []
---

<intent-contract>

## Intent

**Problem:** `./recog-service.sh restart` restarts the copilot supervisor (the tmux session deliberately survives), but the fresh supervisor lifecycle is IDLE, so `supervisor_hello` carries `current_meeting_id: None`, the bridge's `_manual_copilot_active` stays False, and `_reconcile` takes the orphan branch (`adopt(None)` → synthetic `meeting_ended` → GRACE → Kill) — a live manual Copilot session is reaped ~90 s after a full restart, the opposite of the restart-resilience plan D5 claims.

**Approach (human decision, resolution 2026-07-25 — SUPERSEDES the ledger's provisional-guess approach):** Encode the session *kind* in the tmux session name, so the startup probe **reads** the type instead of inferring it. Manual spawns create `copilot-manual`; every meeting-driven spawn keeps today's `copilot`. At startup the supervisor probes the manual name **only**: alive → `lifecycle.adopt(MANUAL_MEETING_ID)` and bind the runner to that name, so `supervisor_hello` carries `current_meeting_id: "manual"` and the existing bridge hello-handler re-arms `_manual_copilot_active` (its "no conference active" check already exists at `ktalk_bridge.py:3489-3493`). A surviving *conference* session is named `copilot`, is never adopted at startup, and therefore keeps today's untouched `_reconcile` path. The live tmux session is still the only marker — no persistence file; the name is the session's own identity, not a side-channel. Update CLAUDE.md and the plan's D5 notes to drop the deferral.

**Why this supersedes the previous approach:** the earlier reading adopted *any* live session as "manual" and relied on a provisional flag corrected by the bridge's first `meeting_state` reply. That reply is the supervisor's own hello echoed back (the bridge re-arms the manual mirror *because* the hello said "manual"), so the guess confirmed itself and a later `meeting_started(<conf-id>)` killed a live conference copilot. Naming removes the ambiguity at its source instead of managing it downstream: no guess, no provisional flag, no correction window.

## Boundaries & Constraints

**Always:**
- The tmux session name IS the type marker. `TMUX_SESSION = "copilot"` stays the meeting-driven/default name — so a session left behind by a pre-upgrade supervisor reads as a conference session, which is the safe interpretation (today's behavior). Manual spawns use a new sibling constant (e.g. `TMUX_SESSION_MANUAL = "copilot-manual"`). The name is derived from `spawn()`'s **existing** `meeting_id` argument: `MANUAL_MEETING_ID` → manual name, anything else → default name.
- `TmuxRunner` operates on the *current* session's name for every tmux verb it already issues (`new-session`, `kill-session`, `has-session`, `send-keys`, `capture-pane`): the name becomes per-session state set at spawn/adoption time instead of a value fixed in `__init__`. A preemption (kill manual → spawn under a conference id) therefore kills the manual name and creates the default one.
- The startup probe runs once at the top of `Supervisor.run()` (before the watchdog starts and before the first connect), only when the machine is IDLE, and targets **only the manual name**. Alive → `lifecycle.adopt(MANUAL_MEETING_ID)`, bind the runner to the manual name, log one stderr line. Not gated on `autospawn` — manual-session machinery works regardless of the flag (D3).
- There is **NO provisional flag and no reconcile-time correction**: a manual session can no longer be confused with a conference one, so there is nothing to correct. `_reconcile()` keeps its current three branches verbatim.
- A surviving conference session (default name alive, manual name dead) is **NOT** adopted at startup: hello reports `None` and the existing `adopt(None)` wildcard branch in `_reconcile` preserves it byte-for-byte as today, keeping the tested invariant "a supervisor restart mid-meeting must not kill a live copilot" (`test_reconcile_adopts_live_session_mid_meeting`).
- Both names alive at startup → **conference wins** (one copilot per machine, inherited design): do not adopt manual, and kill the stale manual session so it cannot linger untracked.
- Bridge code is already complete for this flow (hello re-arm + `_compute_meeting_state` manual reporting) — do not modify `ktalk_bridge.py`.
- Existing behavioral tests must stay green. Tests that hardcode the tmux session name may be updated **only** to reflect the new naming — never to weaken an assertion. `FakeRunner` has no `alive`, so the default probe returns False and existing `run()`-based hello tests are unaffected.

**Block If:** per-kind naming cannot be implemented without changing `SessionLifecycle`'s public transition semantics — the pure state machine must stay untouched (naming belongs to `TmuxRunner`/`Supervisor`, not to the machine).

**Never:**
- No persistence file, no on-disk marker, no tmux environment/option tagging — the session's existence and its name are the only markers (honors the original no-persistence intent).
- Do not edit `_bmad-output/implementation-artifacts/deferred-work.md` (orchestrator-owned).
- `recog-service.sh`: `restart` must keep NOT reaping either session (its skip of `kill_copilot_tmux` is deliberate). `stop`/`uninstall` must reap **both** names — this is the one change the script is allowed, because `kill_copilot_tmux` hardcodes `-t copilot` (line ~152) and would otherwise leave a `copilot-manual` session behind forever, breaking "terminal intent: leave nothing behind".
- Do not touch `_live_meeting_id()` GRACE/IDLE→None semantics (Stop must stay un-defeatable by reconnects).

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Full restart, manual session alive | `copilot-manual` alive, `copilot` dead | adopt as RUNNING("manual"), runner bound to `copilot-manual`; hello carries `"manual"`; bridge re-arms; `meeting_state {active, "manual"}` → same-id no-op; 0 kills, 0 spawns | — |
| Full restart mid-conference | `copilot` alive, `copilot-manual` dead | no startup adoption; hello carries `None`; existing `adopt(None)` wildcard branch preserves the session when the extension's `meeting_started(<conf-id>)` arrives; 0 kills, 0 spawns | — |
| Both sessions alive at startup | `copilot` AND `copilot-manual` alive | conference wins: no manual adoption (hello `None`); the stale `copilot-manual` is killed; runner stays on the default name | kill failure logged, startup continues |
| Restart, no session | both names dead | no adoption; hello carries `None`; behavior identical to today | — |
| Probe raises | `tmux` unavailable / probe exception | probe logs and returns False → no adoption (existing behavior) | logged to stderr |
| Session dies right after adoption | manual probe said alive, session gone by first watchdog tick | watchdog → `session_died` → respawn "manual" (under the manual name) with existing backoff | existing path |
| `AUTOSPAWN=0`, manual session alive | `copilot-manual` alive; meeting frames inert | adoption + hello `"manual"` still happen (not autospawn-gated); `meeting_state` ignored per the existing gate — no flag exists to go stale | — |
| Conference preempts an established manual session | RUNNING("manual") on `copilot-manual`; conference `meeting_started(A)` | unchanged documented precedence: Kill (`copilot-manual`) + Spawn (`copilot`) bound to `A` | — |

</intent-contract>

## Code Map

- `recog/copilot_supervisor.py` -- `TMUX_SESSION` (line 48) + new manual-name sibling constant; `MANUAL_MEETING_ID` (line 55); `TmuxRunner.__init__` (`session_name` param/attr, lines ~193-199) and its five tmux call sites — `spawn`/`new-session` (~241), `kill-session` (~267), `has-session` (~276), `send-keys` (~280), `capture-pane` (~284); `spawn(meeting_id)` (~227 — already receives the id, currently ignores it for naming); `Supervisor.run()` (startup probe site, ~792); `SessionLifecycle.adopt()` (reuse as-is, ~150); `_reconcile()` (~576 — **no change**, listed so the re-drive does not touch it); `_live_meeting_id()` (~653 — no change)
- `recog-service.sh:150-154` -- `kill_copilot_tmux` hardcodes `kill-session -t copilot`; must reap both names. `cmd_restart` (~244) keeps skipping it — do not change that
- `ktalk_bridge.py:3482-3493` -- hello re-arm (`current_meeting_id == "manual"` + no active conference → `_set_manual_copilot_active(True)`); `_compute_meeting_state` (line ~1481) — read-only reference, no changes
- `tests/test_copilot_supervisor.py` -- `FakeRunner` (no `alive`), mock-bridge helpers `_start_mock_bridge`/`_collect_hello` (line ~863), reconcile tests (lines ~489-577) — add new tests here; any test asserting the literal session name updates to the new naming
- `CLAUDE.md` -- "Manual-session restart recovery (webapp plan D5)" bullet: last sentence defers full-restart recovery — rewrite. The success-signal line referencing `tmux -L recog-copilot ls` must mention both session names
- `docs/plans/completed/20260724-webapp-copilot-live.md` -- D5 note (lines 65-84; **line 72-73 currently claims "This covers both restart permutations", which was false before this story — it becomes true only once this lands**) and the "restart resilience (known gap)" smoke item (lines 404-407) — drop the deferral

## Tasks & Acceptance

**Execution:**
- `recog/copilot_supervisor.py` -- add the manual-name constant next to `TMUX_SESSION`; make `TmuxRunner` name-aware: derive the name for a spawn from `spawn()`'s `meeting_id` (`MANUAL_MEETING_ID` → manual name, else default), store it as the runner's current-session name, and have `kill`/`alive`/`send_keys`/`capture_pane` target that name
- `recog/copilot_supervisor.py` -- add a probe for a *specific* session name (generalize the existing aliveness probe rather than adding a second mechanism), plus a private async `_startup_manual_readoption()` that, when `lifecycle.state is State.IDLE`, probes the manual name: alive AND default name dead → `lifecycle.adopt(MANUAL_MEETING_ID)` + bind the runner to the manual name + one stderr log line; alive AND default name alive → kill the manual session, no adoption; call it at the top of `run()` before the watchdog starts. The hello built later in `run()` then reports `"manual"` via the untouched `_live_meeting_id()`
- `recog/copilot_supervisor.py` -- **no changes to `_reconcile()`, `handle_frame()`, or `SessionLifecycle`**: with typed names there is no guess to correct
- `recog-service.sh` -- `kill_copilot_tmux` reaps both session names (`stop`/`uninstall` only; `cmd_restart` keeps skipping it)
- `tests/test_copilot_supervisor.py` -- add tests covering I/O-matrix rows 1-4, 7 and 8: (a) manual name alive, default dead → hello over the mock bridge carries `"manual"` and 0 kills / 0 spawns after `meeting_state {active, "manual"}`; (b) default name alive, manual dead → hello `None` and the extension's later `meeting_started(<conf-id>)` yields 0 kills / 0 spawns (the regression the previous attempt introduced — pin it); (c) both alive → no adoption, manual name killed exactly once, hello `None`; (d) neither alive → identical to today (existing `test_supervisor_sends_hello_on_connect` passes unmodified); (e) `AUTOSPAWN=0` + manual alive → adoption and hello `"manual"` still happen; (f) established RUNNING("manual") + conference `meeting_started` → kill of the manual name + spawn of the default name (precedence unaffected). Name tests in the repo's `given – when – then` style
- `CLAUDE.md` -- rewrite the D5 bullet's final sentence: a full `./recog-service.sh restart` is now covered, because the manual session carries its kind in its tmux name and the fresh supervisor reads it back; also update the success-signal line to mention both session names
- `docs/plans/completed/20260724-webapp-copilot-live.md` -- update the D5 note (lines 72-73's "covers both restart permutations" claim becomes true) and replace the "restart resilience (known gap)" smoke bullet with the now-implemented behavior
- Documented residual limitation (add to the plan's **Known v1 limitations** list, not a task): a supervisor restart inside the 90 s grace window resurrects a manual session the user explicitly stopped — the session still exists during grace, and its existence is the marker

**Acceptance Criteria:**
- Given a live manual-named tmux session, no live default-named session and no conference meeting, when the supervisor process starts fresh, then its first `supervisor_hello` carries `current_meeting_id: "manual"` and after the bridge's `meeting_state {active: true, meeting_id: "manual"}` reply the runner records zero kills and zero spawns.
- Given a live default-named tmux session and no live manual-named session, when the freshly-started supervisor connects and the extension's conference `meeting_started(A)` arrives afterwards, then the session survives — zero kills and zero spawns — and hello reported `current_meeting_id: None`.
- Given both a live default-named and a live manual-named session, when the supervisor starts, then it performs no manual adoption, hello carries `None`, and the manual-named session is killed exactly once.
- Given no live tmux session under either name, when the supervisor starts, then behavior is byte-identical to today (hello `None`; existing test suite passes unmodified).
- Given an established manual session, when a conference `meeting_started` arrives, then kill+spawn preemption still occurs, killing the manual-named session and spawning the default-named one.
- Given `RECOG_COPILOT_AUTOSPAWN=0` and a live manual-named session, when the supervisor starts, then adoption still happens and hello carries `"manual"`.

## Design Notes

Why naming rather than a provisional guess: the previous reading could not distinguish a manual session from a conference session or a stale orphan *because the tmux session name was constant* — so it guessed "manual" and tried to correct the guess from the bridge's first `meeting_state` reply. That reply is not independent evidence: the bridge re-arms its manual mirror **because** the hello said "manual", so the guess confirmed itself, the correction never fired, and the extension's later `meeting_started(<conf-id>)` killed a live conference copilot. Giving the name one more bit of information removes the ambiguity where it originates. The state machine, `_reconcile`, and the whole conference path stay untouched, which is why the tested invariant "a supervisor restart mid-meeting must not kill a live copilot" cannot regress: a conference session is never adopted at startup at all.

Trade-off retired by this approach: the previous spec had to accept "a former orphan is treated as manual". A conference session now carries the default name, so it can never be read as manual — that accepted loss disappears.

Trade-off that remains (documented, not fixed): a supervisor restart inside the 90 s grace window resurrects a manual session the user explicitly stopped. It is intrinsic to "the session's existence is the marker" — during grace the session genuinely still exists.

Backward compatibility: a session created by a pre-upgrade supervisor is named `copilot`, so after the upgrade it reads as a conference session — today's behavior, the safe default.

Bridge-side condition "no conference active" is enforced at `ktalk_bridge.py:3489-3493`; supervisor-side there is nothing to check at probe time (the bridge is not connected yet) — and with typed names nothing needs checking.

## Spec Change Log

### 2026-07-25 — escalation resolved (human decision, interactive session)

Resolved the blocking `intent_gap` by **replacing the approach**, not by selecting one of the three options the escalation offered (all three managed the ambiguity; none removed it).

**Decision:** encode the session kind in the tmux session name (`copilot` = meeting-driven/default, `copilot-manual` = manual). The startup probe reads the type instead of inferring it, so the provisional flag, its lifetime rules, and the reconcile-time correction are all deleted from the intent.

**Amended:** Intent/Approach (superseding paragraph + rationale); `Always` (naming rules, name-aware runner, probe scoped to the manual name, no provisional flag, both-alive → conference wins); `Block If`; `Never` (tmux *option/env* tagging still forbidden but the session *name* is now the marker; `recog-service.sh` change now explicitly allowed for `kill_copilot_tmux` only); I/O matrix (8 rows rewritten); Code Map (added `recog-service.sh`, the five `TmuxRunner` tmux call sites, `spawn`'s unused `meeting_id`); Tasks & Acceptance (6 criteria); Design Notes.

**Consequences for the previous review pass:** the blocking finding is designed out. Four of the moot `patch` findings (flag-lifetime comment false under `AUTOSPAWN=0`; unlogged provisional resolution; `autospawn=0` test not asserting the flag; provisional correction adopting without a `session_alive` check) reference machinery that no longer exists. The two documentation findings survive and are folded into the tasks; the "ended-meeting orphan immortalized as manual" trade-off is retired by the naming itself.

**Not restored:** the attempted patch implements the superseded provisional-flag reading, so the re-drive starts from scratch against this corrected intent.

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 2: (high 0, medium 0, low 2)
- defer: 1: (high 0, medium 1, low 0)
- reject: 13: (high 4, medium 4, low 5)
- addressed_findings:
  - `[low]` `[patch]` The persistent terminal-cleanup failure test proved retries only for `copilot`, so an early return could leave `copilot-manual` untested. Extended the guard to require three kill/probe attempts and a visible warning for both names.
  - `[low]` `[patch]` The linked auto-copilot operations plan still used untargeted tmux kill/attach commands after DW-9 introduced two session names. Updated the respawn and manual-recovery commands to use the exact session-kind targets.

### 2026-07-25 — Review pass (fresh follow-up)
- intent_gap: 0
- bad_spec: 0
- patch: 6: (high 1, medium 2, low 3)
- defer: 0
- reject: 14
- addressed_findings:
  - `[high]` `[patch]` Bare tmux targets prefix-match (`-t copilot` matches a lone `copilot-manual`), so manual-only startup was misclassified as both-alive and killed the session it should adopt. Fixed: every `TmuxRunner` target verb and service cleanup now uses exact `=name` targets; verified against real tmux 3.7a.
  - `[medium]` `[patch]` `spawn()` continued after exhausting stale-sibling kill retries, allowing both session kinds to remain live. Fixed: abort the target spawn until sibling cleanup succeeds.
  - `[medium]` `[patch]` `recog-service.sh stop`/`uninstall` suppressed persistent tmux cleanup failures and could report success with a session still alive. Fixed: bounded retries, post-kill probes, and a visible non-zero failure.
  - `[low]` `[patch]` Sibling cleanup lacked the inverse manual-spawn/default-sibling regression case. Added the symmetric test.
  - `[low]` `[patch]` Manual binding had no direct coverage for watchdog-facing `send_keys()` and `capture()`. Added exact-target assertions for both verbs.
  - `[low]` `[patch]` The terminal service commands had no automated cleanup contract. Added shell-shim tests for `stop`, `uninstall`, and persistent cleanup failure.

### 2026-07-25 — Review pass (re-drive, typed-name approach)
- intent_gap: 0
- bad_spec: 0
- patch: 7: (high 0, medium 2, low 5)
- defer: 0
- reject: 12
- addressed_findings:
  - `[medium]` `[patch]` Cross-kind kill failure orphaned a session (preemption Kill(`copilot-manual`) raises → spawn rebinds to the sibling name, alive-gate probes the wrong name → two live copilots). Fixed: `TmuxRunner.spawn()` now reaps a stale sibling-named session best-effort after the rebind; pinned by 2 new tests.
  - `[medium]` `[patch]` Matrix row 3 error-handling column ("kill failure logged, startup continues") had no covering test — fake tmux always killed successfully. Added `test_startup_both_alive_manual_kill_failure_logged_startup_continues`.
  - `[low]` `[patch]` CLAUDE.md "a reconnect inside the grace window cannot defeat Stop" needed the DW-9 restart-path qualifier — added inline next to the guarantee.
  - `[low]` `[patch]` `Supervisor` docstring still required `async kill()` (no-arg) from the runner — updated to the `session_name=None` contract.
  - `[low]` `[patch]` Restart-resilience smoke verified presence, not function — now also requires a post-restart chat round-trip (`copilot_reply`).
  - `[low]` `[patch]` `recog-service.sh` both-names reap had no verification anywhere — added a stop-reaps-both-names smoke line to the plan.
  - `[low]` `[patch]` `_startup_manual_readoption` docstring said "Probes ONLY the manual name" while the code also consults the default name for both-alive arbitration — reworded.

**reject (12):** startup probe/kill no-retry (both branches explicitly specified by matrix rows 3/5 error handling); residual "understated" (doc plainly says "resurrects"; a stop-tombstone file contradicts the Never clause); pre-upgrade session reads as conference (accepted in Design Notes); "conference wins" tie-break critique (explicit intent decision); rebind()/lock-discipline architecture suggestions (style/hypothetical-future); fixed sleep(0.2) flake claim (can only produce a vacuous pass, not a flaky failure; the no-op is additionally pinned elsewhere); `subcommands()` assert "timing-coupled" claim (refuted: watchdog issues no tmux calls while IDLE); `copilot-manual` hardcoded in 4 places (inherent shell/Python split, comment already flags DW-9); `session_alive_probe` bypass (deliberate per spec); cross-process bridge integration test (spec forbids touching the bridge; hello re-arm covered by existing bridge tests); plus the edge-case duplicate of the cross-kind kill finding counted once.

### 2026-07-25 — Review pass
- intent_gap: 1: (high 1, medium 0, low 0)
- bad_spec: 0
- patch: 8: (high 0, medium 3, low 5)
- defer: 0
- reject: 4
- addressed_findings:
  - none

**intent_gap (blocking):** Provisional "manual" guess self-confirms — on a full restart mid-conference the supervisor's hello lands before the extension rejoins, the fresh bridge re-arms the manual mirror and replies `{active: true, meeting_id: "manual"}`; `_reconcile` clears the provisional flag as a same-id no-op, and the extension's later `meeting_started(<conf-id>)` hits non-provisional RUNNING("manual") → Kill+Spawn of the surviving conference session (pre-DW-9 `adopt(None)` wildcard preserved it). Converged on by 3 of 4 reviewers (adversarial, edge-case-hunter, verification-gap). Root cause is inside `<intent-contract>`: Always #2 mandates unconditionally clearing the provisional flag on the first `meeting_state` reconcile, and Always #1 scopes the correction to `_reconcile` only — every fix (keep the flag across the self-echo, consult it in `meeting_started`, bound it with a TTL) contradicts that text, and the captured intent does not select between "keep the correction window open" and "accept kill+respawn as a documented limitation".

**patch (moot per cascade, recorded for the resolver):** (medium) Stop-during-grace: a supervisor restart within the 90 s grace window resurrects a session the user explicitly stopped — intrinsic to the no-persistence marker principle, needs a documented-limitations note; (medium) ended-meeting orphan sessions are immortalized as "manual" (spec-accepted trade-off, but undocumented in CLAUDE.md/plan); (medium) docs claim full restart-permutation coverage without listing these residual gaps; (low) flag-lifetime comment is false under `AUTOSPAWN=0` (meeting_state early-returns before `_reconcile`, flag never cleared); (low) provisional resolution (confirm/correct/GRACE) is unlogged; (low) `autospawn=0` test never asserts the flag after the inert `meeting_state`; (low) provisional correction branch adopts without checking `session_alive`; (low) startup probe has no timeout guard before the first connect.

**reject:** zombie-pane adoption (watchdog parks it as visibly `blocked` — fail-visible by design); "two representations of unknown ownership" architecture critique (concrete bugs covered by the intent_gap finding); probe false-negative retry at boot (a surviving session implies a running tmux server, `has-session` is prompt); `stop()` not resetting the flag (Supervisor instances are not reused in-process in production).

Attempted change saved as [spec-dw-9-manual-session-tmux-probe-readoption-attempted.patch](spec-dw-9-manual-session-tmux-probe-readoption-attempted.patch); working tree reverted to baseline `709982e` (109 supervisor tests green post-revert).

## Verification

**Commands:**
- `source venv-gigaam/bin/activate && python -m pytest tests/test_copilot_supervisor.py -q` -- expected: all pass (existing + new)
- `python -m pytest tests/ -q` -- expected: no regressions elsewhere
- `python -m compileall recog` -- expected: clean

**Manual checks (if no CLI):**
- CLAUDE.md D5 bullet and plan D5/smoke sections no longer state the full-restart deferral; both describe the tmux-probe re-adoption.

## Auto Run Result

Status: done

Summary: Fresh baseline-to-HEAD review of the typed tmux-session implementation. The implementation keeps the manual/default session distinction, startup re-adoption, conference precedence, and terminal cleanup behavior intact. This pass added two low-severity review patches: complete failure-path coverage for both cleanup targets and exact session targets in the linked operations plan.

Files changed:
- `CLAUDE.md` — documents manual-session recovery across a full service restart and the two tmux session kinds.
- `docs/plans/completed/20260724-webapp-copilot-live.md` — records the implemented D5 restart path, terminal cleanup smoke, and the grace-window residual.
- `recog-service.sh` — reaps both exact tmux session names for terminal stop/uninstall commands.
- `recog/copilot_supervisor.py` — makes tmux operations session-kind aware and re-adopts a surviving manual session at startup.
- `tests/test_copilot_supervisor.py` — covers typed-name startup/preemption/cleanup behavior and now proves persistent cleanup continues for both names.
- `docs/plans/completed/2026-07-02-auto-copilot-session.md` — uses exact tmux targets in respawn and manual-recovery commands.
- `_bmad-output/implementation-artifacts/deferred-work.md` — appends one new pre-existing shutdown-race entry; no existing ledger entry was changed.

Review findings:
- Patches applied: 2 low.
- Items deferred: 1 medium pre-existing `launchctl unload` / tmux cleanup race.
- Items rejected: 13 (4 high, 4 medium, 5 low) after deduplication; they were explicit intent trade-offs, pre-existing accepted behavior, or unsupported hypothetical failures.

Follow-up review recommendation: false. Patched findings were high 0, medium 0, low 2; score = `3 × 0 + 2 = 2`.

Verification:
- `venv-gigaam/bin/python -m pytest tests/test_copilot_supervisor.py -q` — 126 passed.
- `venv-gigaam/bin/python -m pytest tests/ -q` — 794 selected passed, 2 deselected.
- `venv-gigaam/bin/python -m compileall -q recog` — passed.
- `bash -n recog-service.sh` — passed.
- `git diff --check 709982ef67964ce76d6890261620bc38f1ad0c88 --` — passed.
- Commit `d0e23724ac6a1d1c09473efd2643c2ca12309834` contains the review patches; the aggregate baseline-to-final diff still contains every originally reviewed file.

Residual risks:
- The pre-existing terminal shutdown race is recorded as a new deferred-work entry.
- The documented 90-second grace-window resurrection after a supervisor restart remains an accepted v1 limitation.
- The real launchd + tmux + post-restart chat round-trip remains a documented manual smoke; this review verified the modeled and shell-shim surfaces.
- No residual tracked or untracked Git artifacts remain.
