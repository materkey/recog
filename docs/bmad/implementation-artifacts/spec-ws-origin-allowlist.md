---
title: 'WS Origin allowlist for bridge and frontend live-stream endpoints'
type: 'feature'
created: '2026-07-25'
status: 'done'
baseline_revision: 'BASE_REVISION'
final_revision: '518ebc3752cf117867025c2b4633229665dd49dc'
review_loop_iteration: 0
followup_review_recommended: false
context: []
warnings: []
---

<intent-contract>

## Intent

**Problem:** The bridge WebSocket server (`ktalk_bridge._handle_client`, port 8766 — the intent's "8765/8766" both land here; the bridge serves one configurable port and the extension's `WS_URL` is `ws://localhost:8766`) and the frontend live-stream endpoint (`recog/web/server.live_stream`, port 9080) accept connections from any browser page: WebSockets bypass CORS, so a malicious webpage can open the localhost socket, speak the protocol (`subscriber_hello` + `copilot_chat_message` / `copilot_spawn_request`), and inject copilot frames into the tool-bearing Claude session.

**Approach:** Add an Origin allowlist checked at accept-time in both endpoints, backed by one shared helper module (`recog/ws_origin.py`). Admit: conference-page origins used by the Chrome extension (`https://*.ktalk.ru` wildcard, `https://meet.vas3k.cloud`, `https://meet.example.org`, `https://element.example.org`), the webapp origin (`http://127.0.0.1:9080` and its `http://localhost:9080` equivalent), and connections with **no** Origin header (Python clients — supervisor, copilot-channel daemon, `BridgeSubscriber` — send none). Everything else is rejected before any frame is processed or client-set registration happens.

## Boundaries & Constraints

**Always:**
- The check runs before the connection is registered anywhere (`_connected_clients` in the bridge; `live_watcher.register` / `websocket.accept()` in the frontend) and before any inbound frame is read.
- Absent Origin header ⇒ allowed (non-browser clients). Present-but-unlisted Origin ⇒ rejected.
- One shared predicate in `recog/ws_origin.py` used by both files (frontend already puts the project root on `sys.path`).
- `*.ktalk.ru` matching must parse the origin's hostname (allow `ktalk.ru` itself and `*.ktalk.ru` subdomains over https) — a substring/suffix check on the raw string that admits `https://evil-ktalk.ru` is a defect.
- Rejections are logged (stderr in the bridge, consistent with its existing logging) so a blocked legitimate client is diagnosable.
- Existing tests using `FakeWebSocket` (no `request` attribute) and `TestClient.websocket_connect` (no Origin header) must keep passing unmodified.

**Block If:** the websockets 15 server object turns out not to expose the handshake request headers inside the handler (would force a `process_request`-based redesign).

**Never:**
- Do not guard `watch_meeting` (`/api/meetings/{id}/watch`) — out of the ledger's scope (read-only keepalive endpoint, no inbound frame routing).
- No env-var/config knob for the allowlist — hardcoded constants mirroring `manifest.json` hosts.
- Do not edit the deferred-work ledger.
- Origin `"null"` (sandboxed iframe) is not an absent header — it must be rejected, not treated as a Python client.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior | Error Handling |
|----------|--------------|---------------------------|----------------|
| Python client | No Origin header | Connection proceeds as today | No error expected |
| Extension on KTalk | `Origin: https://example.ktalk.ru` | Allowed (wildcard subdomain) | No error expected |
| Extension on Jitsi/Element | `Origin: https://meet.vas3k.cloud` (or example.org / element.example.org) | Allowed | No error expected |
| Webapp page | `Origin: http://127.0.0.1:9080` or `http://localhost:9080` | Allowed | No error expected |
| Malicious page → bridge | `Origin: https://evil.example` to port 8766 | Socket closed (code 1008) before registration; no frame processed | Logged to stderr |
| Malicious page → frontend | `Origin: https://evil.example` to `/api/live/stream` | Handshake denied (close before `accept()`); `live_watcher` never registers it | Rejected connection, no crash |
| Suffix spoof | `Origin: https://evil-ktalk.ru` | Rejected | Logged |
| Scheme downgrade | `Origin: http://example.ktalk.ru` | Rejected (conference origins are https-only) | Logged |
| Sandboxed iframe | `Origin: null` (literal string) | Rejected | Logged |

</intent-contract>

## Code Map

- `recog/ws_origin.py` -- NEW: `origin_allowed(origin: str | None) -> bool` + allowlist constants (exact-origin set + ktalk.ru wildcard rule via `urllib.parse.urlsplit` hostname check).
- `ktalk_bridge.py:2030` -- `_handle_client`: guard at the very top, before the `self._lock` block that adds to `_connected_clients`. Origin comes from `getattr(websocket, "request", None)` → `.headers.get("Origin")` (websockets 15 `ServerConnection.request`; `FakeWebSocket` in tests has no such attribute → treated as no-Origin → allowed).
- `recog/web/server.py:1475` -- `live_stream`: check `websocket.headers.get("origin")` before `websocket.accept()`; on rejection `await websocket.close(code=1008)` and return (Starlette sends a handshake denial when closed pre-accept).
- `browser-extension/manifest.json` -- reference only: source of truth for conference hosts; do not edit.
- `tests/test_ktalk_bridge.py` -- existing `FakeWebSocket` harness (line 77) to extend for origin tests.
- `tests/test_frontend_copilot_proxy.py` -- existing `TestClient.websocket_connect` pattern (line 340) for the frontend-side test.

## Tasks & Acceptance

**Execution:**
1. `recog/ws_origin.py` -- create the shared predicate + constants -- single source of truth for both endpoints.
2. `recog/ktalk_bridge.py` -- import the helper; reject disallowed origins at the top of `_handle_client` (close 1008 + stderr log + return) -- blocks protocol injection at the bridge.
3. `recog/web/server.py` -- import the helper; reject disallowed origins in `live_stream` before `accept()` -- blocks the same-class relay at 9080.
4. `tests/test_ws_origin.py` -- unit-test the predicate over the I/O matrix rows (all allow/reject cases including suffix spoof, scheme downgrade, `"null"`, `None`).
5. `tests/test_ktalk_bridge.py` -- add: (a) `FakeWebSocket` variant carrying `request.headers = {"Origin": "https://evil.example"}` → `_handle_client` closes it and never adds it to `_connected_clients`; (b) allowed-origin variant proceeds into the message loop.
6. `tests/test_frontend_copilot_proxy.py` (or a new frontend test file if fixture reuse is awkward) -- `websocket_connect("/api/live/stream", headers={"Origin": "https://evil.example"})` is denied; a no-Origin connect still receives the `init` frame.

**Acceptance Criteria:**
- Given the bridge is running, when a WS client connects with a disallowed Origin header, then the socket is closed before any of its frames are dispatched and it never appears in `_connected_clients`/`_subscriber_clients`.
- Given the frontend server, when a WS client connects to `/api/live/stream` with a disallowed Origin, then the handshake is denied and `live_watcher` never registers the socket.
- Given any client with no Origin header (all existing Python daemons and the whole current test suite), when it connects to either endpoint, then behavior is byte-for-byte unchanged.

## Spec Change Log

## Review Triage Log

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 6: (high 0, medium 3, low 3)
- defer: 1: (high 0, medium 1, low 0)
- reject: 7
- addressed_findings:
  - `[medium]` `[patch]` Origin extraction failed open under the declared `websockets>=12.0` pin (legacy protocol has `request_headers`, not `.request`) — added legacy fallback in `_handle_client` and bumped the pin to `>=14.0`, with a unit test for the legacy path.
  - `[medium]` `[patch]` Manifest ↔ allowlist mirror was unenforced — added a test deriving origins from `manifest.json` `content_scripts[].matches` and asserting `origin_allowed()` for each.
  - `[medium]` `[patch]` Bridge deny-path was verified only through fakes — added two real-server integration tests (`websockets.connect(..., origin=...)`): disallowed origin closes 1008, allowed ktalk subdomain stays connected.
  - `[low]` `[patch]` Duplicate Origin headers raised `MultipleValuesError` through `.get` (traceback + 1011) — header read wrapped in `except LookupError` → clean 1008 rejection, with test.
  - `[low]` `[patch]` Rejection logging was unthrottled on an attacker-controlled path — both guards now log via `log_rate_limited` keyed by origin.
  - `[low]` `[patch]` CLAUDE.md lacked the new module and gating notes — added `ws_origin.py` bullet and origin-gate notes on the bridge/frontend entries.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 7: (high 0, medium 5, low 2)
- defer: 0
- reject: 5: (high 0, medium 3, low 2)
- addressed_findings:
  - `[medium]` `[patch]` The KTalk wildcard accepted URL-shaped values with userinfo, paths, queries, fragments, or whitespace — tightened `origin_allowed()` to accept only serialized-origin components and added rejection cases.
  - `[medium]` `[patch]` The KTalk wildcard ignored the Origin port, including invalid or unrelated ports — parse the port under the `ValueError` guard, accept only absent/default HTTPS port 443, and cover valid/invalid port cases.
  - `[medium]` `[patch]` Frontend header extraction accepted the first of duplicate Origin headers — use `Headers.getlist()`, reject duplicates before `accept()`/registration, and cover the path directly.
  - `[medium]` `[patch]` Attacker-controlled Origin values created unbounded per-Origin log-rate-limit buckets in both endpoints — switched each endpoint to one fixed rejection key and asserted the keys in focused tests.
  - `[medium]` `[patch]` Manifest synchronization remained one-way, so removed conference hosts could remain trusted — compare manifest exact/wildcard hosts against the allowlist in both directions.
  - `[low]` `[patch]` The frontend rejection test only compared final client-set state and could miss transient registration — spy on `live_watcher.register` and assert it is never called.
  - `[low]` `[patch]` New tests did not follow the repository's given-when-then naming convention — renamed every test introduced by this story accordingly.

### 2026-07-25 — Review pass
- intent_gap: 0
- bad_spec: 0
- patch: 1: (high 0, medium 0, low 1)
- defer: 0
- reject: 11: (high 4, medium 3, low 4)
- addressed_findings:
  - `[low]` `[patch]` The KTalk wildcard accepted malformed DNS labels such as `.ktalk.ru`, `evil..ktalk.ru`, percent escapes, and leading/trailing hyphens — validate hostname length and every DNS label before applying the domain rule, with regression cases.

## Verification

**Commands:**
- `./venv-gigaam/bin/python -m pytest tests/test_ws_origin.py tests/test_ktalk_bridge.py tests/test_frontend_copilot_proxy.py -q` -- expected: all pass.
- `./venv-gigaam/bin/python -m pytest tests/ -q --ignore=tests/e2e` -- expected: no regressions.
- `./venv-gigaam/bin/python -m compileall -q recog frontend ktalk_bridge.py` -- expected: exit 0.

## Auto Run Result

Status: done

Summary: Fresh review of the WS Origin allowlist hardened the KTalk wildcard so only valid ASCII DNS labels can match `ktalk.ru` or its subdomains.

Files changed:
- `CLAUDE.md` — documents the shared Origin gate on both WebSocket surfaces.
- `recog/web/server.py` — rejects disallowed or duplicate browser Origins before accepting or registering a live-stream socket.
- `recog/ktalk_bridge.py` — rejects disallowed or duplicate browser Origins before client registration or frame processing.
- `recog/ws_origin.py` — owns the shared allowlist and now validates hostname length and DNS-label syntax before wildcard matching.
- `deploy/python/requirements.txt` — sets the supported `websockets` floor used by the handler request API.
- `tests/test_frontend_copilot_proxy.py` — covers frontend rejection, duplicate headers, no-Origin clients, and the webapp Origin.
- `tests/test_ktalk_bridge.py` — covers fake and real bridge connections across allowed, denied, duplicate, and legacy-header paths.
- `tests/test_ws_origin.py` — covers the policy matrix, manifest parity, malformed Origins, ports, and invalid DNS labels.

Review findings:
- Patches applied: 1 low-severity hostname-validation defect.
- Items deferred: 0; `deferred-work.md` was not modified.
- Items rejected: 11 intent-conflicting, speculative, or already-covered findings.

Follow-up review recommendation: false. Patched findings: high 0, medium 0, low 1; score = `3 × 0 + 1 × 1 = 1`.

Verification:
- Focused Origin/bridge/frontend suite: 248 passed.
- Full non-e2e suite: 777 passed.
- `compileall` for `recog`, `frontend`, and `recog/ktalk_bridge.py`: exit 0.
- Commit coverage check: all eight files in `BASE_REVISION..HEAD` are present; final revision is `518ebc3752cf117867025c2b4633229665dd49dc`.

Residual risks: The allowlist intentionally mitigates browser-origin attacks only. No-Origin native clients and JavaScript already executing on an allowlisted conference origin remain trusted by the explicit intent contract.
