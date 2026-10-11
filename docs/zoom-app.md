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
Nothing has to be restarted afterwards: the grant is picked up within about
15 seconds. (A running helper cannot see it — macOS answers from a per-process
cache — so, while the permission is missing, the helper exits and a fresh one
is started every 15 seconds; only the first start shows the dialog.) Until
then the log says:

```
[zoom-app] "Recog Zoom Helper" has no Accessibility permission — Zoom meetings are not detected (System Settings → Privacy & Security → Accessibility)
```

The log lines `[zoom-app] …` in `live.stderr.log` name the current state: no
permission, Zoom not running, no meeting, meeting detected.

**When the permission has to be granted again.** macOS recognizes the helper by
its code signature. By default the helper is signed ad hoc, which makes the
signature a hash of the helper itself. It stays the same across a Python
upgrade, a new virtual environment, a checkout, and a rebuild from unchanged
code — two builds of the same source were checked to carry the same signature.
It changes when the helper's code changes (an update of recog that touches
`zoom_ax_probe.swift` beyond comments, or a new Xcode that compiles it
differently). Then the log reports the missing permission again: remove the
old "Recog Zoom Helper" entry in the Accessibility list and switch on the new
one.

A signature that survives code changes needs a certificate macOS trusts for
code signing: set `RECOG_CODESIGN_IDENTITY` in `.env` to its name
(`security find-identity -v -p codesigning` lists what you have), for example
`Developer ID Application: …` from an Apple Developer account. A self-signed
certificate works only after you mark it trusted for code signing in Keychain
Access; without that `codesign` rejects it ("no identity found"). recog does
not create such a certificate for you: its key would let any program running
as you sign a look-alike helper and inherit the Accessibility permission.

## What to expect

Checked on Zoom Workplace 7.1.9 with a scripted test meeting, gallery view,
video off:

- Remote speech is attributed to the participant Zoom marks as the active
  speaker. The mark stays on the last speaker during silence.
- The meeting is detected with the window on another Space and with the
  participants panel closed, in gallery view and in speaker view, and while
  someone shares a screen.
- The meeting starts with the first participant tile: the preview shown
  before "Start" is not yet a meeting.
- Only the tiles on screen are readable: one in minimal view, a page of them
  in a small window or a large meeting. Zoom keeps the current speaker among
  them, so the speaker is still found; the participant list is what has been
  seen so far and is not trimmed when someone leaves.
- Leaving is reported once the meeting window has been gone for six seconds.

On a live meeting hosted on another account six of the seven tiles, the local
one included, named their participant as "View <name>'s profile" instead of
the bare name the test meetings showed. Both forms are read; what makes Zoom
choose one was not established.

Limits:

- **Interface language.** The tile description is worded in the Zoom interface
  language. The mute wording is read from the installed Zoom's own
  localization files, so every language it ships is covered; the speaker mark
  is the literal `active speaker` in all of them. Checked live in English; in
  Russian the mute wording was observed on a live meeting, the speaker mark
  was not (it has no entry in Zoom's localization files).
- **No meeting number.** The application does not expose it, so every
  application meeting is recorded under `https://zoom.us/app`. Rejoining within
  ten minutes continues the same recording; two different meetings inside that
  window continue it too. A meeting joined through the web client is a separate
  recording.
- **Generated title.** The application offers no reliable topic: the one
  element that carries it showed the free-plan timer ("Meeting  40-minutes")
  on a live meeting. When the meeting ends, the recording is titled from its
  transcript, as recordings outside a conference are.
- **Namesakes merge** into one participant.
- **One source at a time.** While a browser conference owns the recording, the
  application source stays silent and retries every 30 seconds.
- Not exercised: gallery view with more participants than one page holds
  (over 25): the speaker is found only while on the visible page. In speaker
  view Zoom brings the speaker onto the screen, so prefer it in large
  meetings. Breakout rooms were not exercised either.
- macOS only. On Linux use the Zoom web client.
