---
title: 'Bridge relays manual spawn/stop and tracks manual session state'
type: 'feature'
created: '2026-07-24'
status: 'done'
baseline_revision: 'fd174ac'
final_revision: '902aa83'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/docs/plans/20260724-webapp-copilot-live.md'
  - '{project-root}/_bmad-output/specs/spec-webapp-copilot-live/SPEC.md'
warnings: [oversized]
---

<intent-contract>

## Intent

**Problem:** The bridge has no notion of a manually started copilot session. There is no route for `copilot_spawn_request` / `copilot_stop_request` to reach `_supervisor_clients`, `_compute_meeting_state` reports `active` only for conference meetings (so a supervisor reconnect during a manual session would GRACE-kill it), the sessions-zero → offline merge is gated on `_is_meeting_active()` (so a manual copilot whose Claude session never registered would keep a false 🟢 chip), and `copilot_chat_message` stamps the orphan meeting id onto every request (so an orphan-meeting rotation mid-flight drops a manual reply as stale).

**Approach:** Add a bridge-side `_manual_copilot_active` flag with handlers for the two new frame types (relay to `_supervisor_clients` the way `_emit_lifecycle` does), give `_compute_meeting_state` a manual branch under conference precedence, re-arm the flag from `supervisor_hello.current_meeting_id` (D5), clear it on conference preemption with a `manual_preempted` notice to subscribers, and route the agent-state merge through a new `_copilot_should_be_alive()` predicate.

## Boundaries & Constraints

**Always:**
- TDD: failing tests in `tests/test_ktalk_bridge.py` first, given–when–then docstrings as in that file.
- Conference meetings take precedence over manual state everywhere the two can conflict.
- Every reject/failure path replies to the **originator socket** echoing the frame's `request_id`, so story 6 can bind the error to the click.
- Supervisor relay swallows per-client send errors with a log (`_emit_lifecycle`, `ktalk_bridge.py:1163-1170`, is the reference).
- Snapshot client sets under `self._lock` before iterating.
- Extension flow stays byte-identical: no new frame reaches `_connected_clients`, and existing extension chat keeps its current `meeting_id` stamping (an extension chats only inside a conference, where `_is_meeting_active()` is True).

**Block If:**
- Clearing the manual flag on conference preemption cannot be done without changing an existing conference lifecycle frame's shape or emit order.

**Never:**
- Do NOT change `_is_meeting_active` itself — other callers (`_compute_meeting_state`, the conference-precedence checks) depend on its conference-only meaning. Add `_copilot_should_be_alive()` instead.
- No `recog/copilot_supervisor.py`, `recog/web/`, or JS changes (stories 3–6). The bridge only relays the two frames; it never interprets supervisor state.
- No guard rejecting `copilot_stop_request` during a conference — a stray stop is already inert at the supervisor (`SessionLifecycle.meeting_ended` no-ops on a mismatched id, `copilot_supervisor.py:111-114`).
- No persistence of the manual flag; recovery is `supervisor_hello`-driven only (D5).

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Spawn relay | Supervisor registered, no conference; `copilot_spawn_request {request_id}` from a subscriber socket | Frame forwarded verbatim to `_supervisor_clients`; `_manual_copilot_active` becomes True | Per-client send errors swallowed + logged |
| Spawn during conference | `_is_meeting_active()` True; spawn request | NOT forwarded; flag unchanged; originator receives `copilot_status {request_id, status: "conference_active"}` | No error expected |
| Spawn, no supervisor | `_supervisor_clients` empty; spawn request | Originator receives `copilot_status {request_id, status: "supervisor_disconnected"}`; flag stays False | No error expected |
| Stop relay | Supervisor registered, flag True; `copilot_stop_request {request_id}` | Frame forwarded; flag becomes False | Send errors swallowed |
| Stop, no supervisor | `_supervisor_clients` empty, flag True; stop request | Originator receives `copilot_status {request_id, status: "supervisor_disconnected"}`; flag still cleared | No error expected |
| Malformed spawn/stop | `request_id` missing or not a non-empty string | Ignored with a log; no relay, no flag change | Logged to stderr |
| Manual meeting_state | Flag True, no conference; `_compute_meeting_state()` | `{active: True, meeting_id: "manual", meeting_title: None}` | No error expected |
| Conference precedence | Flag True AND conference active; `_compute_meeting_state()` | Conference id/title reported, never `"manual"` | No error expected |
| Hello re-arm (D5) | Flag False, no conference; `supervisor_hello {current_meeting_id: "manual"}` | Flag re-armed **before** the reply is computed → reply `{active: True, meeting_id: "manual"}` | No error expected |
| Hello during conference | Conference active; `supervisor_hello {current_meeting_id: "manual"}` | No re-arm; reply carries the conference meeting | No error expected |
| Conference preemption | Flag True; extension `meeting_join` starts a conference | Flag cleared; `copilot_status {request_id: "", status: "manual_preempted"}` broadcast to `_subscriber_clients` only | Per-client errors swallowed |
| Manual sessions-zero | Flag True, no conference, supervisor `online`, `claude_sessions {count: 0}` past the debounce | Effective agent state `offline` (chip must not show 🟢) | No error expected |
| Manual chat escape | No conference active; `copilot_chat_message` | Forwarded `copilot_user_message` carries `meeting_id: None`; pending entry records `""` | No error expected |
| Manual reply after rotation | Manual chat pending as `""`, current meeting rotated, then `copilot_channel_reply` | Reply still broadcast (not dropped as stale) | No error expected |

</intent-contract>

## Code Map

- `ktalk_bridge.py:315-359` -- client sets + agent-merge state; `_manual_copilot_active` is declared here
- `ktalk_bridge.py:1144-1170` -- `_emit_lifecycle`: supervisor fan-out pattern to mirror for the two relays
- `ktalk_bridge.py:1172-1196` -- `_is_meeting_active`: conference-only predicate, **unchanged**
- `ktalk_bridge.py:1198-1220` -- `_compute_meeting_state`: gains the manual branch
- `ktalk_bridge.py:1233-1256` -- `_update_sessions_zero_anchor`: `_is_meeting_active()` → `_copilot_should_be_alive()`
- `ktalk_bridge.py:1258-1282` -- `_effective_agent_state`: same swap
- `ktalk_bridge.py:2288-2294` -- conference `meeting_started` emit path: preemption hook
- `ktalk_bridge.py:2661-2749` -- `copilot_chat_message`: `meeting_id` capture (`:2685`) and pending record (`:2749`)
- `ktalk_bridge.py:2948-2988` -- `supervisor_hello`: re-arm site, must precede `_compute_meeting_state()`
- `ktalk_bridge.py:1093-1120` -- `_broadcast_copilot_frame` (story 1): dual-set fan-out; the subscriber-only notice mirrors its locking
- `tests/test_ktalk_bridge.py:646` `RecordingClient`, `:3085` `_ktalk_storage`, `:3262` `_bridge_with_active_ktalk`, `:3271` `_agent_states`, `:3553` `_copilot_frames`, `:3566` `_bridge_with_extension_and_subscriber`

## Tasks & Acceptance

**Execution:**
- `tests/test_ktalk_bridge.py` -- add a manual-copilot test group covering every I/O Matrix row (reuse the helpers above) -- failing tests first per TDD
- `recog/ktalk_bridge.py` -- declare `_manual_copilot_active: bool = False` beside the supervisor/agent-merge state with a comment on its single-owner semantics (D5)
- `recog/ktalk_bridge.py` -- add `copilot_spawn_request` / `copilot_stop_request` branches to `_process_message`: validate `request_id`, reject spawn with `conference_active` while `_is_meeting_active()`, relay to `_supervisor_clients`, maintain the flag, and reply `supervisor_disconnected` to the originator when no supervisor is registered
- `recog/ktalk_bridge.py` -- extend `_compute_meeting_state` with the manual branch (conference first) and re-arm the flag inside `supervisor_hello` **before** the reply is computed
- `recog/ktalk_bridge.py` -- clear the flag in the conference `meeting_started` emit path and broadcast `manual_preempted` to subscriber clients only
- `recog/ktalk_bridge.py` -- add `_copilot_should_be_alive()` and use it in `_update_sessions_zero_anchor` + `_effective_agent_state`, leaving `_is_meeting_active` untouched
- `recog/ktalk_bridge.py` -- in `copilot_chat_message`, capture `meeting_id` only while `_is_meeting_active()`, else `None` (orphan/mic-only staleness escape)

**Acceptance Criteria:**
- Given the extension-facing suite, when it runs after the change, then every pre-existing `tests/test_ktalk_bridge.py` assertion still holds unchanged (backward-compatibility bar). A fixture may only be completed — never an assertion relaxed — and only where the old fixture failed to model the conference it claims to describe.
- Given a spawn request that the bridge rejects or cannot relay, when the handler returns, then `_supervisor_clients` received nothing at all for that request.
- Given the completed change, when `_is_meeting_active` is diffed against the baseline, then its body is byte-identical and every conference-precedence check still calls it.

## Spec Change Log

### 2026-07-24 — Acceptance criterion corrected (no code loopback)

- **Triggering finding:** three reviewers independently reported that the diff violates AC 1 ("every pre-existing test still passes **unmodified**") because two pre-existing tests were edited.
- **Amended:** AC 1 now bars relaxing an assertion rather than editing a fixture. Verified by all three reviewers (and independently re-checked here): both edits add fixture lines only — `_load_meta` returning `{"source": "ktalk"}` plus a connected extension client — and every original assertion survives untouched.
- **Known-bad state avoided:** the literal wording was unachievable together with the I/O Matrix row "Manual chat escape". Satisfying it would have required gating the chat stamp on a predicate other than `_is_meeting_active()`, which the invocation intent forbids. No code was reverted: a re-derivation from the corrected AC yields byte-identical code.
- **KEEP:** the fixture completions at `tests/test_ktalk_bridge.py:1302-1312` and `:1866-1868` — registering the originating extension in `_connected_clients` also strengthens the pre-existing `extension_ws.sent == []` assertion.

## Review Triage Log

### 2026-07-24 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 5: (high 0, medium 1, low 4)
- defer: 2: (high 0, medium 2, low 0)
- reject: 13: (high 0, medium 5, low 8)
- addressed_findings:
  - `[medium]` `[patch]` `_relay_to_supervisors` returned True for a non-empty `_supervisor_clients` set rather than for actual delivery, so a supervisor socket in its closing handshake (not yet reaped by `_handle_client`'s finally-block) armed `_manual_copilot_active` with no session behind it and returned no error to the user — the exact state the handler's own comment refuses to create. Now tracks delivery inside the `try` and returns it; two tests added (sole dead supervisor → `supervisor_disconnected` + flag False; dead alongside live → live still delivered, flag armed, no exception escapes). Both re-verified as load-bearing by reverting the fix in isolation.
  - `[low]` `[patch]` TOCTOU between the `conference_active` guard and arming the mirror: the relay awaits `client.send`, and a concurrent `meeting_join` completing in that window runs `_preempt_manual_copilot()` against a still-False flag, after which the handler armed it for the whole conference and past its end. Re-checks `_is_meeting_active()` after the relay before arming; test drives the exact interleaving.
  - `[low]` `[patch]` `_compute_meeting_state`'s docstring still opened with "`active` follows `_is_meeting_active`", contradicted three lines below by the new manual `elif`. Leading sentence rewritten to state the real rule.
  - `[low]` `[patch]` `_manual_copilot_active` was the only new shared copilot state accessed outside `self._lock` while every adjacent client-set access takes it, making the omission read as an oversight. Declaration comment now names the asyncio-loop-confinement invariant that makes the lock unnecessary.
  - `[low]` `[patch]` AC 1 was unachievable as literally written — see the Spec Change Log entry above; the criterion was corrected instead of the (verified-correct) code.

Rejected on the authority of the intent itself. The largest cluster (five reviewers' findings) claims the manual mirror has no disarm path, so a supervisor restart resurrects a session: the plan enumerates the flag's mutation set exhaustively ("set on spawn_request relay, cleared on stop_request relay AND on conference `meeting_started` preemption; re-armed from `supervisor_hello.current_meeting_id`"), and a session the user never stopped surviving an infrastructure restart is CAP-3 as specified, not a defect. Findings that the `conference_active` guard and the chat stamp inherit `_is_meeting_active`'s extension-connected requirement propose replacing the very predicate the invocation intent forbids touching. The missing `supervisor_hello.current_meeting_id` producer, the absent supervisor-side handlers for the two relayed frame types, and the `AUTOSPAWN=0` no-op are all story 3 by `stories.yaml`; the manual-state primer for a reconnecting webapp is stories 5/6, and the SPEC constraint forbids the frontend holding manual state. A demand for a success ack echoing `request_id` contradicts plan Task 6, which drives the spawn/stop UI off the `agent_state` chip. Constructing a minimal relay frame instead of forwarding `data` contradicts the I/O Matrix's "forwarded verbatim". The claim that unstamped manual requests escape the `meeting_join` stale loops describes the trade-off the intent deliberately selected (undroppable manual replies). Moving preemption inside `_emit_lifecycle` was refuted by an independent check: `meeting_started` has exactly one emit site.

### 2026-07-24 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 9: (high 1, medium 3, low 5)
- defer: 0
- reject: 6: (high 0, medium 2, low 4)
- addressed_findings:
  - `[high]` `[patch]` Concurrent spawn and stop handlers could deliver `spawn → stop` to the supervisor but finish with `_manual_copilot_active=True`; an asyncio request lock now serializes each relay with its mirror update, and a deterministic suspended-send test proves the final bridge state matches supervisor order.
  - `[medium]` `[patch]` A conference starting while spawn relay awaited produced only a log after the request lost the manual session; the post-relay guard now returns `conference_active` to the originator with the original `request_id`.
  - `[medium]` `[patch]` Stop followed by a new manual spawn could inherit an expired `_sessions_zero_since` anchor; every manual-mirror transition now resets the debounce anchor.
  - `[medium]` `[patch]` New tests exercised private `_process_message` calls rather than the socket/protocol surface named by the intent; real WebSocket tests now register subscriber and supervisor roles through their hello frames, verify full spawn/stop relay order, and bind a rejection to the originating subscriber socket.
  - `[low]` `[patch]` Stop-during-conference behavior was not pinned; coverage now proves the exact stop frame is relayed without a `conference_active` rejection.
  - `[low]` `[patch]` Verbatim relay assertions used only mandatory fields; spawn and stop coverage now includes opaque future-protocol fields at both unit and real-WebSocket boundaries.
  - `[low]` `[patch]` Ordinary conference start had no negative assertion for `manual_preempted`; coverage now proves inactive manual state is a no-op.
  - `[low]` `[patch]` The dedicated preemption fan-out lacked failed-subscriber isolation coverage; a dead subscriber can no longer hide a regression that prevents delivery to a healthy peer.
  - `[low]` `[patch]` The `subscriber_hello` comment still claimed subscribers send no later messages; it now documents manual copilot control frames.

### 2026-07-24 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 3: (high 2, medium 1, low 0)
- defer: 0
- reject: 17: (high 6, medium 7, low 4)
- addressed_findings:
  - `[high]` `[patch]` A `supervisor_hello {current_meeting_id: "manual"}` could re-arm the mirror while a Stop relay awaited I/O, leaving a successfully delivered Stop with `_manual_copilot_active=True`; the Stop handler now clears the mirror again in `finally`, with a deterministic suspended-relay regression test.
  - `[high]` `[patch]` Sequential fan-out could deliver manual spawn to a later supervisor after a conference became active while an earlier supervisor send awaited, allowing the late spawn to displace the conference session; spawn relay now stops before every remaining delivery once `_is_meeting_active()` becomes true, with coverage for the two-supervisor interleaving.
  - `[medium]` `[patch]` The new manual-request `asyncio.Lock` remained bound to a closed event loop when the same `KTalkBridge` object was stopped and started again after contention; `_thread_main` now recreates the lock for each new loop, covered by a real two-loop restart test.

## Design Notes

**Flag is not set when no supervisor is registered.** D5 makes the supervisor the single runtime owner of manual-session truth ("set on spawn_request **relay**"). With no supervisor there is no relay and no tmux session, so arming the flag would make a later `supervisor_hello` reconcile as "no session + active meeting → spawn" and silently start a session the user was already told had failed. Stop is the mirror image and is stated explicitly by the plan: the flag clears even when the relay fails, because there is no session to keep alive for.

**`manual_preempted` goes to subscribers only.** The intent names subscriber clients in all three places it appears (SPEC CAP-4, plan D5, plan Task 2). The extension has no renderer for this status and the backward-compatibility bar forbids introducing a new frame into its stream; the webapp renderer arrives in story 6.

```python
def _copilot_should_be_alive(self) -> bool:
    """A copilot session should exist: conference meeting OR manual session."""
    return self._is_meeting_active() or self._manual_copilot_active
```

## Verification

**Commands:**
- `venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py -v` -- expected: all pass, including the new manual-copilot group
- `venv-gigaam/bin/python -m pytest tests/ -v` -- expected: full suite green
- `venv-gigaam/bin/python -m compileall recog frontend` -- expected: no syntax errors
- `git diff -- ktalk_bridge.py | grep -A20 "def _is_meeting_active"` -- expected: no output (the predicate is untouched)

## Auto Run Result

Status: done

Summary: Follow-up review hardened the bridge's manual lifecycle ordering so conference precedence and a completed Stop remain authoritative across concurrent supervisor I/O, and a reused bridge object receives a fresh request lock for each event loop.

Files changed:
- `recog/ktalk_bridge.py` — stops late spawn fan-out after conference activation, makes Stop win over reconnect re-arm, and recreates the manual request lock on bridge-loop restart.
- `tests/test_ktalk_bridge.py` — adds deterministic regressions for the two relay races and the cross-loop restart failure.
- This story spec — records the review triage, verification, final revision, and terminal result.

Review findings:
- Patches applied: 3 (high 2, medium 1, low 0).
- Deferred: 0; `deferred-work.md` was not modified.
- Rejected: 17 (high 6, medium 7, low 4), primarily explicit later-story surfaces (`recog/copilot_supervisor.py`, frontend, and JS), deliberate contract trade-offs, or unsupported speculative hardening.

Follow-up review recommendation: true — patched findings were high 2, medium 1, low 0; weighted medium/low score `3 × 1 + 1 × 0 = 3`, and the high-severity trigger independently requires a follow-up.

Verification:
- `venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py -v` — 148 passed.
- `venv-gigaam/bin/python -m pytest tests/ -v` — 620 passed, 2 deselected.
- `venv-gigaam/bin/python -m compileall recog frontend` — passed.
- AST-bounded source comparison against `fd174ac:ktalk_bridge.py` — `_is_meeting_active` is byte-identical.
- `git diff --check` — passed.
- Commit `902aa83` contains both reviewed files; working tree is clean.

Residual risks: End-to-end manual copilot behavior still depends on the explicitly separate supervisor/recog/web/UI stories 3–6. No new deferred finding was recorded for this bridge-only story.
