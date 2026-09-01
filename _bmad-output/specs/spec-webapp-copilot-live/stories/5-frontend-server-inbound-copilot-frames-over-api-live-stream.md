---
title: 'Frontend server — inbound copilot frames over /api/live/stream'
type: 'feature'
created: '2026-07-25'
status: 'done'
baseline_revision: 'BASE_REVISION'
final_revision: '0ff37d4c1ed73dc64d314739a2fa60d363caaa06'
review_loop_iteration: 0
followup_review_recommended: true
context:
  - '{project-root}/docs/plans/20260724-webapp-copilot-live.md'
  - '{project-root}/_bmad-output/specs/spec-webapp-copilot-live/SPEC.md'
warnings: []
---

<intent-contract>

## Intent

**Problem:** The `/api/live/stream` endpoint discards everything the browser sends (`receive_text()` loop, `frontend/server.py:1420-1422`), so webapp copilot frames can never reach the bridge even though `BridgeSubscriber.send` (story 4) is ready; and a browser opening `#/live` mid-session shows a ⚫ chip until the next `agent_state` broadcast because the server keeps no primer.

**Approach:** CAP-5 — add a testable `handle_client_frame(raw, subscriber, reply_cb)` helper with the three-type inbound whitelist that relays accepted frames via `subscriber.send(raw)` and answers `copilot_status {status: "bridge_disconnected", request_id}` to the originating client on failure; cache the last `copilot_status {status: "agent_state"}` seen from the bridge and deliver it right after `init` on each new live-stream connection.

## Boundaries & Constraints

**Always:**
- TDD: failing tests first in the new `tests/test_frontend_copilot_proxy.py`; given–when–then docstrings as in the neighbouring test files.
- The server is a pure relay and holds NO manual-session state (D5) — no flags, no spawn/stop bookkeeping, nothing survives beyond the `agent_state` primer cache.
- `_forward_bridge_message` (`frontend/server.py:453`) stays byte-identical — its broadcast is already unconditional; the primer cache hooks in via a wrapper callback passed to the `BridgeSubscriber` constructor instead.
- Accepted frames are relayed to `subscriber.send` verbatim (the original raw string — no re-serialization, no mutation); validation beyond the type whitelist is bridge-side.
- The failure reply keys off `send()`'s bool contract (story 4); no queueing, no retry.
- Existing `/api/live/stream` init behavior (meeting lookup, watermark seeding, `refine_enabled`) is unchanged; the primer frame comes strictly after the `init` frame.
- A failed reply or primer send (originator already gone) is swallowed with a log — it must not crash the endpoint loop or the helper.

**Block If:**
- Inbound relay or the primer turns out to require a change in `ktalk_bridge.py` or `frontend/bridge_subscriber.py` (it must not: subscriber-inbound frames are already dispatched bridge-side, and `FORWARD_TYPES` already carries `copilot_status` downstream).

**Never:**
- No changes to `ktalk_bridge.py`, `frontend/bridge_subscriber.py`, `recog/copilot_supervisor.py`, or JS — story 6 owns `app.js`/UI.
- No content validation of `request_id` or frame fields beyond the type whitelist — the bridge owns protocol validation (story 2).
- No new frontend module — helpers live in `frontend/server.py` (plan Task 5 file list).

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Chat relay | Fake subscriber `send→True`; raw `copilot_chat_message` | Exactly one `subscriber.send(raw)` with the verbatim string; no reply to originator | No error expected |
| Spawn/stop relay | Same; raw `copilot_spawn_request` / `copilot_stop_request` | Same verbatim relay, no reply | No error expected |
| Non-whitelisted type | Raw `{"type": "transcript", ...}` / unknown type | Dropped with a log; `subscriber.send` NOT called; no reply | Logged |
| Malformed frame | Non-JSON raw, JSON non-dict, or non-string `type` | Dropped with a log; no relay, no reply, no exception | Logged |
| Send failure | Subscriber `send→False`; frame with `request_id: "r1"` (each of the three types) | `reply_cb` receives `copilot_status {status: "bridge_disconnected", request_id: "r1"}` | No error expected |
| No subscriber | `subscriber is None` (lifespan not run / shutdown) | Treated as send failure → same `bridge_disconnected` reply | No error expected |
| Missing request_id | Whitelisted frame without `request_id`; send fails | Reply carries `request_id: null` (echo of absent) | No error expected |
| Dead originator | `reply_cb` raises on the failure reply | Exception swallowed + logged; helper returns normally | Logged |
| Cache update | Bridge frame `copilot_status {status: "agent_state", ...}` passes the wrapper | Raw frame cached as the primer | No error expected |
| Cache not clobbered | Later `copilot_reply` / per-request `copilot_status` / `transcript_refine` frames | Primer cache still holds the last `agent_state` frame | No error expected |
| Cache malformed | Wrapper receives non-JSON raw | Cache unchanged, frame still forwarded to `_forward_bridge_message` | No error expected |
| Primer delivery | Cache set; new WS client connects to `/api/live/stream` | Client receives `init` first, then the cached `agent_state` frame | No error expected |
| Primer empty | Cache empty (no `agent_state` seen yet); new client connects | Only `init` — no extra frame | No error expected |

</intent-contract>

## Code Map

- `frontend/server.py:453-455` -- `_forward_bridge_message`: stays byte-identical; new wrapper `_on_bridge_frame` caches then delegates to it
- `frontend/server.py:477-481` -- `BridgeSubscriber` constructor site: `forward_callback` swaps to the wrapper
- `frontend/server.py:1371-1426` -- `/api/live/stream`: init branches (`:1395`, `:1407`), receive loop to route through `handle_client_frame` (`:1420-1422`)
- `frontend/bridge_subscriber.py:152-186` -- `send(raw) -> bool` best-effort contract this story keys off
- `ktalk_bridge.py:1442-1463` -- `_recompute_agent_state`: producer of the `copilot_status {status: "agent_state"}` chip frame being cached (per-socket primer twin at `:1935-1944`)
- `tests/test_audio_retention.py` / `tests/test_frontend_live_watcher.py:17-20` -- precedent: `sys.path.insert(0, str(PROJECT_ROOT / "frontend"))` then import `server` / helpers directly

## Tasks & Acceptance

**Execution:**
- `tests/test_frontend_copilot_proxy.py` -- create; cover every I/O Matrix row: helper-level tests with a fake subscriber (recorded `send` calls, scripted bool) and recorded `reply_cb`; cache tests via the wrapper; primer tests at the endpoint surface via `starlette.testclient.TestClient(server.app).websocket_connect("/api/live/stream")` without running lifespan, with `server.get_storage` monkeypatched to a no-active-meetings fake -- failing tests first per TDD
- `frontend/server.py` -- add `INBOUND_COPILOT_TYPES = frozenset({"copilot_chat_message", "copilot_spawn_request", "copilot_stop_request"})` and `async def handle_client_frame(raw, subscriber, reply_cb)` implementing the matrix; call it from the `/api/live/stream` receive loop with the module-global `bridge_subscriber` and `websocket.send_text` as `reply_cb`
- `frontend/server.py` -- add the module-level `agent_state` primer cache + `async def _on_bridge_frame(raw)` wrapper (cache, then `await _forward_bridge_message(raw)`); pass the wrapper as `forward_callback`; send the cached frame right after the `init` send in `live_stream` (both branches), guarded so a send failure is swallowed

**Acceptance Criteria:**
- Given the pre-existing test suite, when it runs after the change, then every assertion still holds (no test edits).
- Given a live-stream client that connects while the cache holds an `agent_state` frame and no subscriber is running, when it sends a whitelisted copilot frame, then over the real WS endpoint it observes: `init`, then the primer, then `copilot_status {status: "bridge_disconnected"}` echoing its `request_id`.
- Given the completed change, when `_forward_bridge_message` is diffed against the baseline revision, then its body is unchanged.

## Spec Change Log

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 5: (high 0, medium 2, low 3)
- defer: 1: (high 1, medium 0, low 0)
- reject: 10: (high 0, medium 3, low 7)
- addressed_findings:
  - `[medium]` `[patch]` The endpoint success path was never proven through the real wiring: every endpoint test ran with `bridge_subscriber is None` and asserted `bridge_disconnected`, so a lifespan-global wiring regression (e.g. dropping `bridge_subscriber` from the `global` statement) kept the suite green while production would permanently answer `bridge_disconnected` (mutation-demonstrated by two independent reviewers). Added `test_endpoint_relays_to_live_subscriber`: a scripted fake subscriber injected as the module global, verbatim relay of the first frame asserted with no reply, and the `bridge_disconnected` echo for a second deterministic-fail frame proving in-order processing.
  - `[medium]` `[patch]` The primer send in the active-meeting `init` branch was never executed by any test (all endpoint fakes returned zero active meetings) — deleting that line kept all tests green while the motivating mid-meeting scenario lost the primer. Added `test_primer_delivered_after_active_meeting_init` driving the `if current:` branch through the real `live_watcher` methods.
  - `[low]` `[patch]` `handle_client_frame` — the one new public entry point — had zero type annotations against the repo's "type hints for new code" convention. Annotated (`raw: str`, `subscriber: Optional[BridgeSubscriber]`, `reply_cb: Callable[[str], Awaitable[None]]`).
  - `[low]` `[patch]` `test_dead_originator_reply_exception_swallowed` had no assertions and passed even if the helper returned early without attempting relay or reply. Now asserts the relay happened and exactly one reply attempt was made.
  - `[low]` `[patch]` The init-exception branch fell through to the receive loop without the agent-state primer, leaving a client that hit a transient storage error chip-blind until the next broadcast. Primer now sent after the `error` frame, covered by `test_primer_delivered_after_init_error`.

Deferred (pre-existing, not caused by this story): localhost WebSocket servers accept connections from any browser page (no Origin check), so a malicious webpage could drive copilot frames — but the bridge ports (8765/8766) already expose the identical unauthenticated surface to any local WS client by design; this story's endpoint adds no marginal exposure. Rejected on the authority of the intent itself: content/size/rate validation beyond the type whitelist (the intent places protocol validation bridge-side; the server is a pure relay), the primer cache lacking invalidation on bridge death (the intent defines the cache without invalidation, plan Task 5's file list forbids the `bridge_subscriber` hook it would need, and the bridge re-primers the subscriber socket on reconnect — self-healing), the `send()` written-to-transport false-success window (story 4's litigated contract: delivery confirmation is app-level), cross-tab reply fan-out and `request_id` collisions (documented v1 design: duplicates cosmetic, `crypto.randomUUID()` per plan), missing try/except around `subscriber.send` (story 4's spec guarantees no-raise, tested there), unthrottled `print` drop-logs (the file's uniform idiom), `json.loads` on the hot forward path (negligible vs the WS fan-out it precedes), the init-failure path entering the routing loop (baseline-identical structure, init is not auth), duplicate `agent_state` during the register-to-primer window (state frames are idempotent for a chip renderer), and binary frames killing the connection via `receive_text` (baseline-identical, self-inflicted by the sender, browser reconnects).

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 3: (high 0, medium 1, low 2)
- defer: 0
- reject: 9: (high 0, medium 4, low 5)
- addressed_findings:
  - `[medium]` `[patch]` The production `lifespan → BridgeSubscriber(forward_callback=_on_bridge_frame)` adoption was not covered: direct wrapper tests and manually seeded endpoint tests stayed green if the constructor was wired back to `_forward_bridge_message`. Added a lifespan composition test with stubbed I/O that drives the captured subscriber callback and observes the primer through the real `/api/live/stream` endpoint.
  - `[low]` `[patch]` The init-exception branch sent a primer after an `error` frame even though the intent requires the primer strictly after `init` and preserves existing init behavior. Removed the extra error-path primer and its contrary test.
  - `[low]` `[patch]` The required best-effort primer-send guard had no mutation-proof test. Added a dead-WebSocket test that proves one send attempt and no escaping exception.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 4: (high 0, medium 1, low 3)
- defer: 0
- reject: 11: (high 1, medium 8, low 2)
- addressed_findings:
  - `[medium]` `[patch]` The upstream half of the production composition was still unverified: the endpoint relay test injected `bridge_subscriber` directly, while the lifespan test exercised only the subscriber callback. Extended the lifespan test so the exact `BridgeSubscriber` instance created by `lifespan()` records two endpoint-originated frames and the second deterministic failure proves in-order processing through the real `/api/live/stream` route.
  - `[low]` `[patch]` The lifespan test left its fake `observer`, fake `bridge_subscriber`, and closed asyncio loops in module globals after completion. The autouse fixture now snapshots and restores all four globals around every test.
  - `[low]` `[patch]` A deeply nested JSON client frame could raise `RecursionError` out of `handle_client_frame` instead of following the malformed-frame no-exception contract. The helper now drops that parser failure and the malformed-frame matrix includes a recursion-depth case.
  - `[low]` `[patch]` The lifespan comment described the bridge subscriber as refine-only after it became bidirectional. Updated it to document downstream live-stream frames and upstream Copilot requests without changing behavior.

## Design Notes

**Why a wrapper instead of editing `_forward_bridge_message`.** The intent states "No `_forward_bridge_message` change needed" — its unconditional broadcast already covers bridge→webapp fan-out. The primer cache is the only new bridge-frame concern, and it composes in front:

```python
async def _on_bridge_frame(raw: str) -> None:
    _cache_agent_state_frame(raw)   # remembers only copilot_status/agent_state
    await _forward_bridge_message(raw)
```

**Endpoint tests and lifespan.** Most `TestClient.websocket_connect` tests run without lifespan — no `Observer`, no real `BridgeSubscriber`, and `bridge_subscriber` stays `None` (which is exactly the no-subscriber matrix row). The production composition test enters `server.lifespan` with stubbed `Observer` and `BridgeSubscriber`, drives the captured callback, observes the cached primer through the real endpoint, and sends client frames back through the exact lifespan-created subscriber. Monkeypatch `server.get_storage` so `list_active_meetings()` returns `[]` and the init branch never touches the real `meetings/` dir. Reset and restore the module globals mutated by endpoint/lifespan tests, since `server` is imported once per session.

## Verification

**Commands:**
- `venv-gigaam/bin/python -m pytest tests/test_frontend_copilot_proxy.py -v` -- expected: all pass
- `venv-gigaam/bin/python -m pytest tests/ -v` -- expected: full suite green
- `venv-gigaam/bin/python -m compileall recog frontend` -- expected: no syntax errors
- `git diff <baseline> -- frontend/server.py` -- expected: `_forward_bridge_message` body unchanged; no edits outside `frontend/server.py` and the new test file

## Auto Run Result

Status: done

Summary: Re-reviewed and hardened the frontend Copilot WebSocket proxy. The lifespan composition test now proves both downstream primer delivery and upstream verbatim relay through the exact subscriber instance created by production lifespan wiring; malformed frames that exceed JSON recursion depth are dropped safely; test-mutated lifecycle globals are restored; and the subscriber lifecycle comment reflects its bidirectional role.

Files changed:
- `frontend/server.py` — handle JSON recursion failures as malformed input and clarify the bridge subscriber's bidirectional lifecycle role.
- `tests/test_frontend_copilot_proxy.py` — compose upstream and downstream traffic through the lifespan-created subscriber, restore mutated globals, and cover parser recursion depth.

Review findings:
- Patches applied: 4 (high 0, medium 1, low 3).
- Items deferred: 0. The existing deferred-work ledger was left byte-for-byte unchanged.
- Items rejected: 11 (high 1, medium 8, low 2) because they duplicated existing deferred work, contradicted the binding intent, relied on guarantees owned by story 4, or were not sufficiently concrete.

Follow-up review recommendation: true. Patched finding score: `3 × 1 medium + 1 × 3 low = 6`; high patches: 0.

Verification:
- `venv-gigaam/bin/python -m pytest tests/test_frontend_copilot_proxy.py -v` — 29 passed.
- `venv-gigaam/bin/python -m pytest tests/ -v` — 680 passed, 2 deselected.
- `venv-gigaam/bin/python -m compileall recog frontend` — passed.
- `git diff --check` — passed.
- Baseline/current extraction of `_forward_bridge_message` compared byte-identically.
- Commit `0ff37d4c1ed73dc64d314739a2fa60d363caaa06` contains both files in the reviewed diff.

Residual risks: The pre-existing localhost WebSocket Origin exposure remains tracked by the orchestrator-owned deferred-work ledger. No residual worktree artifacts remain.
