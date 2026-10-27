# Conference hosts

KTalk, `meet.vas3k.cloud`, Google Meet (`meet.google.com`) and the Zoom web
client (`*.zoom.us/wc/`) are built in.
Other Jitsi and Element Call servers
are installation settings, stored in the
[installation configuration](configuration.md) `~/.config/recog/config.toml`:

```toml
[conferences]
jitsi = ["https://meet.example.org"]
matrix = ["https://element.example.org"]
```

Use exact lowercase HTTPS origins without a trailing slash, port, path,
credentials, query or wildcard. Each origin belongs to one platform. Do not
repeat built-in hosts. A missing file enables only built-in hosts; malformed
configuration fails startup/build rather than granting broader permissions.

Both Python services and the extension build read this file. To use another
location, pass `RECOG_CONFIG_FILE` in both processes' environments;
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
and remove the old registration. Do not use a symlink to migrate a registration:
Chrome resolves the target path and derives an unpacked extension's ID from it.
Reloading the old registration can install a second ID while the old card stays
at `Reloading…`. In that case, keep the registration loaded directly from
`browser-extension/dist/` and remove the old card; then refresh the conference tab.

Jitsi runs on its configured hosts. Element Call injects only into the bundled
`/widgets/element-call/*` iframe, never the top-level Element page. The meeting
URL uses that iframe's origin and the Matrix room ID. Backend WebSocket checks
allow the same exact configured origins. `RECOG_EXTRA_WS_ORIGINS` remains
available for backend-only integrations; it does not change extension injection.
Google Meet needs no configuration: the extension runs on every
`meet.google.com` page and joins a meeting only on a room path
(`/abc-defg-hij`, or `/meet/abc-defg-hij` when opened from an outside link).
The Zoom web client needs no configuration either: the extension runs on
`/wc/` paths of `zoom.us` and its subdomains and joins a meeting once you are
past prejoin and the waiting room. The first time, Chrome asks whether
`app.zoom.us` may connect to devices and services on this computer; accept
it, otherwise the transcript panel stays disconnected. The recording stores
the meeting as `https://zoom.us/j/<meeting number>` without the password.

## Existing recordings

New Jitsi meetings use the platform identifier `jitsi`. If an installation
previously used a custom source label, add a local alias:

```toml
[conferences]
jitsi = ["https://meet.example.org"]

[sources.aliases]
old-meet = "jitsi"
```

Aliases map nonstandard labels to `jitsi` or `matrix`. Platform identifiers
(`ktalk`, `vas3k`, `jitsi`, `matrix`, `gmeet`, `zoom`) are reserved and cannot
be alias names. Aliases preserve conference
lifecycle checks and same-day reconnect matching, and present canonical source
names in the web UI. Reading old recordings does not rewrite their metadata.
Aliases are not compiled into the extension. Keep this file local; share only
generic examples when documenting an installation.
