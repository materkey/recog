# Zoom conference platform (web client and native application)

## Overview
- Add Zoom (source `zoom`) as a conference platform with two signal sources:
  the Zoom web client in Chrome through the existing extension, and the native
  Zoom application on macOS through a separate helper that speaks the same
  bridge protocol.
- Tier 1 only: mixed system audio plus active-speaker signals. No
  per-participant PCM, no refine, no Redux tap, no captions as a text source.
- Nothing that needs the meeting organizer's permission (RTMS, Zoom Apps SDK
  `onActiveSpeakerChange`, Meeting SDK raw audio, per-participant local
  recording are excluded).
- A missing name or speaking signal yields `Unknown`; audio and text are never
  dropped and a speaker is never guessed.

## Context (from research; nothing below was observed in a live meeting yet)

### Zoom web client
- URLs: `zoom.us/wc/join/<id>` redirects to `/wc/<id>/join`; the same on
  `app.zoom.us` and regional hosts such as `us05web.zoom.us`. `zoom.us/j/<id>`
  is a launcher page. The stable id is the numeric segment:
  `/wc/(?:join/)?(\d+)`.
- On `app.zoom.us` (PWA) the meeting client is reported to live in
  `iframe#webclient`, same-origin per third-party code; the classic client on
  `zoom.us/wc/<id>/join` has no iframe.
- Third-party selectors (2026, layout dependent, drifting): active tile
  `.speaker-bar-container__video-frame--active`,
  `.speaker-active-container__video-frame`,
  `[class*="single-"][class*="-container__video-frame"]`, gallery
  `.gallery-video-container__video-frame--active` (disputed); name
  `.video-avatar__avatar-footer span`; mute
  `.video-avatar__avatar-footer--view-mute-computer`. The active tile is a
  sticky dominant-speaker spotlight and never the local participant.
- CSP of `/wc/<id>/join` (fetched 2026-10-04): `script-src 'self'
  'wasm-unsafe-eval' 'strict-dynamic' 'nonce-…' blob: https:`, `connect-src *`,
  `upgrade-insecure-requests`, no `require-trusted-types-for`.
- Chrome 153 gates page-world connections to loopback behind a per-origin
  permission (`navigator.permissions` names `local-network-access` /
  `loopback-network`). On `app.zoom.us` a `new WebSocket('ws://localhost:8766')`
  stays in `CONNECTING` until the user answers the prompt; `meet.google.com`
  behaves the same in a profile that never granted it. Every Zoom origin the
  user joins from needs the prompt accepted once. Whether
  `upgrade-insecure-requests` interferes can only be seen after the grant.

### Native Zoom application (macOS, Zoom Workplace 7.1.9)
- Native AppKit, not Electron: the `recog/desktop/` mechanism (Node inspector
  plus `session.loadExtension`) does not apply.
- Zoom Plugin SDK (macOS 1.0.0, IPC with the installed client, needs Zoom
  Workplace 7.0.2+, a General app with the `plugin_sdk:read:connection_meta`
  scope, the same account in the app and the client). Documented: participant
  list, mute state, meeting status. Not documented: any speaking / active
  speaker event. The IPC host is present in the 7.1.9 bundle. Unknown: whether
  it attaches to a meeting joined by hand, whether an ordinary participant gets
  the roster, whether an ad-hoc signature is accepted.
- Packet headers (UDP 8801): a 2022 study maps RTP payload type 112 to speech
  and 99 to silence, with type 113 (mobile clients) indistinguishable. Not
  re-verified since. Mapping an audio SSRC to a participant id by `>> 10` is
  taken from the Web SDK video path and unproven for native RTP. Reading
  headers needs BPF access, which is root-only by default.
- Accessibility: tile descriptions carry name and mute state per third-party
  code; whether an active-speaker marker exists is disputed between sources.

### Code facts
- `recog/meeting_storage.py` `on_meeting_join` takes `live_id` from the last
  path segment and `host` from the netloc, so `/wc/<id>/join` would give every
  meeting `live_id="join"`. Sources therefore synthesize the canonical URL
  `https://zoom.us/j/<meetingId>`: numeric `live_id`, `host` independent of the
  subdomain, no `pwd` in `meta.json`, and the web client and the application
  reopen the same meeting inside the 10 minute window.
- `recog/ktalk_bridge.py` reads the platform from `meeting_join` only and
  treats a client that sends no hello as an extension client; a no-Origin
  client passes `origin_allowed`. A native helper is therefore an ordinary
  bridge client and needs no new protocol.
- A second `meeting_join` while another client owns the meeting is rejected
  (`another_tab_active`), but `speaker_update` / `participants_update` are not
  bound to the owner.
- `tests/test_ws_origin.py` asserts the origin allowlist mirrors
  `browser-extension/manifest.json`; the manifest and the allowlist change in
  one commit.
- `browser-extension/src/meeting.js` sends `meeting_leave` on a single negative
  2 s `isInMeeting()` tick; `init.js` disposes and resubscribes the adapter
  about 2 s after join.

### Constraints
- Local branch `zoom-platform`, no push.
- Only the made-up meeting id `12345678901` may appear in the tree; real ids,
  names and dumps of live meetings stay in the gitignored `.local/`.
- Commit only touched files with explicit paths; every commit leaves the tests
  green.

## Decisions
1. Canonical meeting URL `https://zoom.us/j/<meetingId>` from every source.
2. Content script in the top frame only, `matches: https://*.zoom.us/wc/*`;
   the adapter scans its own document plus `iframe#webclient.contentDocument`.
   Fallback for a cross-origin iframe: a separate `content_scripts` entry with
   `all_frames: true`, as for Element Call.
3. The single `ktalk.ru` origin wildcard becomes a set of wildcard domains with
   the same strict hostname parsing.
4. The native helper is a plain bridge client sending `meeting_join`
   (`platform: "zoom"`), `participants_update`, `speaker_update`,
   `meeting_leave`.

## Development Approach
- Each task is one atomic commit with its tests.
- Test names follow `given - when - then`.
- Backend: `venv-gigaam/bin/python -m pytest tests/ -q -m "not e2e"` and
  `venv-gigaam/bin/python scripts/quality.py --style-only --base origin/main`.
- Extension: `cd browser-extension && npm test && npm run build`.
- Web UI: `node --test tests/*.test.cjs`.
- End of branch: `RECOG_QUALITY_BASE=origin/main ./scripts/check.sh`.

## Progress Tracking
- Mark completed items with `[x]` immediately when done.
- Add newly discovered tasks with a ➕ prefix.

## Stage 0 — discovery on a live meeting (no commits)

The native application hosts a test meeting, a Chrome tab joins as the second
participant, a phone is the third one and does the talking (two clients on one
Mac double the captured audio, and a two-party meeting may go peer-to-peer).

| Check | Decides | Fallback |
|---|---|---|
| Hosts and paths at `/j/` → prejoin → meeting → leave; full document load on entering `/wc/` | `matches`, id regex | host-wide `https://*.zoom.us/*` plus `isRoomPage()` |
| `iframe#webclient`: `sandbox`, `contentDocument`, document replaced on rejoin | top frame or `all_frames` | separate `content_scripts` entry |
| Loopback permission granted, then `new WebSocket('ws://localhost:8766')` under `upgrade-insecure-requests` | page-world WS works; exact Origin string | stop and report: a content-script relay is a separate architecture |
| `injected.js` loads under `strict-dynamic` | — | `"world": "MAIN"` |
| Markers of prejoin / waiting room / meeting / post-leave | `inCallMarker`, `isInPrejoin()` | named tiles plus the 6 s debounce |
| Which selector tracks the speaker in speaker view, gallery, screen share, pin; stickiness; own tile | order of `activeTile[]`, diagnostic texts | diagnostic asking for speaker view |
| Name and a stable id on a tile; mute class | participant `id`, `micMutedMarker` | name as id; mute `null` |
| Tab hidden for 60 s | background operation | documented limitation |
| `document.title`, meeting topic element | `getMeetingTitle()` | `Zoom <id>` |

- [ ] web checks recorded under "Context"
- [ ] accessibility dump of the native meeting window (experiment E1) recorded

## Stage 1 — Zoom web client

### Task 1: Backend accepts source `zoom`
- [ ] `recog/meeting_storage.py`: `zoom` in `CONFERENCE_SOURCES` and docstrings
- [ ] `recog/conference_hosts.py`: `zoom` in `PLATFORMS`; built-in host
      rejection moved into `_is_builtin_host()` and extended with `zoom.us` /
      `*.zoom.us`
- [ ] tests: canonical URL → `live_id`, `host`, `source`; rejoin inside the
      window reopens the meeting; config rejections; platform lists in
      `tests/test_ktalk_bridge.py` and `tests/test_ktalk_only.py`

### Task 2: Origin wildcard rule as a set of domains (no behavior change)
- [ ] `recog/ws_origin.py`: `WILDCARD_DOMAINS`, helper `_in_wildcard_domain()`,
      `origin_allowed` signature unchanged
- [ ] `tests/test_ws_origin.py`: import and the manifest mirror assertion

### Task 3: Web UI shows the Zoom source
- [ ] `recog/web/static/app.js`: `SOURCE_LABELS.zoom`,
      `COPILOT_CONFERENCE_SOURCES` (the `zoom-export` key stays)
- [ ] `recog/web/static/style.css`: `--source-zoom`, `.source-zoom`
- [ ] test in `tests/frontend_meetings.test.cjs`

### Task 4: `ZoomAdapter`, not wired
- [ ] `browser-extension/src/platforms/zoom.js` after `gmeet.js`:
      `ZOOM_SELECTORS` as the only place with selectors (ordered
      `activeTile[]`, explicit `--active` before the structural main tile,
      unverified entries `null`); pure helpers `meetingIdFromPath`,
      `buildMeetingUrl`, `tileName`; `_docs()` reconciled by identity inside
      `_refresh()`; `isRoomPage()`; `isInMeeting()` on the in-call marker with
      the 6 s window; throttle, signature dedup, 1 s grace on the active tile
      vanishing; mute by positive evidence only; name as participant id unless
      discovery finds a stable one; drift diagnostics; stable conference object
      and state-preserving `dispose()`
- [ ] `browser-extension/src/platforms/zoom.test.js` with fixtures built from
      `ZOOM_SELECTORS`

### Task 5: Wire `*.zoom.us/wc/*` (one commit with the allowlist)
- [ ] `browser-extension/manifest.json`: `host_permissions`,
      `content_scripts[0].matches`, origin-wide `web_accessible_resources`,
      description
- [ ] `recog/ws_origin.py`: `zoom.us` in `WILDCARD_DOMAINS`
- [ ] `browser-extension/scripts/conference-hosts.js`: reserved name and
      built-in host rejection
- [ ] `browser-extension/src/platforms/index.js`: `detectPlatform`,
      `getPlatformAdapter`, `getPlatformDisplayName`
- [ ] tests: `platforms/index.test.js`, `manifest.test.js`,
      `conference-hosts.test.js`, `tests/test_ws_origin.py` lookalikes

### Task 6: Live verification
- [ ] checklist under "Post-Completion"; selector fixes are separate commits
      with their own tests

### Task 7: Documentation (after live verification)
- [ ] `README.md`, `docs/usage.md`, `docs/conference-hosts.md`, `CLAUDE.md`
      (Zoom paragraph with the canonical URL contract and the loopback
      permission), `browser-extension/README.md`, `browser-extension/AGENTS.md`,
      `.revmux/profile.md`

## Stage 2 — native application: experiments and decision

Throwaway code under `.local/`, nothing committed.

| | Experiment | Answers | Needs |
|---|---|---|---|
| E1 | Accessibility dump of the meeting window (speaker view, gallery, window minimized or on another Space) | name and mute on tiles; an active-speaker marker; meeting id and topic | Accessibility permission for the terminal |
| E2 | Zoom Plugin SDK | a speaking event in the SDK headers; ad-hoc signature accepted; attaches to a hand-joined meeting; roster for an ordinary participant; a notice shown to others | General app in the Zoom Marketplace, SDK download, sample build |
| E3 | Packet headers on UDP 8801 | payload type 112 / 99 still mean speech / silence; audio `SSRC >> 10` equals the Plugin SDK user id `>> 10`; a phone participant is distinguishable | one `sudo tcpdump` run by the owner; a three-party meeting |

Decision after the experiments (reported before any application code; the
detailed Stage 3 task list is then written here):
- Roster: Plugin SDK if E2 passes → Accessibility if E1 confirms tiles → none.
- Speaker: a Plugin SDK event if the headers have one → packet headers if E3
  confirms both mappings (needs standing BPF access) → an Accessibility marker
  if E1 finds one → none: the existing sole-unmuted-participant fallback in
  `recog/live.py`, everything else `Unknown`.
- Reachable with any roster: `source=zoom`, title, lifecycle, Copilot and
  increments; speech attributed when exactly one remote participant is unmuted.

## Stage 3 — native application: implementation (skeleton independent of the decision)
- Swift helper under `recog/native/macos/`, built on demand and run as a child
  process like `sck_audio_capture`; JSON lines on stdout. Plugin SDK frameworks
  are never committed; the SDK path is supplied at install time.
- `recog/zoom_app.py`: reads helper lines and drives a no-Origin bridge client.
  Opt-in, macOS only, off by default.
- One source at a time: while a meeting has an owner, speaker and participant
  frames from another client are ignored; the helper stays silent while another
  source owns the meeting.
- Visible status for a missing permission, Zoom not running or a refused
  handshake.
- Linux: the client is Qt with no verified path — out of scope; the web client
  is the recommended route there.

## Post-Completion (manual, live meeting)
- Console shows `Using adapter: zoom`; the bridge log shows `Client connected`
  and no origin rejection.
- No panel on `/j/<id>` and `/wc/home`; the prejoin hint on prejoin; the panel
  in the meeting.
- `meta.json`: `source=zoom`, numeric `live_id`, `host=zoom.us`, `live_url`
  without `pwd`; identical through `app.zoom.us`, `zoom.us/wc/join/<id>` and a
  regional host.
- Remote speech attributed by name in speaker view and gallery; no
  `meeting_leave` during screen share; attribution follows the speaker under
  pin.
- Hidden tab: attribution continues, or the limitation is documented.
- Leave → `meeting_leave` and increments; rejoin within 10 minutes reopens the
  same meeting.
- Copilot starts with `RECOG_COPILOT_AUTOSPAWN=1`; `--ktalk-only` rejects Zoom.
- A deliberately broken selector yields the diagnostic text in the status line.
- Native application: a hand-joined meeting appears with `source=zoom` and the
  same `live_id` as from the web client; the roster matches the Zoom window;
  attribution matches the chosen source; a minimized window or a revoked
  permission gives a visible status and `Unknown` without losing text.

## Risks
- Loopback permission per Zoom origin; `upgrade-insecure-requests` unverified
  until the first grant.
- Selectors come from third-party projects and depend on the layout; gallery
  may have no active marker.
- The native speaker signal may be unreachable without the organizer's
  permission; the outcome is then roster plus sole-unmuted attribution.
- Plugin SDK: signing (no certificates on the machine), developer role in the
  account, behavior for a hand-joined meeting.
- Incomplete roster (virtualized list, speaker view): the sole-unmuted fallback
  may fire on the one visible participant; such segments are marked `uncertain`.
