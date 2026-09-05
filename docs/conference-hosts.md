# Conference hosts

KTalk and `meet.vas3k.cloud` are built in. Other Jitsi and Element Call servers
are installation settings, stored in the gitignored `.local/conference-hosts.json`
at the repository root:

```json
{
  "jitsi": ["https://meet.example.org"],
  "matrix": ["https://element.example.org"]
}
```

Use exact lowercase HTTPS origins without a trailing slash, port, path,
credentials, query or wildcard. Each origin belongs to one platform. Do not
repeat built-in hosts. A missing file enables only built-in hosts; malformed
configuration fails startup/build rather than granting broader permissions.

Both Python services and the extension build read this file. To use another
location, pass `RECOG_CONFERENCE_HOSTS_FILE` in both processes' environments;
relative paths are resolved against the repository root, and `~/` is supported.
The extension build reads process environment, not the application's `.env`.

After changing the host list:

```bash
npm run build --prefix browser-extension
./recog-service.sh restart
```

Load or reload **`browser-extension/dist/`** in `chrome://extensions`.
The generated manifest and bundle contain the installation's configured hosts;
`dist/` is ignored by Git. The tracked manifest remains a generic template.
If Chrome still registers the source directory, use Load unpacked with `dist/`
and remove the old registration. A registration pointing at a maintained local
compatibility link can continue using that link when it points at `dist/`.

Jitsi runs on its configured hosts. Element Call injects only into the bundled
`/widgets/element-call/*` iframe, never the top-level Element page. The meeting
URL uses that iframe's origin and the Matrix room ID. Backend WebSocket checks
allow the same exact configured origins. `RECOG_EXTRA_WS_ORIGINS` remains
available for backend-only integrations; it does not change extension injection.

## Existing recordings

New Jitsi meetings use the platform identifier `jitsi`. If an installation
previously used a custom source label, add a local alias:

```json
{
  "jitsi": ["https://meet.example.org"],
  "matrix": [],
  "source_aliases": {"old-meet": "jitsi"}
}
```

Aliases map nonstandard labels to `jitsi` or `matrix`. They preserve conference
lifecycle checks and same-day reconnect matching, and present canonical source
names in the web UI. Reading old recordings does not rewrite their metadata.
Aliases are not compiled into the extension. Keep this file local; share only
generic examples when documenting an installation.
