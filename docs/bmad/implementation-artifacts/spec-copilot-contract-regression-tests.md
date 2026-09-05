---
title: 'Copilot contract regression tests (DW-11, DW-14, DW-16, DW-17)'
type: 'chore'
created: '2026-07-25'
status: 'done'
baseline_revision: 'BASE_REVISION'
final_revision: '10a9f57052cda38771fcd1d99800314bc304572b'
review_loop_iteration: 0
followup_review_recommended: false
context: []
warnings: [oversized, multiple-goals]
---

<intent-contract>

## Intent

**Problem:** Four copilot protocol contracts currently hold only by absence of code or by coincidentally-matching literals: (DW-11) the `supervisor_hello.current_meeting_id` key name is pinned independently on each side of the wire, so a one-sided rename passes the suite while silently breaking manual-session recovery; (DW-14) "F5 of the webapp tab does not kill the manual session" holds only because no disconnect-path code sends a stop frame — no test pins it; (DW-16) the originator-only no-channel `channel_disconnected` echo has no negative-recipient assertion, so an accidental widening to broadcast stays green; (DW-17) the manual-session contract that proactive `transcript_tick`s stay disabled has no lifecycle regression test.

**Approach:** Add four regression tests only — no production-code changes. Cross-module: capture the supervisor's actually-produced `supervisor_hello` frame off a real WS and feed it into the bridge's `_process_message` (DW-11). Endpoint-level: prove a disconnected `/api/live/stream` client emits nothing to the bridge subscriber (DW-14). Negative-recipient: unrelated clients receive nothing when the originator gets the no-channel echo (DW-16). Manual-lifecycle: both manual paths (spawn request, `supervisor_hello` re-arm) leave `_copilot_tick_state` as `None` and a segment flood past the count trigger emits no `transcript_tick` to channel clients (DW-17).

## Boundaries & Constraints

**Always:** Test-only change — production modules (`recog/ktalk_bridge.py`, `recog/copilot_supervisor.py`, `recog/web/server.py`, `recog/web/static/app.js`) stay untouched. New tests follow the existing given/when/then docstring convention and reuse existing helpers (`RecordingClient`, `_copilot_frames`, `_bridge_with_supervisor`, `_make_storage_for_meeting`, `FakeSubscriber`, `FakeStorage`, `_start_mock_bridge`) instead of duplicating them. DW-11's test must consume the frame the supervisor sends over a real websocket — never a hand-built dict re-stating the key literal.

**Block If:** Any of the four contracts turns out to already be violated by current code (a new test fails against unmodified production code for a reason that is not a test bug).

**Never:** Do not edit `_bmad-output/implementation-artifacts/deferred-work.md` (the orchestrator records resolution). Do not modify or weaken existing tests — add new ones. Do not add a beforeunload/pagehide sender or any stop-on-disconnect behavior (the absence IS the contract).

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| DW-11 cross-module hello | Supervisor with live manual session connects to mock WS; captured raw frame fed to `KTalkBridge._process_message` | `_manual_copilot_active` becomes True; reply is `meeting_state {active: true, meeting_id: "manual"}` | A one-sided rename of `current_meeting_id` in either module fails this test |
| DW-14 tab disconnect | Live-stream WS client connects (init received), sends one whitelisted frame, then disconnects | `FakeSubscriber.sent` contains only that frame — the disconnect path added nothing | A future stop-on-disconnect sender turns the assertion red |
| DW-16 no-channel echo audience | No channel clients; extension client, subscriber client, and supervisor client registered; originator sends `copilot_chat_message` | Originator alone receives `copilot_status {status: "channel_disconnected"}`; every other registered client has `sent == []` | Widening the echo to broadcast fails the negative assertions |
| DW-17 manual spawn path | `copilot_spawn_request` relayed via registered supervisor, mirror armed; channel client registered; ≥6 `send_transcript` calls | `_copilot_tick_state` stays `None`; no `transcript_tick` frame reaches the channel client | Seeding tick state from a manual path turns the assertion red |
| DW-17 hello re-arm path | `supervisor_hello {current_meeting_id: "manual"}` re-arms the mirror; channel client registered; ≥6 `send_transcript` calls | Same as above: tick state `None`, zero `transcript_tick` frames | Same |

</intent-contract>

## Code Map

- `tests/test_copilot_supervisor.py` -- DW-11 lands here: `_start_mock_bridge` / `_collect_hello` harness (~line 868) captures the supervisor's raw hello; `test_supervisor_hello_reports_live_manual_session` (~line 912) shows how to build a live manual session (`handle_frame(copilot_spawn_request)` + `lifecycle.session_ready()`); `test_manual_meeting_id_matches_bridge_constant` (~line 590) is the precedent for importing `ktalk_bridge` inside a test.
- `tests/test_ktalk_bridge.py` -- DW-16 and DW-17 land here. Helpers: `RecordingClient` (line 796), `_copilot_frames` (line 3784), `_bridge_with_supervisor`, `_make_storage_for_meeting` (line 2168), `_activate_tick_state` + tick tests (lines 2471–2600), no-channel echo test `test_copilot_chat_message_without_channel_returns_disconnected_status` (~line 1494), spawn tests (~line 4749), hello re-arm tests (~line 4973).
- `tests/test_frontend_copilot_proxy.py` -- DW-14 lands here. `FakeSubscriber` / `FakeStorage` helpers + `TestClient` websocket pattern (`test_endpoint_relays_to_live_subscriber`, line 422). Starlette's `websocket_connect` context exit joins the handler task, so post-`with` assertions see the completed disconnect path.
- `recog/ktalk_bridge.py` -- Read-only reference: `supervisor_hello` handler (~line 3449, re-arm before `_compute_meeting_state`), `copilot_chat_message` no-channel echo (~line 3052), `copilot_spawn_request` handler (~line 3361), `_copilot_tick_state` seeding (conference paths only: lines 2651, 2668–2673) and `_maybe_emit_transcript_tick` (~line 899).
- `recog/copilot_supervisor.py` -- Read-only reference: hello construction in `run()` (~line 940), `_live_meeting_id` (~line 774).
- `recog/web/server.py` -- Read-only reference: `live_stream` receive loop + `WebSocketDisconnect → pass / unregister` finally-block (~lines 1548–1555).

## Tasks & Acceptance

**Execution:**
- `tests/test_copilot_supervisor.py` -- Add `test_supervisor_hello_frame_rearms_real_bridge` next to the hello tests: build a live manual session, start the supervisor against `_start_mock_bridge`, capture the raw hello via `_collect_hello`-style polling, then construct a real `KTalkBridge` (local import), feed the decoded frame to `await bridge._process_message(frame, websocket=<recording stub>)`, assert `bridge._manual_copilot_active is True` and the single reply is `meeting_state` with `active is True` and `meeting_id == "manual"`. Define a minimal local recording-websocket stub (async `send` appending to a list) — do not import test helpers from `tests/test_ktalk_bridge.py`. -- pins the wire key cross-module (DW-11).
- `tests/test_frontend_copilot_proxy.py` -- Add `test_live_stream_disconnect_emits_nothing_to_bridge`: monkeypatch `get_storage` → `FakeStorage()` and `bridge_subscriber` → `FakeSubscriber(send_result=True)`; connect via `TestClient(...).websocket_connect("/api/live/stream")`, receive init, send one whitelisted frame (positive control proving the recorder is wired), exit the `with` block; assert `subscriber.sent == [<that frame>]` — the disconnect emitted nothing. -- pins "F5 does not kill the manual session" (DW-14).
- `tests/test_ktalk_bridge.py` -- Add `test_copilot_chat_message_no_channel_echo_is_originator_only` next to the existing no-channel test: bridge with zero channel clients; register an extension `RecordingClient` in `_connected_clients`, a subscriber in `_subscriber_clients`, and a supervisor in `_supervisor_clients`; send `copilot_chat_message` from a separate originator socket; assert the originator gets exactly one `channel_disconnected` `copilot_status` and each of the three other clients has `sent == []`. -- pins the originator-only echo audience (DW-16).
- `tests/test_ktalk_bridge.py` -- Add two tick tests next to the tick section: (a) `test_manual_spawn_does_not_seed_tick_state` — use `_bridge_with_supervisor()` (or equivalent: supervisor `RecordingClient` in `_supervisor_clients`), set `bridge._loop = asyncio.get_event_loop()` and `set_meeting_storage(_make_storage_for_meeting("m-1"))`, register a channel client, process `copilot_spawn_request` (assert mirror armed), assert `_copilot_tick_state is None`, flood ≥6 `send_transcript` calls, `await asyncio.sleep(0.05)`, assert no `transcript_tick` in the channel client's frames; (b) `test_supervisor_hello_rearm_does_not_seed_tick_state` — same skeleton but drive `supervisor_hello {current_meeting_id: "manual"}` from a supervisor socket instead of a spawn request. -- pins "no proactive ticks in manual sessions" (DW-17).

**Acceptance Criteria:**
- Given the current production code, when the full suite runs (`python -m pytest tests/ -v`), then all four new tests pass and no existing test regresses.
- Given a simulated one-sided rename (mentally: producer key or consumer `data.get` key changed), when the DW-11 test runs, then the re-arm assertion fails — because the asserted frame travelled from the supervisor's real `run()` send into the bridge's real handler.
- Given the DW-14 test, when the live-stream client's context exits, then the post-`with` assertion runs against the completed handler (starlette joins the task), so the check is deterministic, not racy.
- Given the DW-16/DW-17 tests, when they assert negatives, then each negative is paired with a positive control in the same test (echo received / mirror armed) so an unwired fixture cannot vacuously pass.

## Design Notes

- DW-11 must NOT bypass the WS hop: the point is that the asserted bytes are produced by `Supervisor.run()` itself. Reuse the `test_supervisor_hello_reports_live_manual_session` setup (`autospawn=False`, `handle_frame({"type": "copilot_spawn_request", "request_id": ...})`, `lifecycle.session_ready()`) so the captured hello carries `"manual"` — a `None` hello cannot exercise the re-arm branch.
- `_make_storage_for_meeting` returns a `Mock`, so `_is_meeting_active()` is False (its `meta.get("source")` is a Mock, `is_conference_source` rejects it) — the spawn guard passes and the hello re-arm proceeds; no extension clients are registered in the DW-17 tests, keeping the meeting inactive by both criteria.
- Existing test `test_copilot_chat_message_without_channel_returns_disconnected_status` stays untouched; the new DW-16 test is additive.

## Verification

**Commands:**
- `python -m pytest tests/test_copilot_supervisor.py tests/test_ktalk_bridge.py tests/test_frontend_copilot_proxy.py -v` -- expected: all pass, including 5 new tests.
- `python -m pytest tests/ -q` -- expected: full suite green, no regressions.
- `python -m compileall recog frontend` -- expected: exit 0 (repo convention syntax check).

## Spec Change Log

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 3: (high 0, medium 0, low 3)
- defer: 1: (high 0, medium 0, low 1)
- reject: 10
- addressed_findings:
  - `[low]` `[patch]` DW-17 tests' negative assertions could pass vacuously if transcript recording broke — added `assert len(bridge._copilot_transcript_buffer) == 6` positive control after the six-call flood in both tests.
  - `[low]` `[patch]` DW-11 test's `assert len(replies) == 1` coupled it to "hello reply is exactly one frame forever" — now filters decoded replies to `type == "meeting_state"` and asserts exactly one such frame.
  - `[low]` `[patch]` DW-14 test docstring claimed the full page-reload invariant while only the server-side disconnect path is exercised — docstring narrowed to the server surface, noting the app.js half is not covered.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 2: (high 0, medium 0, low 2)
- defer: 0
- reject: 12
- addressed_findings:
  - `[low]` `[patch]` DW-14's endpoint positive control used a chat frame and did not establish the named manual-session path — changed it to a `copilot_spawn_request` while preserving the post-disconnect exact-list assertion.
  - `[low]` `[patch]` The five added tests did not follow the repository's given-when-then naming rule — renamed them without changing their exercised behavior.

## Auto Run Result

Summary: Fresh review hardened the four Copilot contract regression groups without changing production code. The DW-14 disconnect test now establishes the manual-spawn path, and all five added test functions use given-when-then names.

Files changed:
- `tests/test_copilot_supervisor.py` — Renamed the DW-11 cross-module hello test to the given-when-then convention.
- `tests/test_frontend_copilot_proxy.py` — Made the DW-14 positive control a manual spawn request and renamed the test.
- `tests/test_ktalk_bridge.py` — Renamed the DW-16 and DW-17 tests to the given-when-then convention.

Review findings:
- Patches applied: 2 low-severity findings.
- Items deferred: 0.
- Items rejected: 12.

Follow-up review recommendation: false. Patched findings: high 0, medium 0, low 2; score = `3 × 0 + 1 × 2 = 2`.

Verification:
- `venv-gigaam/bin/python -m pytest tests/test_copilot_supervisor.py tests/test_ktalk_bridge.py tests/test_frontend_copilot_proxy.py -v` — 347 passed.
- `venv-gigaam/bin/python -m pytest tests/ -q` — 799 passed, 2 deselected.
- `venv-gigaam/bin/python -m compileall recog frontend` — exit 0.
- `git diff --check` — exit 0.

Residual risks: DW-14 intentionally pins the `/api/live/stream` server disconnect surface described by the intent contract; browser-side unload behavior remains outside this regression test. No residual worktree artifacts were visible before commit.
