---
title: 'Bridge broadcasts copilot frames to subscriber clients'
type: 'feature'
created: '2026-07-24'
status: 'done'
baseline_revision: 'f01624c'
final_revision: '4a21d71'
review_loop_iteration: 0
followup_review_recommended: false
context:
  - '{project-root}/docs/plans/20260724-webapp-copilot-live.md'
  - '{project-root}/_bmad-output/specs/spec-webapp-copilot-live/SPEC.md'
warnings: []
---

<intent-contract>

## Intent

**Problem:** Every copilot frame the bridge broadcasts (`copilot_reply`, and all `copilot_status` variants — per-request, `agent_state` chip, `channel_connected`, `channel_disconnected`) goes out via `_broadcast_message`, which targets only `_connected_clients` (Chrome extension). `_subscriber_clients` (the frontend `BridgeSubscriber`) never receive them, so the webapp can never render copilot replies or status.

**Approach:** Add a `_broadcast_copilot_frame` helper that fans out to `_connected_clients` **plus** `_subscriber_clients` (snapshot under `self._lock`, per-client errors swallowed) and route every copilot broadcast site through it. `_broadcast_message` itself stays extension-only so no non-copilot frame gains the wider fan-out.

## Boundaries & Constraints

**Always:**
- TDD: write the failing tests in `tests/test_ktalk_bridge.py` first, then the code.
- Test names/docstrings follow the given–when–then convention already used in that file.
- Route **all** copilot broadcast sites — selection is by frame type, not a hand-picked subset (a partial patch leaves the webapp blind to channel-daemon death mid-session).
- Snapshot recipient sets under `self._lock` before iterating (`send_refine`, `ktalk_bridge.py:1007-1009`, is the reference pattern) — `_handle_client` mutates them concurrently.
- Extension clients keep receiving every frame they receive today, byte-identical.
- Per-client send failures are swallowed (one dead socket must not abort the fan-out).

**Block If:**
- A copilot broadcast site cannot be routed through the helper without changing an emitted frame's shape or a caller's lifecycle semantics.

**Never:**
- Do not add `_subscriber_clients` to `_broadcast_message` itself — that would leak transcript/lifecycle/meeting frames to subscribers.
- No manual spawn/stop frames, no `_manual_copilot_active`, no `_compute_meeting_state` change (story 2).
- No changes to `recog/web/bridge_subscriber.py`, `recog/copilot_supervisor.py`, or any JS.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Reply fan-out | 1 extension + 1 subscriber registered; `copilot_channel_reply` processed | Both receive the same `copilot_reply` frame | No error expected |
| Per-request status | Same clients; `copilot_channel_status` processed | Both receive `copilot_status {request_id, status}` | No error expected |
| Agent-state chip | Same clients; `_recompute_agent_state` flips the effective state | Both receive `copilot_status {status: "agent_state"}` | No error expected |
| Channel connected | Same clients; first channel client sends `copilot_channel_hello` (0→1) | Both receive `copilot_status {status: "channel_connected"}` | No error expected |
| Channel disconnected | Same clients; last channel client's socket closes (1→0) | Both receive `copilot_status {status: "channel_disconnected"}` | No error expected |
| Webapp-only | Subscriber registered, **no** extension clients | Subscriber still receives the copilot frame | Must not early-return on empty `_connected_clients` |
| Dead subscriber | Subscriber whose `send` raises | Extension still receives the frame | Exception swallowed, fan-out continues |
| Non-copilot frame | 1 extension + 1 subscriber; `transcript_history` broadcast | Only the extension receives it | No error expected |

</intent-contract>

## Code Map

- `ktalk_bridge.py:1075` -- `_broadcast_message`: extension-only fan-out; stays unchanged (early-returns when `_connected_clients` is empty)
- `ktalk_bridge.py:1007-1017` -- `send_refine`: lock-snapshot + dual-set fan-out pattern to mirror
- `ktalk_bridge.py:322` -- `_subscriber_clients` set (populated by `subscriber_hello`, `:1772`)
- `ktalk_bridge.py:1264` -- copilot site 1: `agent_state` chip in `_recompute_agent_state`
- `ktalk_bridge.py:1847` -- copilot site 2: `channel_disconnected` on last channel-client departure (in `_handle_client` finally-block)
- `ktalk_bridge.py:2619` -- copilot site 3: `channel_connected` on first channel client
- `ktalk_bridge.py:2780` -- copilot site 4: `copilot_reply` from `copilot_channel_reply`
- `ktalk_bridge.py:2825` -- copilot site 5: per-request `copilot_status` from `copilot_channel_status`
- `tests/test_ktalk_bridge.py:646` -- `RecordingClient` (async `send` recorder); `:76` `FakeWebSocket`; `:3260` `_bridge_with_active_ktalk`; `:3269` `_agent_states`

## Tasks & Acceptance

**Execution:**
- `tests/test_ktalk_bridge.py` -- add a copilot fan-out test group covering all eight I/O Matrix rows (reuse `RecordingClient`, `_bridge_with_active_ktalk`, `_agent_states`) -- failing tests come first per TDD
- `recog/ktalk_bridge.py` -- add `async def _broadcast_copilot_frame(self, data: dict)` next to `_broadcast_message`: snapshot `list(self._connected_clients) + list(self._subscriber_clients)` under `self._lock`, serialize once with `ensure_ascii=False`, `await client.send(...)` per recipient with per-client exception swallowing, and no early-return when the extension set is empty -- one helper so a future copilot site cannot silently skip subscribers
- `recog/ktalk_bridge.py` -- switch the five copilot sites (`:1264`, `:1847`, `:2619`, `:2780`, `:2825`) from `_broadcast_message` to `_broadcast_copilot_frame` -- frame payloads unchanged
- `recog/ktalk_bridge.py` -- verify no remaining `_broadcast_message` call site emits a `copilot_*` frame, and that non-copilot sites are untouched

**Acceptance Criteria:**
- Given a bridge with one extension client and one subscriber client, when any of the five copilot sites broadcasts, then both clients receive the identical frame and the extension's delivery is unchanged from before.
- Given a bridge with a subscriber but no extension clients, when a copilot frame is broadcast, then the subscriber receives it.
- Given a subscriber whose `send` raises, when a copilot frame is broadcast, then the extension still receives it and no exception escapes the broadcast call.
- Given a bridge with one extension and one subscriber, when a non-copilot frame (`transcript_history`) is broadcast, then the subscriber receives nothing.
- Given the full test suite, when it runs, then it is green (no regression in extension-facing copilot tests).

## Spec Change Log

## Review Triage Log

### 2026-07-24 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 2: (high 0, medium 1, low 1)
- defer: 2: (high 0, medium 1, low 1)
- reject: 12: (high 0, medium 3, low 9)
- addressed_findings:
  - `[medium]` `[patch]` `test_dead_subscriber_does_not_abort_copilot_fan_out` could not fail for the property it named — the `ExplodingClient` sat in `_subscriber_clients`, which the helper always iterates last, so a fan-out mutated to abort on first failure stayed green (mutation-proven by two independent reviewers). Rewritten as `test_dead_client_does_not_abort_copilot_fan_out`: the dead socket moved into `_connected_clients` with live recorders in both sets, so a live subscriber is guaranteed to be reached after it; both recorders are now asserted. Mutation re-verified independently (hoisting the `try` out of the loop now FAILS the test; mutation reverted, no residue).
  - `[low]` `[patch]` Docstring of the pre-existing `test_copilot_channel_reply_does_not_loopback_to_channel_clients` (`tests/test_ktalk_bridge.py:1457`) still attributed loopback prevention to `_broadcast_message` iterating only `_connected_clients` — a mechanism this change moved off that path. Docstring updated to name `_broadcast_copilot_frame`; assertions untouched.

Rejected as out of this story's scope on the authority of the intent itself (`stories.yaml` assigns these surfaces to stories 4/5/6): the `BridgeSubscriber` type filter still dropping copilot frames, the absent frontend-server relay and SPA renderer, and the missing late-joiner reply primer. Other rejects were unreachable-in-production (dual-hello loopback, list-concat duplicate delivery — no real client sends two hellos), pre-existing patterns identical to `_broadcast_message` (unreaped failed clients), latent-only and documented (non-reentrant `self._lock` — all five call sites verified lock-free), shutdown-path-only with nil consequence (`CancelledError` escaping the finally-block fan-out, where teardown is already known not to run under launchd restart), or design choices the intent explicitly selected (helper-based routing rather than type dispatch inside one function; test precedent for direct `_subscriber_clients` injection already exists at `test_send_refine_broadcasts_to_subscribers`).

### 2026-07-24 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 2: (high 0, medium 0, low 2)
- defer: 0
- reject: 12: (high 0, medium 3, low 9)
- addressed_findings:
  - `[low]` `[patch]` The updated `_broadcast_message` docstring stated that subscribers must not receive transcript frames, contradicting the existing dedicated `send_refine` subscriber path. Clarified that only ordinary transcript/lifecycle/meeting frames stay extension-only and named `transcript_refine` as the explicit dedicated-path exception.
  - `[low]` `[patch]` `test_channel_disconnected_fans_out_to_extension_and_subscriber` asserted successful channel registration before entering cleanup, so an early failure could leave its handler task alive. Wrapped registration, close, and assertions in `try/finally`, with an idempotent close and awaited cleanup for unfinished tasks.

## Design Notes

`_broadcast_message` early-returns on `if not self._connected_clients`. The helper must **not** copy that guard: the primary CAP-1 scenario (webapp open, no conference tab) has zero extension clients, and an early return there would drop every copilot frame.

```python
async def _broadcast_copilot_frame(self, data: dict):
    """Fan a copilot frame out to extension AND subscriber clients."""
    with self._lock:
        recipients = list(self._connected_clients) + list(self._subscriber_clients)
    if not recipients:
        return
    message = json.dumps(data, ensure_ascii=False)
    for client in recipients:
        try:
            await client.send(message)
        except Exception:
            pass
```

The two per-socket copilot sends that already target one originator (`_handle_client` primers `:1728`/`:1747`, `channel_disconnected` chat reply `:2666`) are not broadcast sites and stay as-is — a subscriber that is the originator already receives them.

## Verification

**Commands:**
- `venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py -v` -- expected: all pass, including the new fan-out group
- `venv-gigaam/bin/python -m pytest tests/ -v` -- expected: full suite green
- `venv-gigaam/bin/python -m compileall recog frontend` -- expected: no syntax errors
- `grep -n "_broadcast_message" ktalk_bridge.py` -- expected: no remaining site emits a `copilot_reply` / `copilot_status` frame

## Auto Run Result

Status: done

### Summary

Fresh baseline-to-HEAD review confirmed the five copilot broadcast sites still fan out through `_broadcast_copilot_frame` while `_broadcast_message` remains extension-only. The pass clarified the documented transcript exception and hardened disconnect-test cleanup.

### Files Changed

- `recog/ktalk_bridge.py` — documents `send_refine` as the explicit subscriber-facing transcript exception.
- `tests/test_ktalk_bridge.py` — guarantees the channel-disconnect fan-out test closes and awaits its handler task on every failure path.

### Review Findings

- Patches applied: 2 (high 0, medium 0, low 2).
- Deferred: 0; the deferred-work ledger was not modified.
- Rejected: 12 (high 0, medium 3, low 9), primarily explicitly out-of-scope downstream webapp surfaces or hypothetical changes outside the selected fan-out contract.

Follow-up review recommendation: false. Patch score: `3 × 0 medium + 1 × 2 low = 2`.

### Verification

- `venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py -v` — 122 passed.
- `venv-gigaam/bin/python -m pytest tests/ -v` — 594 passed, 2 deselected.
- `venv-gigaam/bin/python -m compileall recog frontend` — passed.
- `_broadcast_message` call-site inspection plus a multiline search for copilot payloads — no remaining `copilot_reply` or `copilot_status` emission through the extension-only helper.
- Commit coverage — `4a21d71` contains both files from the reviewed diff.

### Residual Risks

No residual risk remains inside this story's bridge-only intent. End-to-end webapp forwarding and rendering remain deliberately excluded by the intent and belong to later stories in the epic.

Residual artifacts: none in `git status --porcelain`.
