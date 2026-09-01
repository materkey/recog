---
title: 'BridgeSubscriber becomes bidirectional'
type: 'feature'
created: '2026-07-25'
status: 'done'
baseline_revision: 'BASE_REVISION'
final_revision: '4344e4a'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/docs/plans/20260724-webapp-copilot-live.md'
  - '{project-root}/_bmad-output/specs/spec-webapp-copilot-live/SPEC.md'
warnings: []
---

<intent-contract>

## Intent

**Problem:** `frontend/bridge_subscriber.py` is receive-only and forwards a single frame type: the bridge's copilot fan-out (story 1) now reaches the subscriber socket, but `_handle_message` drops everything except `transcript_refine`, and there is no way to push webapp copilot frames (`copilot_chat_message` / `copilot_spawn_request` / `copilot_stop_request`) up the already-open bridge connection.

**Approach:** Widen the forward filter to a `FORWARD_TYPES` frozenset `{transcript_refine, copilot_reply, copilot_status}`, hold the live websocket in an attribute, and add best-effort `async def send(self, raw: str) -> bool` that fails fast (returns `False`) whenever the bridge link is down — no queueing across reconnects.

## Boundaries & Constraints

**Always:**
- TDD: failing tests first in `tests/test_bridge_subscriber.py`; given–when–then docstrings as used throughout that file.
- `send` is best-effort: no buffering, no retry, no waiting for reconnect — a frame submitted while disconnected returns `False` immediately (SPEC constraint: a chat message typed while the bridge is down fails fast).
- `send` returns `True` only after the socket send was awaited without exception; every failure path (never connected, backoff window, socket dies mid-send) returns `False` and must not raise into the caller.
- A `send` failure must never crash or perturb the run loop's reconnect cycle.
- The ws attribute is cleared whenever the connection is lost — before the backoff sleep starts — so `send` fails fast during the whole disconnected window.
- Existing `transcript_refine` forwarding behavior stays byte-identical; frames outside `FORWARD_TYPES` are still dropped; malformed-frame handling is unchanged.

**Block If:**
- Outbound send turns out to require a change in `ktalk_bridge.py` (it must not: subscriber-inbound frames are already dispatched through `_process_message`, `ktalk_bridge.py:1980`).

**Never:**
- No changes to `ktalk_bridge.py`, `frontend/server.py`, `recog/copilot_supervisor.py`, or JS — stories 5–6 own the callers of `send`.
- No typed-exception API for `send` — the contract is the bool return; story 5's `bridge_disconnected` reply keys off it.
- Do not alter the reconnect/backoff logic, `subscriber_hello` behavior, or `stop()` semantics.
- Do not trigger a reconnect from inside `send`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| copilot_reply forward | Connected; bridge sends `copilot_reply {...}` | Raw frame passed to `forward_callback` | No error expected |
| copilot_status forward | Connected; bridge sends `copilot_status {...}` (any variant) | Raw frame passed to `forward_callback` | No error expected |
| Refine unchanged | Connected; bridge sends `transcript_refine` | Forwarded exactly as before | No error expected |
| Other types dropped | Bridge sends `transcript` / unknown type | Not forwarded | No error expected |
| Send connected | Live connection; `await send(raw)` | Bridge socket receives `raw` verbatim; returns `True` | No error expected |
| Send never-connected | `send` before `start()` or while the bridge URL is dead | Returns `False` immediately, no exception | No error expected |
| Send during backoff | Bridge dropped, run loop waiting to reconnect; `send(raw)` | Returns `False`; after reconnect the frame is **never** delivered (no queueing) | No error expected |
| Send mid-death race | ws attribute still set but socket just closed; `send(raw)` | Internal exception caught → `False`; run loop keeps reconnecting normally | Logged to stderr |

</intent-contract>

## Code Map

- `frontend/bridge_subscriber.py:167` -- the single-type `transcript_refine` filter to replace with `FORWARD_TYPES`
- `frontend/bridge_subscriber.py:87-124` -- `_run`: connect block (`async with websockets.connect`) where the ws attribute is set on entry and must be cleared on exit/failure
- `frontend/bridge_subscriber.py:1-31` -- module + class docstrings describing the receive-only single-type role; must be updated
- `frontend/server.py:453-481` -- sole constructor site (`forward_callback=_forward_bridge_message`); unchanged here, story 5 becomes the `send` caller
- `ktalk_bridge.py:1956-1980` -- bridge side: `subscriber_hello` registration and `_process_message` dispatch of subscriber-inbound frames (proof no bridge change is needed)
- `tests/test_bridge_subscriber.py:16` -- `_start_mock_bridge` helper; `:147` shows the handler-side `recv` pattern for asserting frames the subscriber sends

## Tasks & Acceptance

**Execution:**
- `tests/test_bridge_subscriber.py` -- add tests covering every I/O Matrix row (reuse `_start_mock_bridge`; for send-delivery, have the mock handler `recv` past `subscriber_hello` and record the next frame; for the no-queueing row, drop the bridge, `send` → `False`, restore the bridge, assert the frame never arrives) -- failing tests first per TDD
- `frontend/bridge_subscriber.py` -- add class attribute `FORWARD_TYPES = frozenset({"transcript_refine", "copilot_reply", "copilot_status"})` and switch the `_handle_message` filter to membership -- one declaration site so story 5 needs no further widening
- `frontend/bridge_subscriber.py` -- hold the current connection in an attribute (set right after `websockets.connect` succeeds, cleared in a `finally` covering the recv loop) and add `async def send(self, raw: str) -> bool` per the matrix -- the attribute-clear placement is what guarantees fail-fast during backoff
- `frontend/bridge_subscriber.py` -- update the module and class docstrings: the subscriber now relays copilot frames downstream and carries webapp copilot frames upstream

**Acceptance Criteria:**
- Given the pre-existing tests in `tests/test_bridge_subscriber.py`, when the suite runs after the change, then every pre-existing assertion holds unchanged.
- Given a `send` that returned `False` while the bridge was down, when the subscriber reconnects, then that frame is never delivered to the bridge (no queueing across reconnects).
- Given the full test suite, when it runs, then it is green (`tests/` — no regression in bridge or supervisor groups).

## Spec Change Log

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 6: (high 0, medium 1, low 5)
- defer: 0
- reject: 10: (high 0, medium 3, low 7)
- addressed_findings:
  - `[medium]` `[patch]` `send()`'s exception path was never exercised with a live run loop: `test_send_mid_death_race_returns_false` never calls `start()`, so the matrix row's "run loop keeps reconnecting normally" half was unpinned — a mutation adding `self._task.cancel()` to the except branch survived the whole suite (independently demonstrated by the verification-gap reviewer). Added `test_send_failure_does_not_perturb_live_run_loop`: live mock bridge, `sub._ws` shadowed with a raising stub, `send → False`, then the load-bearing assertion that a subsequent `transcript_refine` on the same run loop is still forwarded. Mutation re-applied → new test fails; reverted with zero residue.
  - `[low]` `[patch]` `self._ws` was published before the `subscriber_hello` send, so a concurrently awaited `send()` could emit a copilot frame as the connection's first frame while the bridge still classified the socket as an extension client (flagged independently by three reviewers). Assignment moved after the hello try/except, still inside the inner try so the `finally` clears it.
  - `[low]` `[patch]` The comment justifying `send()`'s local copy claimed it prevents a None deref — a race that cannot happen (no await point between check and use in single-threaded asyncio). Rewritten to name the actual model: the only real race is the connection dying around `await ws.send()`, handled by the except path.
  - `[low]` `[patch]` `send()`'s docstring "True only after the socket send completed" overstated the contract — completion means written to the transport, not delivered. Clause added: delivery confirmation only comes via app-level replies.
  - `[low]` `[patch]` The no-queueing guarantee was proven by a fixed 0.3 s sleep (a late-flush queueing bug would pass forever). Replaced with an ordering proof: after reconnect, `send(sentinel)` → `received_frames == [sentinel]`.
  - `[low]` `[patch]` `not in self.FORWARD_TYPES` raised `TypeError` on an unhashable `type` value (e.g. `{"type": []}`), escaping `_handle_message` and dropping the live connection until reconnect — the replaced `!=` comparison was total. Filter now requires `isinstance(msg_type, str)` first; new test asserts the frame is dropped and the connection survives (`connection_count == 1`). TDD-red confirmed the exact `TypeError` before the fix.

Rejected on the authority of the intent itself. The unenforced upstream frame whitelist in `send()` is story 5's surface — SPEC CAP-5 places the three-type inbound whitelist in the frontend server's `handle_client_frame`, and D1 makes the subscriber a raw relay. Per-request `copilot_reply`/`copilot_status` fanning out to every webapp client is the plan's documented v1 design ("pending state is per-author via `request_id`, duplicates are cosmetic"; story 6 renders unknown-request_id replies deliberately). Remaining rejects were unreachable-in-production (a `send()` timeout for half-open TCP on a loopback-only bridge URL), bounded-by-design (unthrottled send-failure logging — the disconnected window takes the silent `_ws is None` fast path, so volume is capped by actual mid-send socket deaths), test-shape preferences consistent with the file's pre-existing conventions (a public `connected` property instead of `_ws` pokes, `Optional[Any]` typing, migrating old poll loops to `_wait_for`, a mock-handler assert whose failure still fails the test via timeout), duplicate path coverage (send-after-stop is the already-tested `_ws is None` path), and established run practice (plan Task-4 checkboxes stay unchecked — stories 1–3 did the same; plan bookkeeping is story 7's docs task).

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 1: (high 0, medium 1, low 0)
- defer: 0
- reject: 10: (high 0, medium 3, low 7)
- addressed_findings:
  - `[medium]` `[patch]` The load-bearing `subscriber_hello`-before-`_ws` publication order was implemented but not deterministically verified: existing real-socket tests had no controlled send interleaving, so moving `self._ws = ws` above the hello await survived. Added `test_send_waits_for_subscriber_hello_before_connection_is_published` with a blocked hello send; it proves an early `send()` returns `False`, then proves hello remains first and later sends succeed. The ordering mutation made the new test fail (`True is False`); restoring production order returned the suite to green with zero mutation residue.

## Design Notes

`send` reads the ws attribute once into a local before using it — the run loop clears the attribute concurrently on disconnect, and the local copy makes the race window collapse into the mid-death row (exception → `False`) instead of an `AttributeError`/`None` deref:

```python
async def send(self, raw: str) -> bool:
    ws = self._ws
    if ws is None:
        return False
    try:
        await ws.send(raw)
        return True
    except Exception as exc:
        # log + fall through
        return False
```

The `websockets` client supports one concurrent `recv` loop plus `send` calls from other tasks, so no extra locking is needed around the shared connection.

## Verification

**Commands:**
- `venv-gigaam/bin/python -m pytest tests/test_bridge_subscriber.py -v` -- expected: all pass, including the new bidirectional group
- `venv-gigaam/bin/python -m pytest tests/ -v` -- expected: full suite green
- `venv-gigaam/bin/python -m compileall recog frontend` -- expected: no syntax errors

## Auto Run Result

Status: done

Summary: Completed a fresh baseline-to-HEAD review of the bidirectional bridge subscriber and hardened the load-bearing `subscriber_hello` ordering with a deterministic concurrency regression test.

Files changed:
- `tests/test_bridge_subscriber.py` — added controlled coverage proving `_ws` is not published until `subscriber_hello` completes and later frames preserve protocol order.

Review findings:
- Patches applied: 1 (high 0, medium 1, low 0).
- Items deferred: 0; `deferred-work.md` was not modified.
- Items rejected: 10 (high 0, medium 3, low 7), covering intent-excluded raw-relay validation, pre-existing/unchanged hello behavior, redundant compositional lifecycle coverage, and non-actionable loopback-only transport concerns.

Follow-up review recommendation: false. Patched finding score = `3 × 1 medium + 1 × 0 low = 3`; no high-severity patch.

Verification performed:
- `venv-gigaam/bin/python -m pytest tests/test_bridge_subscriber.py -v` — 15 passed.
- `venv-gigaam/bin/python -m pytest tests/ -q` — 651 passed, 2 deselected.
- `venv-gigaam/bin/python -m compileall recog frontend` — passed.
- `git diff --check` — passed.
- Mutation check: moving `_ws` publication before the blocked hello made the new test fail with `True is False`; restoring the intended order returned the suite to green, and `git diff --exit-code HEAD -- frontend/bridge_subscriber.py` confirmed zero mutation residue.
- Reviewed diff coverage: `git diff --name-only BASE_REVISION..4344e4a` contains both `frontend/bridge_subscriber.py` and `tests/test_bridge_subscriber.py`.

Residual risks: none within this story's intent contract. Caller-side raw-frame validation remains owned by the later frontend-server story and was not deferred.

Residual artifacts: none; `git status --porcelain --untracked-files=all` is clean.
