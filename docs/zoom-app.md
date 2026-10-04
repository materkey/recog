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

The process that runs it needs the **Accessibility** permission (System
Settings → Privacy & Security → Accessibility). Without it the log says so and
no meeting is detected:

```
[zoom-app] no Accessibility permission for this process — Zoom meetings are not detected
```

The log lines `[zoom-app] …` in `live.stderr.log` name the current state: no
permission, Zoom not running, no meeting, meeting detected. Which binary macOS
asks the permission for when live runs as a service has not been checked; the
manual run from a terminal uses the terminal's permission.

The helper `zoom_ax_probe` is compiled on first use from
`recog/native/macos/zoom_ax_probe.swift` (needs `swiftc`, as the system-audio
helper does).

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
