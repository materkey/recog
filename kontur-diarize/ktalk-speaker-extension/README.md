# KTalk Speaker Detector (extension)

This folder contains the Chrome extension that injects the KTalk transcript UI and sends speaker data to `ktalk_bridge.py`.

## Build (required after edits)

```bash
npm install
npm run build
```

This compiles `src/` into the injected bundle `injected.js` used by the extension.

## Reload in Chrome

1) Open `chrome://extensions`
2) Enable Developer Mode
3) Click **Reload** on the extension

## Source layout

- `src/` — editable sources (modules)
- `injected.js` — built bundle loaded by the content script
- `content.js` — injects `injected.js` into the KTalk page

## Notes

- If you change `src/`, re-run `npm run build`.
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
cd kontur-diarize/ktalk-speaker-extension
npm run build

# 3. Reload the extension in chrome://extensions.
```

A healthy session shows `[PcmTransport] Attached participant <pid>` in the Chrome console and `pcm_chunk received from <pid>` every ~3 seconds in `ktalk_bridge.py` logs. See `docs/plans/2026-04-22-per-speaker-asr-pipeline.md` for the full decisions table (chunk cadence, batching, watchdog fallback).
