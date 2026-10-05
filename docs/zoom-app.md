# Zoom application (macOS)

The Zoom web client is covered by the Chrome extension. The Zoom application is
a native program the extension cannot see, so a separate source reads its
Accessibility tree and reports the meeting to the bridge as source `zoom`:
participants, mute state and the active speaker. Recording, attribution,
Copilot and increments then work as for any other conference.

Nothing is installed into Zoom and nothing is visible to the meeting organizer
or the other participants. No Zoom account setup is involved.

## Enable

It is off by default. Add to `.env` in the checkout and restart:

```
RECOG_ZOOM_APP=1
```

```bash
./recog-service.sh restart
```

`live --zoom-app` does the same for a manual run. For a check without the live
pipeline, run `python -m recog.zoom_app` while the live service is up.

### Accessibility permission

The Accessibility tree is read by a small helper application, **Recog Zoom
Helper** (`RecogZoomHelper.app` in the checkout, built on first use from
`recog/native/macos/zoom_ax_probe.swift`; needs `swiftc`). The permission
belongs to that application, not to Python or the terminal, so it is granted
once and is the same for the service and for a manual run.

On first start macOS shows "Recog Zoom Helper would like to control this
computer using accessibility features". Choose **Open System Settings** and
switch on **Recog Zoom Helper** under Privacy & Security → Accessibility.
The helper notices the grant without a restart. Until then the log says:

```
[zoom-app] "Recog Zoom Helper" has no Accessibility permission — Zoom meetings are not detected (System Settings → Privacy & Security → Accessibility)
```

The log lines `[zoom-app] …` in `live.stderr.log` name the current state: no
permission, Zoom not running, no meeting, meeting detected.

**When the permission has to be granted again.** macOS recognizes the helper by
its code signature. By default the helper is signed ad hoc, and that signature
changes whenever the helper is rebuilt — which happens only when its Swift
source changes, not on a Python upgrade, a new virtual environment or a
checkout that leaves the file's content alone. After such a rebuild remove the
old "Recog Zoom Helper" entry in the Accessibility list and switch on the new
one.

To keep the grant across rebuilds, sign the helper with a certificate of your
own: set `RECOG_CODESIGN_IDENTITY` in `.env` to a code-signing identity from
your keychain (`security find-identity -v -p codesigning`), for example
`Developer ID Application: …`. This has not been exercised: no such identity
was available where the helper was developed.

## What to expect

Checked on Zoom Workplace 7.1.9 with a scripted test meeting, gallery view,
video off:

- Remote speech is attributed to the participant Zoom marks as the active
  speaker. The mark stays on the last speaker during silence.
- The meeting is detected with the window on another Space and with the
  participants panel closed. In minimal view only the current speaker is
  visible; the roster seen before is kept.
- Leaving is reported once the meeting window has been gone for six seconds.

Limits:

- **English Zoom interface only.** Mute and speaker state are read from the
  tile description in the interface language. Another language yields the
  roster but no speaker; speech is then `Unknown`, never guessed.
- **No meeting number.** The application does not expose it, so every
  application meeting is recorded under `https://zoom.us/app`. Rejoining within
  ten minutes continues the same recording; two different meetings inside that
  window continue it too. A meeting joined through the web client is a separate
  recording.
- **Title only if seen.** The topic is readable while the meeting toolbar is
  shown; otherwise the recording has no title.
- **Namesakes merge** into one participant.
- **One source at a time.** While a browser conference owns the recording, the
  application source stays silent and retries every 30 seconds.
- Not exercised: speaker view, screen share, large meetings with several pages
  of tiles, breakout rooms.
- macOS only. On Linux use the Zoom web client.
