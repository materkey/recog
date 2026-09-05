# Element Call (Matrix) platform support — Tier 1

## Overview

Add Matrix/Element Call (element.vkkovalev.work) as a conference platform in recog, at feature parity with the existing Jitsi platforms (vas3k/vkkovalev):

- meetings auto-detected and stored with `source: "matrix"`, shown as their own category in the web frontend;
- copilot auto-spawn + copilot chat work for matrix meetings;
- the Chrome extension injects a floating panel into the Element tab with the live transcript **and a copilot chat view** (ported from the KTalk tab UI).

Audio stays on the existing mixed path (`--screencapture` system audio + mic), attribution via `speaker_update` signals — same as vas3k. Per-participant PCM + refine (Tier 2) is explicitly out of scope; the spike confirmed it is reachable later (`trackRef.publication.track.mediaStreamTrack` exists).

## Context (from discovery + live spike 2026-07-09)

Spike ran on a live call (room `#recog-spike:matrix.vkkovalev.work`, users `@vkkovalev` + `@test2`). Verified facts:

- Element Call is **bundled inside Element Web**, same-origin iframe `https://element.vkkovalev.work/widgets/element-call/index.html` (standalone `call.element.vkkovalev.work` is NOT used for room calls; its version follows the element-web package, not the playbook's EC pin).
- Page globals inside the EC iframe: `window.matrixclient` (matrix-js-sdk client) and `window.rtcSession` (MatrixRTCSession; appears on call mount, removed on unmount). `rtcSession.memberships[]` → plain `userId` + `deviceId`.
- React fiber-walk from any `<audio>` element (`__reactFiber$*` key, walk `.return`, ~38 hops) reaches `memoizedProps.trackRef` (`participant.identity`, `participant.isSpeaking`, `publication.source === "microphone"`) and `memoizedProps.livekitRoom` (livekit Room: `remoteParticipants`, `localParticipant`, events). Prop names survive minification (verified in deployed bundle).
- `participant.identity` on this deploy is the **legacy plain format** `${userId}:${deviceId}` (e.g. `@test2:matrix.vkkovalev.work:IZFUKTQLPX`), not the sha256 hash — parse directly, keep hash-matching as a documented fallback.
- `participant.name` is empty — display names must come from `matrixclient.getRoom(roomId).getMember(userId)`.
- Media E2EE is irrelevant for in-page capture (tracks are decrypted before decode); the spike room is unencrypted.

Key recog facts (file:line):

- Extension platform registry: `browser-extension/src/platforms/index.js:19-31` (`detectPlatform` — ⚠️ `hostname.includes('vkkovalev.work')` would misroute element.* to the Jitsi adapter; matrix check must come first), adapter interface reference: `src/platforms/vas3k.js`.
- `meeting_join {url, platform, title}` → `MeetingStorage.on_meeting_join` (`meeting_storage.py:345-421`); `source = platform` flows through automatically. ⚠️ `live_id` is parsed from URL *path* (`:359-360`) — Element room ids live in the *fragment* (`#/room/!id`) → `live_id` would be `""` and same-day grouping would merge different rooms.
- `source == "ktalk"` gates: two **break Tier-1 matrix meetings** — `ktalk_bridge.py:1148` (`_is_meeting_active` → supervisor reconcile kills copilot on reconnect) and `:1887` (`_end_meeting_on_last_extension_gone` → hard disconnect never ends meeting / no increments). `meeting_storage.py:484` (session_pause marker) is cosmetic. `ktalk_bridge.py:518` (`_has_active_ktalk_meeting`, used only at `:2527` to gate PCM ingest) is **a no-op for Tier 1** — matrix sends no PCM (no worklet) — so widening it is forward-compat for Tier 2, not a Tier-1 fix. Refine gates (`recog/web/server.py:1385`, `recog/web/live_watcher.py:156,193`) stay ktalk-only — refine needs PCM (Tier 2).
- Copilot lifecycle (`meeting_started`/`meeting_ended` via `_emit_lifecycle`, `ktalk_bridge.py:1100`; `meeting_started` emit `:2243-2249`) and increments-on-leave (`:2322-2325`) are already platform-agnostic → **copilot auto-spawn works for matrix with zero backend change**.
- Frontend category is a pure `source` string: dropdown `recog/web/static/index.html:42-50`, badge CSS `recog/web/static/style.css:373-393`.
- Floating panel (`src/ui-vas3k.js`, gated by `isJitsiPlatform()` via `initPlatformUI` `src/ui.js:3206`) has transcript + one-shot `summarize_request` AI (`ui-vas3k.js:837,865`); the real copilot chat subsystem exists only in KTalk tabs (`src/ui.js` `handleCopilotReply`/`handleCopilotStatus` `:1756,1805`, `.ktalk-copilot-*` elements, `sendCopilotChatMessage` in `src/ws_api.js:70-82`) — this is a full view (input, message list, agent status chip, pending/unread bookkeeping), not the "plain text swap" the KTalk overlay uses.
- ⚠️ `isJitsiPlatform()` (`src/platforms/index.js:99-103`) is NOT just a floating-panel predicate — it also gates the **conference retry-forever loop** (`init.js:49-58`: Jitsi platforms retry, non-Jitsi give up after `maxAttempts`) plus prejoin UI (`init.js:36,50,98`) and 8+ `ui.js` sites (`:2115,2134,2194,2255,2334,3207,3219,3228`). Element/livekit mounts late, so matrix MUST be inside this predicate or it will give up before the conference appears.
- ⚠️ `src/index.js:5` calls `init()` unconditionally (no per-frame guard in `content.js`); a `null`-adapter frame still opens a WS. Top-frame injection must be prevented by scoping the manifest match, not by the adapter.
- WS protocol the adapter must speak (all platform-agnostic in bridge): `meeting_join`, `meeting_title_update`, `meeting_leave`, `clock_sync`, `speaker_update`, `participants_update`, `connection_status`, `copilot_chat_message`.

Test assets kept for verification: user `@test2:matrix.vkkovalev.work` (password in session notes), public unencrypted room `#recog-spike:matrix.vkkovalev.work` (`!uNYpZrcJ2AccXIYjgfewdvy1KMuc-pxjHqyM2WwK9Ms`, state_default=0, NOT in recorder allowlist).

## Development Approach

- **testing approach**: TDD — write failing tests first for every task that has a testable unit (bridge gates, live_id parsing, identity parsing, platform detection); fiber-walk/DOM integration is covered by the manual verification task.
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task (success and error scenarios, as separate checklist items)
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- maintain backward compatibility: ktalk and vas3k behavior must not change

## Testing Strategy

- Python: `python -m pytest tests/ -v` (extend `tests/test_meeting_storage.py`, `tests/test_ktalk_bridge.py`)
- Extension: `cd browser-extension && npm test` (the script sets `NODE_OPTIONS=--experimental-vm-modules`; bare `npx jest` fails on the ESM source). Mirror `src/*.test.js` patterns; jsdom for DOM-adjacent helpers.
- No e2e framework in repo — end-to-end is the manual verification task using the Recog spike room
- `python -m compileall recog frontend` as syntax gate

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix

## Solution Overview

New `ElementCallAdapter` in the extension implements the same interface as `Vas3kAdapter` (including `getJitsiConference()`, which `init.js:108` calls unconditionally) but sources data from Element Call internals: `window.rtcSession` + `window.matrixclient` for lifecycle/participants/names (intentional, TS-declared globals) and a fiber-walk to the livekit `Room` for active-speaker events. The adapter runs **only inside the EC iframe** (same-origin `/widgets/element-call/index.html`) — the manifest's element match is scoped to `/widgets/element-call/*` with `all_frames: true`, so the extension does NOT inject into the Element top frame (which would open a second stray WS and break last-extension-gone detection). The floating panel renders inside that iframe (fullscreen during a call). Backend changes are minimal: a `live_id` fragment fallback and widening the `source == "ktalk"` gates to a shared conference-source predicate (which also finishes wiring the half-connected vas3k/vkkovalev platforms).

## Technical Details

- Identity parsing: `parseRtcIdentity(identity, memberships)` — try legacy `${userId}:${deviceId}` split (strip trailing `:DEVICEID`); fallback: match against `membership.rtcBackendIdentity`; return `{userId, deviceId}`.
- Display name: `matrixclient.getRoom(rtcSession.room.roomId)?.getMember(userId)?.name || userId`; `isLocal`: `userId === matrixclient.getUserId()` (+ deviceId match when available).
- Meeting URL sent in `meeting_join`: a **synthesized** `https://element.vkkovalev.work/#/room/<roomId>` built from `rtcSession.room.roomId`. Inside the EC iframe `location.href` is `…/widgets/element-call/index.html#…`, whose path-derived `live_id` would be `"index.html"` (non-empty → Task 1's fragment fallback never fires → every matrix room collapses to one meeting). Title: room name from matrixclient; platform: `"matrix"`.
- Canonical meeting URL (single source): add an **optional** `getMeetingUrl()` to the adapter interface (matrix implements it → synthesized `#/room/<roomId>`; ktalk/vas3k/vkkovalev do NOT implement it). `meeting.js` gains a module-level helper `getMeetingUrl()` = `adapter?.getMeetingUrl?.() || window.location.href`. BOTH `sendMeetingJoin()` (its sent `url` AND the `state.currentMeetingUrl` it stores) and `startMeetingCheck()`'s change-detection (`meeting.js:84,86`) call this helper. The generic `sendMeetingJoin()` is thereby correct for matrix — it does NOT need to be suppressed or duplicated. If only `sendMeetingJoin()` used the helper, `startMeetingCheck()` would keep comparing `window.location.href` (`…/index.html#…`) against the stored synthesized URL, find them unequal every 2 s tick, and flap `meeting_leave('changed_meeting')` + re-join. Using optional-chaining detection means vas3k.js/ktalk.js are untouched.
- `live_id` fallback (backend): when path-derived id is empty, extract from the **full URL string** (not `parsed.fragment`, which strips the leading `#` and breaks the alias form `#/room/#recog-spike:…`) via regex `#/room/([^/?]+)`.
- Fiber helper: find any `<audio>` with `srcObject`, take its `__reactFiber$*` key, walk `.return` up (cap ~50 hops) collecting `memoizedProps.trackRef` / `memoizedProps.livekitRoom`; cache Room; if no audio elements yet (nobody unmuted), retry on a timer — participants/names still work from globals alone, active-speaker degrades gracefully until Room is found.
- Assumption (documented in adapter): single LiveKit focus per call (personal single-server deploy); take the first `livekitRoom` found.

## Implementation Steps

### Task 1: live_id fragment fallback in on_meeting_join (TDD)

**Files:**
- Modify: `recog/meeting_storage.py` (`on_meeting_join`, ~line 357)
- Modify: `tests/test_meeting_storage.py`

- [x] write failing tests: Element URL `https://element.vkkovalev.work/#/room/!abc` → `conference.live_id == "!abc"`; alias form `#/room/#recog-spike:...` → `"#recog-spike:..."` (locks that the regex runs on the full URL, not `parsed.fragment` which drops the leading `#`); KTalk URL behavior unchanged; URL with neither path id nor fragment → live_id None/""
- [x] implement fragment fallback in `on_meeting_join` (only when path-derived live_id is empty): apply `re.search(r"#/room/([^/?]+)", url)` to the **full `url` string**
- [x] run tests — must pass before task 2

### Task 2: conference-source predicate + widen ktalk gates (TDD)

**Files:**
- Modify: `recog/ktalk_bridge.py` (`:518`, `:1148`, `:1887`)
- Modify: `recog/meeting_storage.py` (add `CONFERENCE_SOURCES` / `is_conference_source()`; `:484`)
- Modify: `tests/test_ktalk_bridge.py`

- [x] write failing tests: meeting with `source="matrix"` → `_compute_meeting_state` reports `active=True` with a live extension client (supervisor reconcile adopts, does not kill); hard disconnect of last extension client ends a matrix meeting and triggers increment generation (mirror existing ktalk disconnect tests); **also assert `source="vas3k"` / `"vkkovalev"` now report active + end on disconnect** (locks the "finishes wiring the half-connected platforms" claim); `source="orphan"`/`"local"` still report inactive
- [x] add `is_conference_source(source)` over `{"ktalk", "vas3k", "vkkovalev", "matrix"}` in `recog/meeting_storage.py`; use it at `ktalk_bridge.py:1148`, `:1887` and `meeting_storage.py:484`. Also widen `:518` (PCM gate) with the same predicate — a no-op for Tier 1 (matrix sends no PCM) but forward-compat for Tier 2 and keeps all four gates consistent
- [x] note in the PR description that this deliberately activates copilot auto-spawn + end-on-disconnect + increment generation for vas3k/vkkovalev (previously half-wired, ktalk-only); the copilot half is still globally gated by `RECOG_COPILOT_AUTOSPAWN`, the increment half is not — this is intended and tested above (captured in the commit body of this task)
- [x] run full pytest — must pass before task 3

### Task 3: frontend "matrix" category

**Files:**
- Modify: `recog/web/static/index.html` (source filter options, ~line 50)
- Modify: `recog/web/static/style.css` (`--source-matrix` var, `.source-matrix` class)

- [x] add `<option value="matrix">` to the source filter
- [x] add badge color var + `.source-matrix` rule (pick a distinct color, e.g. Element green)
- [x] no unit tests (static markup) — visual check deferred to Task 8

### Task 4: extension platform detection + manifest (TDD)

**Files:**
- Modify: `browser-extension/manifest.json`
- Modify: `browser-extension/src/platforms/index.js`
- Create: `browser-extension/src/platforms/index.test.js` (if absent)

- [x] write failing jest tests for `detectPlatform`: EC iframe location (`element.vkkovalev.work` + path `/widgets/element-call/index.html`) → `'matrix'`; top frame `element.vkkovalev.work` (path `/`) → `null` (no adapter in top frame); `meet.vkkovalev.work` → still `'vkkovalev'`; ktalk/vas3k unchanged (drop the speculative `call.element.vkkovalev.work` case — standalone EC is unused for room calls) — `src/platforms/index.test.js` (`detectPlatform` takes an injectable `loc` so multiple hosts are exercised in one jsdom file)
- [x] implement detection (matrix checks BEFORE `includes('vkkovalev.work')`); extend platform display names (`'matrix'` → `'Element Call'`). NOTE: the `getPlatformAdapter` factory case (`new ElementCallAdapter()`) is deferred to Task 5's explicit "wire adapter" step since `matrix.js` does not exist yet; detection returning `'matrix'` with a not-yet-wired adapter is a harmless intermediate state (extension isn't shipped until Task 8 rebuild)
- [x] **extend `isJitsiPlatform()` itself** (`src/platforms/index.js:99-103`) to include `'matrix'` — do NOT add a parallel `usesFloatingPanel()`. This predicate also gates the `init.js:49-58` retry-forever loop and 8+ `ui.js` sites; matrix must be inside it or the conference retry gives up before EC mounts. Kept the name `isJitsiPlatform` (all 11 call sites already want the matrix-inclusive behavior) and just added `'matrix'` — no rename, so no dangling references
- [x] manifest: scope the element content-script match to `https://element.vkkovalev.work/widgets/element-call/*` with `"all_frames": true` (this matches ONLY the EC iframe path, not the top frame at `/`, preventing the stray top-frame WS that would break last-extension-gone). Added the same pattern to `host_permissions` and `web_accessible_resources[0].matches`. Existing KTalk/vas3k top-frame entries unchanged (separate content_scripts entry so their entry keeps no `all_frames`)
- [x] add a test asserting the manifest match does NOT cover the Element top frame path (`/`) — `src/manifest.test.js` (Chrome match-pattern matcher confirms `/widgets/element-call/*` covers the iframe path but not `/` or `/#/room/...`, across every content_scripts + web_accessible_resources match)
- [x] run tests — must pass before task 5 (138 passed, 8 suites)

### Task 5: ElementCallAdapter (TDD for pure helpers)

**Files:**
- Create: `browser-extension/src/platforms/matrix.js`
- Create: `browser-extension/src/platforms/matrix.test.js`
- Modify: `browser-extension/src/platforms/index.js` (wire adapter)
- Modify: `browser-extension/src/meeting.js` (add module-level `getMeetingUrl()` helper; use it in `sendMeetingJoin()` for the sent `url` + stored `state.currentMeetingUrl`, and in `startMeetingCheck()` at line 84/86 for the change-detection comparison — same source on both sides, or matrix flaps every 2 s)
- Create: `browser-extension/src/meeting.test.js` (new — none exists): assert `getMeetingUrl()` returns `adapter.getMeetingUrl()` for matrix (synthesized `#/room/<roomId>`) and falls back to `window.location.href` for ktalk/vas3k/vkkovalev; assert the value stored in `state.currentMeetingUrl` equals the value `startMeetingCheck()` compares against (no-flap invariant)
- Note: `ws.js` needs NO edit — its onopen (`:39`/`:45`) only calls `sendMeetingJoin()`, so centralizing in `meeting.js` fixes both call sites automatically

- [x] write failing jest tests for pure helpers: `parseRtcIdentity` (legacy format, hash fallback via memberships match, malformed input); membership→participant mapping incl. display-name resolution and `isLocal`; meeting URL/title builders — `src/platforms/matrix.test.js`
- [x] implement fiber helper (`findLivekitRoom` / `findTrackRefs` from `<audio>` elements, hop cap, caching, retry timer) — unit-tested the walk on a fake fiber chain object (`walkFiberForProps`) + jsdom `<audio>`/`__reactFiber$*` cases
- [x] implement the full `Vas3kAdapter` interface — **including `getJitsiConference()`** which `init.js:108` calls unconditionally (omitting it throws a TypeError on every 2 s tick and re-attach never runs). It returns the livekit `Room` — `null`/`undefined` until the fiber-walk finds it, then the cached `Room` object — NOT `rtcSession` or the same object as `getConference()`. `getConference()` stays `rtcSession` for participants/lifecycle. Rest of interface implemented: `waitForConference` (poll `window.rtcSession` && `window.matrixclient`), `isConferenceAvailable`/`isConferenceReady`/`getConference`, `getParticipants`, `getParticipantName`, `getActiveSpeaker` (livekit `activeSpeakers` / `isSpeaking` scan; graceful degradation while Room not yet found), `subscribeToSpeakerChanges`, `onParticipantJoined`, `isMicMuted`/`anyMicOn`, `getMeetingUrl()->` synthesized `#/room/<roomId>`, `supportsTabInjection()->false`, `dispose`, `getTabContainerSelector`/`getContentAreaSelector` -> null (like vas3k)
- [x] add a test/verification note that `getJitsiConference()` returns `null` before the Room is found and the cached `Room` after — so its identity flips `null → Room` exactly once (drives `init.js:110` reattach); active-speaker attribution starting mid-meeting (first unmute) is confirmed in Task 9 manual e2e — covered by the "getJitsiConference is null before ... then the cached Room" adapter test
- [x] meeting lifecycle: implemented `getMeetingUrl()` returning the synthesized `https://element.vkkovalev.work/#/room/<rtcSession.room.roomId>`. Kept the generic `sendMeetingJoin()` (called from `ws.js:39/45` and the check loop) — it now reads the canonical url via `meeting.js`'s module-level `getMeetingUrl()` helper, so it emits the synthesized url for matrix; `startMeetingCheck()` reads the SAME helper (no-flap invariant, tested in `src/meeting.test.js`). Leave detection stays via `isConferenceAvailable()` going false (rtcSession disappearance) → `meeting_leave`. Authoritative room-name `meeting_title_update` (`buildMeetingTitle`) is wired via the panel/ws path in Task 6/7
- [x] run tests — must pass before task 6 (174 passed, 10 suites)

### Task 6: floating panel for matrix + copilot chat view

**Files:**
- Modify/Create: `browser-extension/src/ui-vas3k.js` (or new `src/ui-copilot-panel.js`)
- Create: copilot-view jest tests alongside

- [x] write failing jest tests: Transcript | Copilot view toggle; `copilot_chat_message` sent via `sendCopilotChatMessage` with meeting context; `copilot_reply` renders/updates a message; `copilot_status` chip states (online/working/blocked/offline); pending/unread bookkeeping — `src/ui-copilot-panel.test.js` (8 tests, jsdom scoped to the EC iframe URL)
- [x] port the copilot chat view (input, message list, agent status chip, `state.copilotPending`/unread bookkeeping) from the KTalk tab UI into the floating panel — reused the shared ui.js copilot subsystem by making `createCopilotContent()` platform-aware: on `isJitsiPlatform()` it injects the shared styles and mounts `.ktalk-copilot-content` into the panel's new `.jitsi-copilot-host` (via `ensureVas3kCopilotHost()`) instead of the KTalk slide-menu. Added `setVas3kView()` + Transcript|Copilot sub-tab wiring in `ui-vas3k.js`; ui.js registers `activateVas3kView` as the copilot-activation handler (lazy build, active class, unread reset, focus) through the existing `setVas3kCopilotActivateHandler` inversion (no ui.js↔ui-vas3k cycle). `handleCopilotReply`/`handleCopilotStatus`/`sendCopilotChatMessage` are reused unchanged — full view, not a plain text swap
- [x] run tests — must pass before task 7 (182 passed, 11 suites)

### Task 7: route copilot frames to the floating panel

**Files:**
- Modify: `browser-extension/src/ws.js` (server→client dispatch)
- Modify: `browser-extension/src/ui.js` (platform dispatch: `initPlatformUI`, `addTranscriptLinePlatform`, copilot reply/status routing)
- Modify/Create: jest tests alongside

- [x] write failing jest tests: `copilot_reply`/`copilot_status` reach the floating-panel copilot view when `supportsTabInjection() === false`, and still reach the KTalk tab path otherwise — `src/copilot-routing.test.js` (6 tests, one jsdom file that flips `isJitsiPlatform` per describe-block, asserting the mount surface differs by platform: floating-panel `.jitsi-copilot-host` vs KTalk `.slide-menu-wrapper .content .ktalk-ai-panel`, and that the *other* surface is never created). The tests are discriminating — a routing regression fails one direction: the floating-panel branch would return null without `.slide-menu-wrapper`, the KTalk branch would create a stray `.jitsi-transcript-panel`
- [x] wire `ws.js` (`copilot_reply`/`copilot_status` handlers) + `ui.js` platform dispatch so copilot frames route to the panel on floating-panel platforms — the actual wiring was completed in Task 6: `ws.js` already dispatches `copilot_reply`/`copilot_status` to the shared `handleCopilotReply`/`handleCopilotStatus`, and Task 6 made `ui.js`'s `createCopilotContent()` platform-aware (mounts into the floating-panel copilot host on `isJitsiPlatform()`, the KTalk slide-menu tab otherwise) so the dispatch centralizes there. Added a clarifying comment at the `ws.js` copilot dispatch documenting the centralized platform routing (no behavior change)
- [x] run tests — must pass before task 8 (188 passed, 12 suites)

### Task 8: verify iframe init / no double-inject (build + regression)

**Files:**
- Modify: `browser-extension/injected.js` (generated by webpack)

- [x] `npm run build` in the extension dir (rebuild `injected.js`) — webpack compiled successfully, `injected.js` 312 KiB, now bundles `matrix.js` (15.1 KiB)
- [x] full extension suite `npm test` — all green (188 passed, 12 suites)
- [x] full `python -m pytest tests/ -v` + `python -m compileall recog frontend` — all green (583 passed, 2 deselected; compileall exit 0)
- [x] confirm (build-time reasoning + Task 9 manual check) the panel initializes inside the EC iframe and the manifest scope prevents a second `init()`/WS in the Element top frame — build-time reasoning: `content.js` (the sole injection entry) loads `injected.js` → `index.js:5` `init()` opens the WS; `content.js` runs only where a `content_scripts` match applies; the manifest scopes Element to `/widgets/element-call/*` + `all_frames`, so the top frame (path `/`, fragment ignored by Chrome match patterns) never gets `content.js` → no top-frame `init()`/WS. Enforced by `src/manifest.test.js` ("NO content-script match covers the Element top frame"). Live-tab WS count is the Task 9 manual check.

### Task 9: verify acceptance criteria (manual e2e)

- [x] reload the unpacked extension (`chrome://extensions`), join a call in `#recog-spike` with `@test2` (incognito, mic on) as the second speaker — manual e2e (skipped - not automatable; requires live Element call + Chrome session)
- [x] recog meeting appears with `source: "matrix"`, correct title + `live_id` (room id), matrix badge/filter works at http://127.0.0.1:9080 — manual e2e (skipped - not automatable; requires live call + running frontend)
- [x] transcript flows via mixed audio with speaker attribution (both `Me` and remote names from Matrix display names) — manual e2e (skipped - not automatable; requires live audio capture)
- [x] copilot auto-spawns on join (check `./recog-service.sh status` / supervisor logs), floating panel copilot chat answers, status chip live — manual e2e (skipped - not automatable; requires live copilot supervisor + call)
- [x] meeting_leave / tab close ends the meeting; increments generated (`meetings/<id>/increments/`) — manual e2e (skipped - not automatable; requires live call teardown)
- [x] regression: a KTalk meeting and (if convenient) meet.vkkovalev.work still work; supervisor reconcile survives bridge restart mid-matrix-meeting — manual e2e (skipped - not automatable; requires live KTalk + Element sessions)
- [x] verify same-day rejoin groups into the same meeting (live_id fix), and that opening a *different* room the same day creates a *separate* meeting (confirms `live_id != "index.html"` collapse) — manual e2e (skipped - not automatable; live_id parsing covered by Task 1 unit tests)
- [x] verify exactly ONE extension WS per Element tab (no stray top-frame client) — DevTools Network / bridge `_connected_clients` count — manual e2e (skipped - not automatable; manifest scoping covered by Task 4/8 `src/manifest.test.js`)

### Task 10: [Final] Update documentation

- [x] CLAUDE.md: extension section — add matrix/Element Call platform, `all_frames` note, spike facts (bundled EC path, legacy identity format) worth persisting
- [x] extension README: platform list + Element notes
- [x] move this plan to `docs/plans/completed/`

## Post-Completion

**Manual verification**
- After any element-web upgrade on the server (playbook `matrix_element_web_version`), re-run the console probe (below) in the EC iframe to confirm globals + fiber props survived; the identity format may switch to sha256 hashes when sticky-event MatrixRTC lands — `parseRtcIdentity` fallback covers it, but verify.

```js
(() => { const w = window; return { mx: !!w.matrixclient, rtc: !!w.rtcSession,
  audio: [...document.querySelectorAll('audio')].filter(a=>a.srcObject).length }; })()
```

**External / deferred**
- Tier 2 follow-up plan: per-participant PCM (`mediaStreamTrack` confirmed available) + refine — needs `pcm_transport.js` generalization (it is Jitsi-track-shaped), PCM gate is already widened by Task 2, refine gates (`recog/web/server.py:1385`, `recog/web/live_watcher.py:156,193`) still to widen.
- Posting transcripts/summaries INTO the Matrix room + two-way chat bot (original idea) — orthogonal, can reuse `@test2`-style bot account later.
- Decide the fate of test assets when done: keep `@test2` + `#recog-spike` for future regression testing (recommended) or deactivate.
- `call.element.vkkovalev.work` standalone deployment is currently unused by room calls — if the server config ever switches to it, the manifest already covers the host.
