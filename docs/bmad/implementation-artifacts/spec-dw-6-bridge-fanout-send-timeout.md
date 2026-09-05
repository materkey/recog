---
title: 'Bound bridge fan-out sends with a timeout (DW-6)'
type: 'bugfix'
created: '2026-07-25'
status: 'done'
baseline_revision: '4f8b0b81637166eb1d52e8dfb4dae8561dd23efd'
final_revision: '97d363a406b5566f4425dc0dcfd849e2c311b994'
review_loop_iteration: 0
followup_review_recommended: true
context: []
warnings: []
---

<intent-contract>

## Intent

**Problem:** Every fan-out loop in `recog/ktalk_bridge.py` awaits `client.send(message)` sequentially with no timeout, so one socket stalled by WebSocket flow-control backpressure (which blocks without raising) freezes `_process_message` / `_handle_client` for every recipient queued behind it. The recipient set now spans a second process (the frontend launchd service, restarted routinely by `./recog-service.sh restart`), making a stalled socket a realistic event.

**Approach:** Introduce one bounded-send helper (`asyncio.wait_for` with a module-level timeout constant) and route every broadcast loop through it; multi-recipient loops dispatch concurrently via `asyncio.gather` so total fan-out latency is ~one timeout, not N×. A timed-out socket is closed fire-and-forget so its `_handle_client` finally-block reaps it from the client sets.

## Boundaries & Constraints

**Always:**
- Bound and apply concurrent dispatch in ALL seven fan-out sites: `_broadcast_message`, `_broadcast_copilot_frame`, `_broadcast_to_channel_clients`, the supervisor loop inside `_emit_lifecycle`, `_relay_to_supervisors`, `_notify_manual_copilot_preempted`, and the channel-client forward loop in the `copilot_chat_message` handler — except `_relay_to_supervisors`, which stays sequential (see below).
- Preserve each loop's existing error visibility: loops that silently `pass` on send errors keep doing so for non-timeout exceptions; loops that print to stderr keep printing. A timeout always logs one stderr line (it is self-limiting — the stalled client is closed, so it fires once per socket).
- On `asyncio.TimeoutError`, schedule `client.close()` via `create_task` (never await it inline — `close()` on a stalled socket blocks until its own close_timeout). Tolerate clients without a `close` attribute (test fakes) — the helper must not raise.
- `_relay_to_supervisors` keeps its exact semantics: sequential iteration, per-iteration `copilot_spawn_request`-during-active-meeting break check, `delivered` flag. Only the send itself gets bounded; a timeout counts as not delivered.
- Each broadcast call awaits its full gather before returning, preserving frame ordering per client across successive broadcasts.
- Timeout constant is a plain module-level constant (style of `COPILOT_TICK_INTERVAL_SECONDS`), read at call time so tests can monkeypatch it.

**Block If:** Bounding a send site would require changing a frame's protocol semantics (e.g. a caller that depends on a send having fully completed synchronously beyond the delivered-flag contract of `_relay_to_supervisors`).

**Never:**
- Do not touch the `run_coroutine_threadsafe` dispatch paths (`send_transcript`, `_maybe_emit_transcript_tick`, `send_refine`) — they already never block their calling thread.
- Do not bound single-recipient originator replies (`_reply_copilot_status`, per-socket status/history replies) — out of DW-6 scope (fan-out loops only).
- Do not add per-client send queues, background sender tasks, or a retry mechanism.
- Do not edit `_bmad-output/implementation-artifacts/deferred-work.md`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Happy path | All recipients healthy | Every recipient gets the frame; behavior unchanged | No error expected |
| Stalled socket in fan-out | One recipient's `send` never returns; others healthy | Healthy recipients still receive the frame; broadcast returns within ~1 timeout; stalled client's `close()` is scheduled | Timeout logged to stderr |
| Dead socket (send raises) | One recipient raises on `send` | Fan-out continues to remaining recipients (existing behavior preserved) | Per-loop existing handling (silent or stderr) |
| Stalled supervisor in `_relay_to_supervisors` | Sole supervisor's `send` hangs | Returns `False` within ~1 timeout → caller replies `supervisor_disconnected` | Timeout logged to stderr |
| Stalled + healthy supervisor | First supervisor hangs, second healthy | Returns `True`; healthy one received the frame | Timeout logged for the stalled one |
| Client without `close` | Test fake times out | No AttributeError escapes the helper | Timeout still logged |

</intent-contract>

## Code Map

- `recog/ktalk_bridge.py` -- all seven fan-out sites: `_broadcast_message` (~:1102), `_broadcast_copilot_frame` (~:1120), `_broadcast_to_channel_clients` (~:1149), `_emit_lifecycle` supervisor loop (~:1190), `_relay_to_supervisors` (~:1199), `_notify_manual_copilot_preempted` (~:1310), `copilot_chat_message` channel forward (~:2944); module constants live near the top (~:55).
- `tests/test_ktalk_bridge.py` -- existing fakes (`FakeWebSocket`, `ControllableFakeWebSocket`, `RecordingClient`, `ExplodingClient` pattern) and fan-out tests (`test_dead_client_does_not_abort_copilot_fan_out`, extension/subscriber recorder helpers) to extend.

## Tasks & Acceptance

**Execution:**
- `recog/ktalk_bridge.py` -- add `BROADCAST_SEND_TIMEOUT_S = 5.0` near the other module constants, and a `_send_bounded(self, client, message) -> bool` async helper: `asyncio.wait_for(client.send(message), timeout=BROADCAST_SEND_TIMEOUT_S)`; on timeout log to stderr, schedule `close()` if present, return False; let non-timeout exceptions propagate to the caller loop so per-loop handling stays where it is today (or return a sentinel — implementer's choice, as long as per-loop visibility is preserved).
- `recog/ktalk_bridge.py` -- convert the six multi-recipient loops to concurrent dispatch (`asyncio.gather` over per-client coroutines that wrap `_send_bounded` plus the loop's existing exception handling); keep empty-recipient early returns.
- `recog/ktalk_bridge.py` -- in `_relay_to_supervisors`, keep the sequential loop and break check; wrap only the `await client.send(message)` with the bounded helper; set `delivered = True` only on a successful bounded send.
- `tests/test_ktalk_bridge.py` -- add given/when/then tests for the I/O matrix rows: stalled-socket fan-out (monkeypatch `ktalk_bridge.BROADCAST_SEND_TIMEOUT_S` to ~0.1), close-scheduled-on-timeout, stalled supervisor → `False`, stalled+healthy supervisor → `True`, fake without `close`.

**Acceptance Criteria:**
- Given a fan-out with one stalled socket and one healthy recipient, when any of the seven broadcast paths runs, then the healthy recipient receives the frame and the call returns in roughly one timeout (not hang).
- Given a stalled socket that timed out, when the broadcast completes, then a `close()` call has been scheduled on that socket.
- Given the existing test suite, when `python -m pytest tests/test_ktalk_bridge.py` runs, then all pre-existing tests still pass unchanged.

## Spec Change Log

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 7: (high 0, medium 2, low 5)
- defer: 3: (high 0, medium 3, low 0)
- reject: 6: (high 0, medium 0, low 6)
- addressed_findings:
  - `[medium]` `[patch]` Timed-out client was not evicted from the client sets, so every broadcast until the `_handle_client` reap re-paid a full timeout on the same socket — `_send_bounded` now discards the client from all four sets under `self._lock` on timeout; docstring's "once per socket" claim is now guaranteed by the eviction.
  - `[medium]` `[patch]` Stalled-client tests covered only 3 of 7 converted fan-out sites — added stalled-plus-healthy tests for `_broadcast_to_channel_clients`, `_emit_lifecycle`, the `manual_preempted` subscriber fan-out, and the `copilot_chat_message` channel forward.
  - `[low]` `[patch]` Fire-and-forget `create_task(close())` held no strong reference (task could be GC'd before running) — close tasks now kept in `self._bg_close_tasks` with a done-callback discard.
  - `[low]` `[patch]` `_relay_to_supervisors` treated a timeout as definitively "not delivered" though the frame may have reached the transport buffer — docstring now states delivery-unknown / at-most-once semantics.
  - `[low]` `[patch]` Six `asyncio.gather` sites lacked `return_exceptions=True` (a `BaseException` in one dispatch would detach siblings during shutdown) — added to all six.
  - `[low]` `[patch]` No test pinned the gather concurrency (single-stall tests pass even for a sequential bounded loop) — added `test_multiple_stalled_clients_time_out_concurrently` (3 stalled + 1 healthy, elapsed strictly below sequential cost).
  - `[low]` `[patch]` Load-sensitive timing assertions (fixed 0.05 s sleep before `close_calls` check, tight `elapsed < 1.0` bounds) — close check now polls with a 2 s deadline; single-stall bounds relaxed to `< 2.0`.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 4: (high 1, medium 1, low 2)
- defer: 0
- reject: 8: (high 0, medium 0, low 8)
- addressed_findings:
  - `[high]` `[patch]` Immediate timeout eviction removed channel, supervisor, and extension role membership before `_handle_client.finally` could snapshot it, skipping pending-chat cleanup, prompt supervisor-offline recomputation, and stable meeting ownership — `_send_bounded` now records timed-out clients separately and leaves role sets intact until handler cleanup.
  - `[medium]` `[patch]` Timeout tests inserted standalone fakes directly into client sets and could not observe the broken handler-level cleanup — added handler-owned stalled-socket regressions for channel pending requests, supervisor status, and extension meeting lifecycle.
  - `[low]` `[patch]` Overlapping broadcasts could time out the same socket independently, logging and scheduling multiple close tasks — the timed-out marker now makes the first timeout the only one that logs and closes while later sends fail fast.
  - `[low]` `[patch]` A failing fire-and-forget `close()` task produced an unhandled task exception — the close coroutine now retrieves and logs asynchronous failures.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 7: (high 0, medium 5, low 2)
- defer: 0
- reject: 10: (high 0, medium 5, low 5)
- addressed_findings:
  - `[medium]` `[patch]` A send could time out after `_handle_client` had already reaped its socket, re-adding an orphan to `_timed_out_clients` and closing it twice — timeout bookkeeping now marks and closes only clients still present in a bridge role set, with a deterministic race regression.
  - `[medium]` `[patch]` Stalled-client tests awaited hour-long fake sends directly, so a timeout regression would hang verification instead of failing promptly — every bounded-send scenario now has an independent two-second await deadline.
  - `[medium]` `[patch]` Gather concurrency was pinned by a scheduler-sensitive wall-clock threshold — the regression now uses a shared send barrier and proves all stalled sends enter before any is released.
  - `[medium]` `[patch]` `_emit_lifecycle` coverage paired a stalled supervisor only with a healthy extension, leaving the supervisor loop's own stalled-plus-healthy behavior unverified — the test now asserts delivery to a healthy supervisor peer.
  - `[medium]` `[patch]` A sole stalled supervisor was tested only through the private relay return value — an outer protocol regression now proves the originator receives `supervisor_disconnected` with its request id.
  - `[low]` `[patch]` All timeout tests monkeypatched the production default, so the required five-second value could drift unnoticed — added a default-configuration assertion.
  - `[low]` `[patch]` Background close-task retention and completed-task removal were only observed indirectly — the handler-owned cleanup regression now asserts the pending strong reference and its removal after close completes.

## Verification

**Commands:**
- `./venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py -q` -- expected: all pass, including new timeout tests
- `./venv-gigaam/bin/python -m pytest tests/ -q -x --ignore=tests/e2e` -- expected: no regressions elsewhere
- `./venv-gigaam/bin/python -m compileall ktalk_bridge.py` -- expected: clean

## Auto Run Result

Status: done

Summary: Hardened bounded bridge fan-out against a timeout/handler-cleanup race and strengthened verification of concurrency, timeout defaults, close-task lifecycle, supervisor fan-out, and the originator-visible timeout response.

Files changed:
- `recog/ktalk_bridge.py` — records timed-out clients only while they still belong to a bridge role set.
- `tests/test_ktalk_bridge.py` — adds deterministic race, concurrency, cleanup, lifecycle, default-timeout, non-hang, and protocol-surface regressions.

Review findings:
- Patches applied: 7 (high 0, medium 5, low 2).
- Items deferred: 0; `deferred-work.md` was not modified.
- Items rejected: 10 (high 0, medium 5, low 5), including changes that contradicted the required sequential supervisor relay or expanded the intent into new queue/retry/protocol semantics.

Follow-up review recommendation: true. Patched findings were high 0, medium 5, low 2; score = `3 × 5 + 2 = 17`.

Verification:
- `./venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py -q` — 177 passed.
- `./venv-gigaam/bin/python -m pytest tests/ -q -x --ignore=tests/e2e` — 709 passed.
- `./venv-gigaam/bin/python -m compileall ktalk_bridge.py` — passed.
- `git diff --check` — passed.
- Commit `97d363a406b5566f4425dc0dcfd849e2c311b994` contains both reviewed files; the working tree is clean.

Residual risks:
- A timed-out WebSocket send remains delivery-unknown by design; callers treat it as not delivered without retrying.
- Backpressure is exercised with controllable WebSocket fakes; injecting a genuinely flow-controlled network socket remains outside this story's deterministic test surface.
- Residual artifacts: none.
