# Repository Guidelines

## Project Overview
This directory contains the KTalk Speaker Detector Chrome extension. It injects UI into KTalk pages and streams speaker events to a local bridge over WebSocket (`ws://localhost:8765`).

## Project Structure & Module Organization
- `src/` holds the editable ES module source (UI, WebSocket, speaker logic, debug panel).
- `src/index.js` is the webpack entry point; the build outputs the complete unpacked extension under `dist/`.
- `content.js` injects `injected.js` into KTalk pages; `popup.html`/`popup.js` power the extension popup.
- `manifest.json` (MV3) and `icons/` define extension metadata and assets.
- `dist/` is generated — do not edit directly.

## Build, Test, and Development Commands
- `npm install` installs webpack dependencies (first run only).
- `npm run build` compiles `src/` into `dist/injected.js` and packages the manifest and assets in `dist/`.
- Reload after changes: `chrome://extensions` → enable Developer Mode → **Reload**.

## Coding Style & Naming Conventions
- JavaScript uses ES modules, 2-space indentation, semicolons, and `camelCase` for functions/vars.
- Keep UI/data modules separated (e.g., `ui.js`, `ws.js`, `speaker.js`) and prefer named exports.
- Avoid direct edits to bundled files (`injected.js`) and keep `manifest.json` in sync with assets.

## Testing Guidelines
- Run `npm test -- --runInBand` for the automated suite.
- Manual smoke checks: run `npm run build`, reload the extension, join a KTalk meeting, and confirm console logs (e.g., `[KTalk Transcript]`) plus live WebSocket traffic to `ws://localhost:8765`.

## Commit & Pull Request Guidelines
- Recent commits use `scope: short description` (e.g., `frontend: add date picker filter`). Follow that pattern with concise, imperative phrasing.
- PRs should include a brief summary, verification steps (build + reload + KTalk check), and screenshots for UI changes.

## Configuration & Runtime Tips
- The tracked manifest includes KTalk, `meet.vas3k.cloud` and `meet.google.com` (Google Meet, host-wide; the adapter joins only on a room path). Optional `.local/conference-hosts.json` at the repository root (or `RECOG_CONFERENCE_HOSTS_FILE`) adds exact Jitsi and Matrix origins when building; see README. Keep private origins out of tracked files and load `dist/` in Chrome.
- Ensure the local bridge (`ktalk_bridge.py`) is running so WebSocket events resolve during manual testing.
