# Google Meet conference platform (DOM speaker diarization)

## Overview
- Add Google Meet (`meet.google.com`, source `gmeet`) as a fourth conference platform next to KTalk, Jitsi (vas3k/jitsi) and Element Call (matrix), so meetings held in Meet get speaker-attributed transcripts.
- Tier 1 only: mixed system audio + DOM-based active-speaker signals (same path as vas3k/matrix). No per-participant PCM, no refine.

## Context (from discovery)

### Live findings in a Meet test room (2026-09-26, two devices of one account)
- Participant tile: `[data-participant-id="spaces/<space>/devices/<n>"]` — stable per-device id. Display name = first non-empty line of the tile's `innerText` (the name appears twice; dedupe lines).
- Speaking indicator: inside the tile `[jsname="QgSmzd"]` (jscontroller `YQvg8b`). Silent state has class `gjg47c`; while speaking the class flips to `wEsLMd` / `Oaajhc` (volume levels) and back to `gjg47c` on silence. Latency ≈1–2 s after audio. Class names are obfuscated; `jsname` is more stable. A sibling `[jsname="QgSmzd"][jscontroller="tae9tc"]` flips the same way.
- Document Picture-in-Picture: when the Meet tab is hidden Meet moves the tiles into `documentPictureInPicture.window.document`; the main `document` then has 0 tiles. The adapter must scan and observe both documents and react to the `documentPictureInPicture` `enter` event. During the handover there can be a moment with 0 tiles in every document.
- Self tile candidate: in the observed session one tile had `button[aria-label="Reframe"]` and `button[aria-label="More options for <name>"]` (UI language dependent — verify; treat as best-effort).
- Audio: no `<audio>`/`<video>` with audio tracks exist in the DOM; `window.RTCPeerConnection` and its prototype are frozen / non-writable before `document_idle`; Meet forwards a few "loudest speaker" virtual streams, not one per participant → per-participant PCM is out of scope.
- CSP: response header contains `require-trusted-types-for 'script'`, no `trusted-types` directive, no default TT policy. HTML assignment sinks in the floating-panel path: `browser-extension/src/ui.js`, `src/ui-vas3k.js` and `src/debug/panel.js` (`:23`, `:80`; `ensureDebugPanel()` runs unconditionally from `setupUI()` in `init.js:171,183`, and a throw inside the `init.js:356` 2 s interval skips `floatingUiInitialized = true` and `tryAttachConference()` forever). `src/utils.js:75` (`escapeHtml`) only *reads* `innerHTML` (getter) — not a sink. The authoritative list is whatever `grep -nE "innerHTML\s*=|outerHTML\s*=|insertAdjacentHTML"` over `src/` (excluding `*.test.js`) returns at implementation time — do not trust counts in this plan.
- Meeting URL `https://meet.google.com/abc-defg-hij` already yields `live_id="abc-defg-hij"`, `host="meet.google.com"` in `recog/meeting_storage.py:405-418` — no backend change needed for ids. Non-room paths: `/`, `/home`, `/landing`. Meet navigates from these to a room without a page reload.
- `meeting.js:158` sends `meeting_leave` on a single negative 2 s `isInMeeting()` tick, and `meeting.js:32` falls back to `location.href` when `adapter.getMeetingUrl()` is null — so `isInMeeting()` must be debounced and must never be true outside a room path.
- `init.js:40-44,106-115`: `waitForConference` does not set `state.activeConfRef`, so ~2 s after join `tryAttachConference()` always calls `adapter.dispose()` and resubscribes once. Subscriptions must therefore be fully re-creatable by `subscribeToSpeakerChanges`, and `dispose()` must keep per-participant state and the stable conference object (as `ElementCallAdapter.dispose()` does, `matrix.js:539-557`).

### Code touch points (pattern from `docs/plans/completed/20260709-element-call-matrix-platform.md`)
- Backend: `recog/meeting_storage.py:47` `CONFERENCE_SOURCES` (drives `is_conference_source`: copilot autospawn, end-on-extension-gone, session_pause, PCM gate); `recog/conference_hosts.py:11` `PLATFORMS` (also the reserved `source_aliases` key set, `:54-56`) and `:41-42` built-in host rejection; `recog/ws_origin.py:30` `ALLOWED_ORIGINS` (must mirror manifest — asserted exactly by `tests/test_ws_origin.py:126-157`, so it lands in the same commit as the manifest).
- Tests hardcoding platform lists: `tests/test_ktalk_bridge.py:3447-3461` (+ parametrize below), `tests/test_conference_hosts.py:70-82`, `tests/test_ktalk_only.py:136`.
- Webapp: `recog/web/static/app.js:173-184` `SOURCE_LABELS`, `app.js:1070` `COPILOT_CONFERENCE_SOURCES`, `recog/web/static/style.css:27,521` (`--source-matrix`, `.source-matrix`).
- Extension: `browser-extension/manifest.json` (host_permissions, content_scripts.matches, web_accessible_resources); `scripts/conference-hosts.js:34-37` built-in host rejection and `:16` reserved `source_aliases` names; `src/platforms/index.js` (`detectPlatform` `:29-49`, `getPlatformAdapter` `:65-89`, `getPlatformDisplayName` `:102`, `isJitsiPlatform` `:127-130` — gates floating panel + retry-forever loop); `src/init.js` (waiting panel/retry `:35-58,96-101`, 2 s UI interval `:354-368`); `src/ui-vas3k.js:583` `showVas3kWaitingState` (existing floating-panel status line `.jitsi-speaker-name`); adapter interface used by `init.js`, `speaker.js`, `meeting.js` (see `src/platforms/matrix.js` as the model — `ElementCallAdapter(win, doc)` is jsdom-testable). `src/ui-copilot-early-frame.test.js` is an existing harness that imports `init.js`.
- Adapter interface: `waitForConference`, `isConferenceAvailable`, `isInMeeting`, `isInPrejoin`, `getConference`/`getJitsiConference` (stable object identity), `isConferenceReady`, `getActiveSpeaker`, `getParticipants`, `getParticipantName`, `isMicMuted`, `anyMicOn`, `subscribeToSpeakerChanges`, `onParticipantJoined` (optional), `getMeetingUrl`, `getMeetingTitle`, `getTabContainerSelector`/`getContentAreaSelector`/`supportsTabInjection`, `dispose`. New optional hooks introduced here (gmeet only, callers treat absence as "no opinion"): `isRoomPage()`, `getDiagnostic()`.

### Constraints
- The working tree contains unrelated uncommitted changes by other agents (e.g. `browser-extension/src/state.js`, `browser-extension/src/ui.test.js`, `docs/usage.md`, `recog/ai_defaults.py`, `recog/web/.env.example`, `tests/test_ai_provider.py`). Do NOT stage, revert, reformat or commit them. Commit only files you touched, listing paths explicitly (`git commit -m "..." -- path1 path2`). If a file you must edit already has foreign uncommitted hunks, stop and record a ⚠️ blocker instead of committing it. New extension tests therefore go into new files (never `ui.test.js`).
- Commit messages: no ticket prefix (not a stash repo); follow the repo/CLAUDE.md conventions; one coherent idea per commit; every commit leaves `npm test` and pytest green (no commit depends on a later one).
- Do not edit `.env` files.

## Development Approach
- **Testing approach**: Regular
- Complete each task fully (including its commit) before moving to the next
- Test names follow `given - when - then`
- Run `cd browser-extension && npm test` for extension tasks and `venv-gigaam/bin/python -m pytest tests/ -q -m "not e2e"` for backend tasks; `./scripts/check.sh` at the end

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

### Task 1: Backend accepts the `gmeet` source
- [x] Add `gmeet` to `CONFERENCE_SOURCES` in `recog/meeting_storage.py` and to the platform docstrings there
- [x] Add `gmeet` to `PLATFORMS` in `recog/conference_hosts.py` (this also reserves `gmeet` as a `source_aliases` key); reject `meet.google.com` in config hosts like the other built-in hosts
- [x] Do NOT touch `recog/ws_origin.py` here (the origin lands with the manifest in Task 5, keeping the exact-mirror test green)
- [x] Extend tests: `tests/test_ktalk_bridge.py` conference-source parametrize, `tests/test_conference_hosts.py` (incl. `meet.google.com` rejection and `gmeet` as a reserved alias key), `tests/test_ktalk_only.py` rejection parametrize; add a `on_meeting_join` test for `https://meet.google.com/qrs-tuvw-xyz` and `...?authuser=0` → `live_id == "qrs-tuvw-xyz"`, `source == "gmeet"` (placed in `tests/test_meeting_storage.py` next to the other `on_meeting_join` URL tests)
- [x] Run pytest
- [x] Commit (explicit paths): `recog/meeting_storage.py recog/conference_hosts.py tests/test_ktalk_bridge.py tests/test_conference_hosts.py tests/test_ktalk_only.py` (+ `tests/test_meeting_storage.py`)

### Task 2: Webapp shows the Google Meet source
- [x] `recog/web/static/app.js`: `SOURCE_LABELS.gmeet = 'Google Meet'`, add `gmeet` to `COPILOT_CONFERENCE_SOURCES`
- [x] `recog/web/static/style.css`: `--source-gmeet` color (light + dark tokens like matrix) and `.source-gmeet` badge
- [x] Run any existing frontend/static tests that cover source labels (`node --test tests/*.test.cjs`: 73 pass; frontend/static/copilot pytest subset: 335 pass)
- [x] Commit (explicit paths): `recog/web/static/app.js recog/web/static/style.css` (+ any test file touched) — added a `gmeet` → `Google Meet` dropdown test in `tests/frontend_meetings.test.cjs`; there are no dark-theme tokens in `style.css` (one `:root` palette), so only one `--source-gmeet` was added

### Task 3: Floating panel survives Trusted Types (helper refactor, no platform wiring)
- [x] Add a helper in `browser-extension/src/utils.js` (e.g. `toTrustedHtml(html)`/`setHtml(el, html)`) that lazily creates `trustedTypes.createPolicy('recog', {createHTML: s => s})` inside try/catch and falls back to the raw string when TT is unavailable or policy creation fails
- [x] Build the sink list with `grep -nE "innerHTML\s*=|outerHTML\s*=|insertAdjacentHTML" browser-extension/src --include=*.js` (excluding `*.test.js`) and route every assignment site through the helper — expected files: `src/ui.js`, `src/ui-vas3k.js`, `src/debug/panel.js` (15 innerHTML sites wrapped in `toTrustedHtml(...)`; no other TT sinks found). `escapeHtml` in `utils.js` is a getter and stays as is. No behavior change on other platforms
- [x] Unit test for the helper in a NEW file `browser-extension/src/utils.test.js` (with and without a fake `trustedTypes`; policy creation throwing → raw string fallback); do not touch `ui.test.js`
- [x] Run `npm test` and `npm run build`
- [x] Commit (explicit paths): `browser-extension/src/utils.js browser-extension/src/utils.test.js browser-extension/src/ui.js browser-extension/src/ui-vas3k.js browser-extension/src/debug/panel.js`

### Task 4: GoogleMeetAdapter with DOM speaker detection (not wired yet)
- [x] Create `browser-extension/src/platforms/gmeet.js` exporting `GoogleMeetAdapter(win = window, doc = document)` implementing the adapter interface listed in Context; nothing imports it yet
- [x] Single `GMEET_SELECTORS` constant: `tile: '[data-participant-id]'`, `speaking: '[jsname="QgSmzd"]'`, `silentClass: 'gjg47c'`, `speakingClasses: ['wEsLMd', 'Oaajhc']`, `inCallMarker` (leave-call control; best-effort, verify live), self-tile marker(s), mute marker(s) — so a Meet redesign is a one-place fix
- [x] `_docs()` returns main document + `win.documentPictureInPicture?.window?.document`
- [x] `subscribeToSpeakerChanges(cb)` creates ALL live subscriptions: a `MutationObserver` (attributes `class`, `childList`, subtree) on every document in `_docs()` at that moment (so an already-open PiP is covered), the `documentPictureInPicture` `enter` listener (attach an observer to the new PiP document) and the PiP window `pagehide` listener (drop that observer); invokes `cb` on indicator/tile mutations, throttled (~100 ms); `onParticipantJoined` on new tile ids
- [x] `getParticipants()`: tiles from all documents deduped by `data-participant-id`; `name` from the first non-empty text line; `isSpeaking` = positive evidence only: some indicator in the tile has a class from `speakingClasses`; an indicator with neither `silentClass` nor any `speakingClasses` is "unknown" and counts as not speaking; `isLocal` best-effort via self-tile marker; `micMuted` best-effort (false when unknown)
- [x] Speaking state is tracked per pid across documents (a pid is speaking if its indicator speaks in any document) and survives tile removal/re-add; `speakingSince[pid]` is stamped only on a real silent→speaking transition of that merged state (level changes `wEsLMd`↔`Oaajhc` and PiP re-adds of an already-speaking pid do not restamp)
- [x] `getActiveSpeaker()`: keep the current speaker while it is still speaking or has been silent < ~1 s (grace); otherwise the speaking pid with the latest `speakingSince`; null when none or when the state is "unknown" (see drift below). Schedule one re-evaluation timer (grace + small margin) when the current speaker goes silent so the switch fires `cb` without waiting for another mutation
- [x] `getMeetingUrl()`: `origin + pathname` only for paths matching `^/[a-z]{3}-[a-z]{4}-[a-z]{3}$`, else null; `isRoomPage()` = `getMeetingUrl() !== null`
- [x] `isInMeeting()` = room path AND (tiles present in any document now, OR tiles were last seen < ~6 s ago on the same room path — ≈3 missed 2 s ticks, absorbs the PiP handover). True immediately when tiles appear; false immediately on a non-room path or a different room path. Time-based (not call-counted) so multiple callers don't skew it; `isConferenceAvailable()` = `isInMeeting()`; `isInPrejoin()` = room path and not `isInMeeting()`
- [x] `getMeetingTitle()` = meeting code fallback (use `document.title` "Meet – <code>" parsing; calendar title if cheaply available); `getConference()` and `getJitsiConference()` both return the SAME stable object for the adapter's lifetime; `supportsTabInjection()` → false
- [x] `dispose()` disconnects observers, removes the PiP `enter`/`pagehide` listeners and clears timers, but keeps per-pid speaking state, `speakingSince`, the current speaker and the stable conference object (mirrors `matrix.js:539-557`), so init.js's post-join dispose→resubscribe is lossless
- [x] Selector-drift detection, only while the in-call marker (`inCallMarker`) is present (lobby / post-call pages never trigger it): (a) in-call for >30 s with no tiles in any document; (b) tiles present but no speaking element; (c) an indicator with neither `silentClass` nor any `speakingClasses`; (d) ≥2 tiles and all of them "speaking" continuously for >30 s. On first detection: `console.warn('[recog][gmeet] ...')` once and set a short user-facing text returned by `getDiagnostic()` (null otherwise). While (c) or (d) holds, `getActiveSpeaker()` returns null
- [x] `src/platforms/gmeet.test.js` in the style of `matrix.test.js`, fixtures built from the exported `GMEET_SELECTORS`, fake timers where needed: class flip → `cb` + `getActiveSpeaker`; two speakers with no current speaker → most recent transition wins; current speaker kept while speaking and for the ~1 s grace, then switches via the timer; level oscillation `wEsLMd`↔`Oaajhc` does not change the winner; PiP re-add of a speaking tile does not change the winner; renamed silent class (indicator with no known class) → not speaking, `getActiveSpeaker()` null, `getDiagnostic()` set; lobby page without in-call marker → no diagnostic; fake `win.documentPictureInPicture` with a second jsdom document (already open at subscribe time AND opened later via `enter`); `pagehide` stops observing the PiP document; a pid present in both documents counts once; a mutation burst fires `cb` once per throttle window; tile-less tick during PiP move keeps `isInMeeting()` true, stays false after the ~6 s window; `isInMeeting()` false on `/home` even with tiles; `getMeetingUrl` for `/home` vs room code; `getConference() === getJitsiConference()` and stable; dispose → resubscribe → PiP flip → `cb` fires; dispose alone stops callbacks
- [x] Run `npm test`
- [x] Commit (explicit paths): `browser-extension/src/platforms/gmeet.js browser-extension/src/platforms/gmeet.test.js`

### Task 5: Wire meet.google.com to the `gmeet` platform
- [x] `browser-extension/manifest.json`: add `https://meet.google.com/*` to host_permissions, the content_scripts matches and web_accessible_resources matches; update description
- [x] `recog/ws_origin.py`: add `"https://meet.google.com"` to `ALLOWED_ORIGINS` (same commit as the manifest — `tests/test_ws_origin.py` asserts an exact mirror)
- [x] `browser-extension/scripts/conference-hosts.js`: reject `meet.google.com` as a configured host (built-in, `:34-37`) and add `gmeet` to the reserved `source_aliases` names (`:16`)
- [x] `src/platforms/index.js`: `detectPlatform` → `gmeet` for hostname `meet.google.com` (host-wide, since Meet navigates into rooms without reload); `getPlatformAdapter` → `GoogleMeetAdapter`; display name `Google Meet`; replace the `isJitsiPlatform()` name list with `!!platform && platform !== 'ktalk'` (update its doc comment) so future platforms need no list edit; `isJitsiPlatform`/`getPlatformDisplayName` take an optional `platform` (default `getPlatform()`) so the jsdom-pinned test file can cover every platform
- [x] `src/init.js`: when `adapter.isRoomPage?.() === false`, the 2 s UI interval skips `setupUI()` (no waiting floating panel on `/`, `/home`, `/landing`) and the `waitForConference` catch neither shows the waiting state nor reschedules the retry — the existing 2 s `tryAttachConference()` picks the conference up once the user enters a room. Adapters without `isRoomPage` behave exactly as today
- [x] `src/init.js` + `src/ui-vas3k.js`: `showVas3kWaitingState(text = 'Ожидание подключения...')` gains an optional text; in the 2 s interval, after `tryAttachConference()`, if `adapter.getDiagnostic?.()` returns text, show it via `showVas3kWaitingState(text)` (existing panel status line, `textContent`, no new UI)
- [x] Tests: `platforms/index.test.js` (detection for `meet.google.com` room and `/landing`, display name, `isJitsiPlatform` true for gmeet/matrix and false for ktalk/null), `manifest.test.js`, `conference-hosts.test.js` (host rejection + reserved `gmeet` alias); a NEW `src/init-gmeet.test.js` modeled on `ui-copilot-early-frame.test.js`: on `/landing` the interval creates no floating panel, on a room path it does, a set diagnostic appears in `.jitsi-speaker-name`; keep `tests/test_ws_origin.py` green
- [x] Run `npm test`, `npm run build` and pytest (incl. `tests/test_ws_origin.py`)
- [x] Commit (explicit paths): `browser-extension/manifest.json recog/ws_origin.py browser-extension/scripts/conference-hosts.js browser-extension/src/platforms/index.js browser-extension/src/init.js browser-extension/src/ui-vas3k.js browser-extension/src/platforms/index.test.js browser-extension/src/manifest.test.js browser-extension/src/conference-hosts.test.js browser-extension/src/init-gmeet.test.js` (+ `tests/test_ws_origin.py` if touched)

### Task 6: Documentation
- [x] `CLAUDE.md` (AGENTS.md is a symlink): list Google Meet among platforms and add a short Google Meet paragraph (DOM indicator `jsname=QgSmzd` with positive speaking classes, Document PiP, `isInMeeting` debounce, why no per-participant PCM, TT helper, `isRoomPage`/`getDiagnostic` hooks)
- [x] `docs/conference-hosts.md` and `browser-extension/README.md`: built-in `meet.google.com`, reserved `gmeet` alias
- [x] Run `./scripts/check.sh` (with `RECOG_CHECK_PYTHON` pointing at the main checkout's `venv-gigaam`; the worktree has none)
- [x] ➕ `./scripts/check.sh` Ruff gate flagged the `ALLOWED_ORIGINS` statement touched in Task 5 (`recog/ws_origin.py`) as unformatted — reformatted that statement only, in a separate style commit
- [x] Commit (explicit paths): `CLAUDE.md docs/conference-hosts.md browser-extension/README.md`

### Task 7: Push
- [x] Verify `git status` shows only the foreign uncommitted files listed in Constraints (plus this plan) — working tree clean
- [ ] Push to current branch (skipped - blocked by repo policy, needs the user)
- ⚠️ Not pushed: the public `origin` (github.com/materkey/recog) must hold only `main`, feature branches stay local, and nothing employer-specific or real room ids may reach it. The real test room id is replaced in the tree with the placeholder `qrs-tuvw-xyz` (review fix), but earlier branch commits still contain it and every commit carries an employer author e-mail. Before merging to `main`: squash/rewrite the branch history and fix the author e-mail (needs the user), then merge/push `main` only.

## Post-Completion
- `npm run build`, reload the extension from `browser-extension/dist/`, `./recog-service.sh restart`
- Manual e2e in a Meet test room with two devices: meeting appears with `source=gmeet`, `live_id=<room code>`; remote speech attributed by name; attribution keeps working with the tab hidden (PiP) and no `meeting_leave` is sent during the PiP handover; no floating panel on `/landing`; floating panel renders with no TT errors in the console (incl. debug panel); `injected.js` actually loads under Meet CSP (expected: extension URLs bypass page CSP; if not — follow-up: `world: "MAIN"` content script entry for Meet); leave → increments; rejoin within 10 min reopens the same meeting; copilot spawns with `RECOG_COPILOT_AUTOSPAWN=1`
- Verify / fix the self-tile, mute and in-call (leave-call) selectors on a live call (UI language dependent); if the in-call marker itself breaks, drift warnings go silent — re-check it after Meet UI changes
- Check the `[data-participant-id]` count on the pre-join (lobby) screen; if >0, require the in-call marker in `isInMeeting()` (➕ task)
- Move this plan to `docs/plans/completed/` after e2e
