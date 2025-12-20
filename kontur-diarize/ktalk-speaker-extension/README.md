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
