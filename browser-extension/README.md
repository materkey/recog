# KTalk Speaker Detector (extension)

This folder contains the Chrome extension that injects the transcript UI and sends speaker data to `ktalk_bridge.py`.

## Supported platforms

`detectPlatform()` (`src/platforms/index.js`) selects a per-platform adapter (`src/platforms/*.js`) from the tab URL:

- **KTalk** (`ktalk.js`) — injects into the KTalk tab; per-speaker PCM transport (ADR-0018) supported.
- **Jitsi** (`vas3k.js`) — built-in `meet.vas3k.cloud` and configured Jitsi deployments; floating panel (transcript + copilot chat).
- **Matrix / Element Call** (`matrix.js`) — see below; floating panel, mixed-audio path.

### Element Call (matrix)

Element Call is bundled inside Element Web as a **same-origin iframe** at `<configured-origin>/widgets/element-call/index.html` (the standalone `call.element.*` host is not used for room calls). The manifest scopes the Element content-script match to `/widgets/element-call/*` with `"all_frames": true`, so the extension injects **only into the EC iframe, never the Element top frame** — a top-frame injection would open a stray second WS to the bridge and break last-extension-gone meeting-end detection.

`ElementCallAdapter` sources data from Element Call internals:

- Lifecycle / participants / display names from page globals `window.rtcSession` (MatrixRTCSession) and `window.matrixclient` (matrix-js-sdk). Names resolve via `matrixclient.getRoom(roomId).getMember(userId).name` (`participant.name` is empty in Element Call).
- Active-speaker events from a React fiber-walk (`__reactFiber$*` on an `<audio>` element, walk `.return` up to the livekit `Room`); participants/names still work from the globals alone if the Room isn't found yet.
- Participant identity is the legacy plain format `${userId}:${deviceId}` (e.g. `@test2:matrix.example.org:IZFUKTQLPX`); `parseRtcIdentity()` splits it, with a sha256-hash fallback for future MatrixRTC deploys.
- The meeting URL is a **synthesized** `<current-page-origin>/#/room/<roomId>` (optional `getMeetingUrl()` adapter hook) so the backend derives a stable `live_id` from the room id instead of `index.html`.

Per-participant PCM + refine (Tier 2) is out of scope; the mixed-audio path + `speaker_update` signals provide attribution, same as the Jitsi platforms. After any element-web server upgrade, re-run the console probe in the plan's Post-Completion section to confirm globals + fiber props survived. See `docs/plans/completed/20260709-element-call-matrix-platform.md`.

## Build (required after edits)

```bash
npm install
npm run build
```

This creates the complete unpacked extension in `dist/`: the injected bundle,
manifest, popup, icons and audio worklet. The tracked `manifest.json` is a generic
template; personal origins appear only in ignored build output.

KTalk and `meet.vas3k.cloud` work without configuration. For another Jitsi or
Element deployment, create `.local/conference-hosts.json` at the repository root:

```json
{
  "jitsi": ["https://meet.example.org"],
  "matrix": ["https://element.example.org"]
}
```

Alternatively set `RECOG_CONFERENCE_HOSTS_FILE` in the build shell to a JSON
file path (`~/` is expanded; relative paths resolve from the repository root). Use canonical HTTPS
origins with lowercase ASCII DNS labels and no trailing slash, path, query, fragment, credentials, wildcard or
explicit port. Origins must be unique across platforms and must not repeat the
built-in hosts. A missing file means no custom hosts; malformed configuration
fails the build. Optional `source_aliases` maps old stored source IDs to `jitsi`
or `matrix` for the backend and is not included in the browser bundle.

The same file configures the Python bridge's allowed conference origins. Rebuild
and reload the extension and restart Recog after changing it.

## Reload in Chrome

1) Open `chrome://extensions`
2) Enable Developer Mode
3) First installation or migration: **Load unpacked** → `browser-extension/dist/`; remove the previous registration if it points at the source directory.
4) Subsequent builds: click **Reload** on the extension

## Source layout

- `src/` — editable sources (modules)
- `src/platforms/` — per-platform adapters (`ktalk.js`, `vas3k.js`, `matrix.js`) + `index.js` (`detectPlatform`)
- `dist/` — complete generated extension; `dist/injected.js` is the built bundle
- `content.js` — injects `injected.js` into the conference page (KTalk top frame or the scoped Element Call iframe)

## Notes

- If you change `src/`, re-run `npm run build`.
- Run `npm test -- --runInBand` for the automated suite.
- The extension communicates with the local bridge at `ws://localhost:8765`.

## PCM transport (per-speaker ASR, ADR-0018)

When the Python bridge is started with `RECOG_PCM_TRANSPORT=1`, the extension attaches an `AudioWorkletNode` to every remote participant's `MediaStream`, downsamples 48 kHz → 16 kHz Int16 PCM, and streams chunks over the existing WS with `type: "pcm_chunk"`. This lets GigaAM transcribe each speaker independently and removes overlap-misattribution on the server side.

The gate is **server-authoritative**:

- Bridge reads `RECOG_PCM_TRANSPORT` once at startup and sends `{"type": "pcm_transport_enabled", "enabled": bool}` to every WS-connected extension on handshake.
- Extension keeps an in-memory flag in `src/ws_api.js` and checks it before every chunk emit. On `true → false` transitions the worklets are detached automatically (`pcmTransportDispose()`), so there is nothing to toggle in the popup.
- There is intentionally **no** `chrome.storage` toggle for this. One source of truth avoids silent-mismatch combinations (popup-off + env-on = muted meeting; popup-on + env-off = wasted uplink).

To enable end-to-end locally:

```bash
# 1. Flip the server-side flag and restart the bridge + live pipeline.
RECOG_PCM_TRANSPORT=1 ./recog-service.sh restart

# 2. Rebuild the extension if you changed src/.
cd browser-extension
npm run build

# 3. Reload the extension in chrome://extensions.
```

A healthy session shows `[PcmTransport] Attached participant <pid>` in the Chrome console and `pcm_chunk received from <pid>` every ~3 seconds in `ktalk_bridge.py` logs. See `docs/plans/2026-04-22-per-speaker-asr-pipeline.md` for the full decisions table (chunk cadence, batching, watchdog fallback).
