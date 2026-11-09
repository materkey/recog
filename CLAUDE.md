# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Recog** is a Russian speech recognition system with live speaker identification via KTalk. It combines:
- **GigaAM** (v3_e2e_rnnt) for Russian ASR
- **Conference participant signals** for speaker identification

## Repository layout

Start with [the documentation index](docs/README.md) and
[the repository layout](docs/repository-layout.md). The code is the Rust workspace
`rust/` (one crate per former Python module group, docs/plans/2026-11-07-rust-migration.md);
`recog/` keeps what is not Rust: the static web UI (`recog/web/static/`), the
prompts (`recog/prompts/`), the Swift sources (`recog/native/`), the desktop
bootstrap script. The optional ASR server is the crate `rust/recog-asr-server`. The
Chrome extension is `browser-extension/`; OS service templates are under
`deploy/{macos,linux}/`, and benchmarks under `scripts/benchmarks/`.

`./setup.sh` builds the programs into `bin/` and the units run them from there;
after a change of the units run `./recog-service.sh install` followed by
`./recog-service.sh restart`. Keep the generated `sck_audio_capture` binary at
the root for installed-machine compatibility. Local sample recordings belong in gitignored `.local/examples/`;
meeting data uses `get_data_dir()` below. Never edit existing environment files.

BMAD artifacts live in `docs/bmad/`; `_bmad-output` is a relative compatibility
symlink to that directory. Keep `_bmad/` and tool configuration paths where their
tools expect them. `AGENTS.md` is a symlink to this file.

## Changelog

`CHANGELOG.md` lists, by date, what an update asks of an existing installation
(moved or renamed settings, files or units, new dependencies, a setup or service
step, stricter validation) and behaviour users would notice. A commit that
introduces such a change adds its entry in the same commit, under today's date,
with what to do after `git pull`; internal refactors and fixes without a visible
effect do not.

## Commands

### Preparing a revmux review

The caller preparing each review round must generate quality measurements before
launching revmux; read-only reviewers consume the reports and do not run the gate.
Read `docs/quality.md` for dependencies and policy, fetch the review's target ref,
then run from the repository root (replace `origin/main` with that target):

```bash
python3 scripts/quality.py --base origin/main --output .local/quality
```

Copy `report.md` and `report.json` from the exact `run-*` directory printed by this
invocation into the round's `input/context/`, and identify them in the review context.
Regenerate after changes; never substitute an older successful report for a failed
measurement. Resolve gate failures before review. If measurement cannot complete,
record the error and explicitly report metrics as unverified instead of claiming
the quality check passed. See `.revmux/profile.md` for reviewer interpretation.

### Setup
```bash
# Full setup on macOS and Linux: cargo builds the programs of rust/ (release)
# into bin/, then config files, extension build, Swift helpers, the ASR model
# and the service units. Idempotent — this is also how you update after a git
# pull. Needs cargo (rustup), no Python.
./setup.sh

# Flags: --no-services | --no-prefetch | --no-extension | --no-env (keep
# existing environment files, do not create them from the examples)
# Environment files are created on a first install only: once the service
# units exist, a missing .env stays missing (the examples are not neutral).
# Call-shell env (NOT .env): CARGO_TARGET_DIR, RECOG_SKIP_COPILOT_CHECK.
```

`cargo build --release` builds `recog`, `recog-web`, the two Copilot binaries,
`recog-desktop` (and `recog-mic` on macOS); `recog check` by the fresh binary
checks the configuration before anything is installed; the programs are then
copied into `<repo>/bin/` (gitignored, outside `rust/target`, which a cargo cache
cleaned twice under running services; `recog_core::install` makes a binary there
take that checkout as its own), each renamed over the old one. The scripts ask
`recog` what they need: `recog check` (`deps_asr_config_check`), `recog config
data-dir|config-path|conferences|install-runtime|copilot-launch`, `recog launcher`
(Recog.app), `recog prefetch` (the wavo model), `recog desktop …` (execs
`recog-desktop`). `[install] runtime` is "rust"; `scripts/deps.sh` reads it itself
(calling shell or a plain line in config.toml, never `.env`) and "python", the
removed installation, stops setup. The per-service `runtime` keys and `[live]
mic_worker` are still accepted and do nothing.

### Running
```bash
# Live with the conference Chrome extension + system audio (backend per OS)
bin/recog live --ktalk --system-audio auto

# Explicit backends: screencapture (macOS) | pulse (Linux) | device:<id> | none
bin/recog live --system-audio pulse

# List audio devices (on Linux also prints `pactl list short sources`)
bin/recog live --list-devices
```

`--screencapture` and `--system-device <id>` remain as hidden aliases of
`--system-audio screencapture` / `--system-audio device:<id>`.

### Frontend (web UI)
```bash
# Normally the frontend runs as a service unit; this is only for debugging.
bin/recog-web --port 9080  # http://127.0.0.1:9080
```

### Services (launchd / systemd --user)
```bash
./recog-service.sh install            # Render + register the 4 units (live + frontend + copilot-channel + copilot-supervisor) WITHOUT starting them
./recog-service.sh start              # Start all installed services (readiness-gated)
./recog-service.sh status             # Show service status
./recog-service.sh logs               # Tail live transcription logs
./recog-service.sh restart            # Restart after code changes
./recog-service.sh restart-supervisor # Restart copilot supervisor daemon only
./recog-service.sh logs-supervisor    # Tail copilot supervisor daemon logs
./recog-service.sh render --dry-run D # Render units into D only (tests/CI)
```

The units' commands come from `svc_command` in `scripts/service/common.sh`
(`@COMMAND@` in the templates): `bin/recog live …`, `bin/recog-web --host
127.0.0.1 --port 9080 --repo <repo>`, `bin/recog-copilot-* --repo-root <repo>`.

On macOS the live unit's program is `Recog.app/Contents/MacOS/recog_launcher`
(bundle id `com.recog.live`, source `recog/native/macos/recog_launcher.swift`,
built by `setup.sh` through `bin/recog launcher` with recog-zoom's bundle builder,
sha256 stamp `Recog.source-sha256` beside the bundle), which runs `bin/recog` as
its child: macOS attributes Screen Recording and the microphone to the job's
program, so the permissions belong to "Recog". `scripts/deps.sh` refuses to start
without it.

The backend is chosen from `uname -s` (`RECOG_SERVICE_OS` overrides it, which
only changes which templates/verbs are used, so a Mac can render Linux units):
launchd on macOS, `systemd --user` on Linux. Units are rendered from
`deploy/{macos,linux}/*.in` by `scripts/service/{common,launchd,systemd}.sh`
behind the `svc_*` contract. `install` never activates anything — that split is
what lets `setup.sh` verify dependencies before starting a pipeline that cannot
serve; `start`/`restart`/`restart-*` run the same probes (`scripts/deps.sh`) and
refuse to activate an incomplete install rather than stop a working one
(`RECOG_SKIP_READINESS=1` bypasses the gate). On Linux the
services need `loginctl enable-linger $USER` to survive logout (`status` prints
the reminder).

**Important**: After changing Rust code, run `./setup.sh --no-extension` (it
rebuilds and reinstalls bin/) and `./recog-service.sh restart`.

Manual copilot launch (`./recog-service.sh claude`) stays available for debugging; it now passes the same `--strict-mcp-config --mcp-config` flags the supervisor uses, so the `copilot` MCP server is registered.

### Chrome Extension (KTalk speaker detection)
```bash
cd browser-extension
npm install        # First time only
npm run build      # Rebuild after changes to src/
```
After rebuild, reload in Chrome: `chrome://extensions` → Developer Mode → Reload.
For an existing checkout whose old extension path is unavailable, use Load unpacked
with `browser-extension/dist/` and remove the stale registration.

The extension connects to `ws://localhost:8766` (recog-bridge, the one server subscribers, the supervisor and the Copilot channel also use) and sends speaker signals from conferences on multiple platforms: KTalk, Jitsi (`vas3k`/`jitsi`), Matrix/Element Call (`matrix`), Google Meet (`gmeet`), and the Zoom web client (`zoom`). Each platform has an adapter in `src/platforms/` (`ktalk.js`, `vas3k.js`, `matrix.js`, `gmeet.js`, `zoom.js`) selected by `detectPlatform()` in `src/platforms/index.js`; the tracked `manifest.json` contains built-in hosts only; webpack adds the installation hosts from `[conferences]` of the installation configuration (`~/.config/recog/config.toml`, parsed only by `rust/recog-core/src/install_config.rs`, which the build asks through `bin/recog config conferences`; `docs/configuration.md`) to `dist/manifest.json`. Load `browser-extension/dist/` in Chrome. See `docs/conference-hosts.md` for shared host and legacy source configuration.

**Element Call (matrix)** — Element Call is bundled inside Element Web as a same-origin iframe (`https://element.example.org/widgets/element-call/index.html`); the standalone `call.element.*` host is unused for room calls. The manifest scopes the Element match to `/widgets/element-call/*` with `"all_frames": true`, so the content script injects **only into the EC iframe, never the Element top frame** (a top-frame injection would open a stray second WS that outlives the call). `ElementCallAdapter` (`src/platforms/matrix.js`) sources lifecycle/participants/names from page globals `window.rtcSession` (MatrixRTCSession) + `window.matrixclient` (matrix-js-sdk), and active-speaker events from a React fiber-walk (`__reactFiber$*` → `.return` up to the livekit `Room`). Participant identity is the legacy plain format `${userId}:${deviceId}` (e.g. `@test2:matrix.example.org:IZFUKTQLPX`), parsed by `parseRtcIdentity()` with a sha256-hash fallback for future MatrixRTC. Display names come from `matrixclient.getRoom(roomId).getMember(userId).name`. The meeting URL is a **synthesized** `https://element.example.org/#/room/<roomId>` (via the optional `getMeetingUrl()` adapter hook) so the backend derives a stable `live_id` from the room fragment instead of `index.html`. Matrix uses the same mixed-audio path + floating panel (transcript + copilot chat view) as the Jitsi platforms; per-participant PCM/refine (Tier 2) is out of scope. See ADR/plan `docs/plans/completed/20260709-element-call-matrix-platform.md`.

**Google Meet (gmeet)** — built-in host `meet.google.com`; the content script runs host-wide because Meet navigates from `/`, `/home`, `/landing` into a room (`/abc-defg-hij`, which becomes the `live_id`) without a reload; a room opened from an outside link (Calendar) stays on `/meet/abc-defg-hij`, and `buildMeetingUrl()` reports both forms as `https://meet.google.com/<code>`, so they reopen one meeting. Meet exposes no conference object, so `GoogleMeetAdapter` (`src/platforms/gmeet.js`) reads the DOM, with every selector in `GMEET_SELECTORS`: tiles `[data-participant-id]` (name = the tile's name tag `span.notranslate`, never the tile text, which also holds icon names of the hover controls and the paused-video notice) and the speaking indicator `[jsname="QgSmzd"]`, where speaking is **positive evidence only** (a volume-level class from `speakingClasses`; the silent class or an unknown class counts as silent). With the tab hidden Meet moves the tiles into a Document Picture-in-Picture window, so the adapter observes both documents, and `isInMeeting()` is time-debounced (~6 s without tiles on the same room path) so the PiP handover does not send `meeting_leave`. Two optional adapter hooks, absent on other platforms: `isRoomPage()` (false off a room path → neither `init.js` nor an early bridge frame creates the floating panel, and the conference wait is not rescheduled) and `getDiagnostic()` (selector drift during a call → text in the floating-panel status line that takes precedence over the speaker text, speaker attribution suspended). The user's own microphone comes from the toolbar's microphone button `[jsname="hw0c9"]` (the camera's is `psRWwc`), whose `data-is-muted` is not localized, unlike its `aria-label`; the optional `getLocalMicMuted()` hook (shared with Zoom) reports it as `localMicMuted`, so the bridge does not depend on the own tile, which is found only by the `Reframe` control and is not always on screen. Meet enforces Trusted Types, so floating-panel HTML goes through `toTrustedHtml()` in `src/utils.js`. The transcript toggle and the Debug button dock into the bottom bar's right column (`getControlsDock()`: the next sibling of the call controls region `[jsname="vNB5le"]`, after the chat / meeting tools nav) instead of floating over the side panel's message box. Tier 1 only: Meet forwards a few "loudest speaker" streams rather than one per participant, and `RTCPeerConnection` is frozen before the content script runs, so there is no per-participant PCM/refine. See `docs/plans/completed/2026-09-26-google-meet-platform.md`.

**Zoom web client (zoom)** — built-in, `https://*.zoom.us/wc/*`, top frame only (the client has no iframe; every entry URL lands on `app.zoom.us/wc/<id>/join`, and prejoin → waiting room → meeting happen without a document load or a path change). Zoom exposes no conference object, so `ZoomAdapter` (`src/platforms/zoom.js`) reads the DOM, with every selector in `ZOOM_SELECTORS`: tiles of the speaker-view strip, the main tile and the gallery; the display name is the participant id (tiles carry none, namesakes merge); the speaker is the tile with an explicit `--active` class, which Zoom keeps on the last speaker during silence and never puts on the local participant — the structural main tile is only the last resort, since under pin it shows the pinned participant. "In the meeting" is entered on the `#wc-footer` toolbar **together with a participant tile** — Zoom mounts the toolbar alone for a second or two right after "Join", before the waiting room — and is then kept by the toolbar alone (tiles can vanish), debounced ~6 s. The meeting URL is **synthesized** as `https://zoom.us/j/<id>` (`getMeetingUrl()`): the backend takes the last path segment as `live_id`, which would otherwise be `join` for every meeting, and every Zoom source must report this same canonical form so they reopen one meeting. `zoom.us` is a wildcard domain in `rust/recog-core/src/platforms.rs` next to `ktalk.ru`. The user's own microphone comes from the toolbar's audio button, kept in the DOM while the toolbar is hidden: its `aria-label` is an untranslated client literal (`mute my microphone` → open, `unmute my microphone` / `join audio` → closed, as are `audio settings` / `audio setting` / `audio`, which the client shows when the user cannot be heard — not allowed to talk, or the microphone disabled; anything else → unknown, logged once), and `getLocalMicMuted()` reports it as `localMicMuted` (see the bridge below); unknown leaves it to the own tile, and with no tile of the user's own (a webinar attendee) it counts as closed — only when the user's name is known (typed on prejoin) and the toolbar has no audio button of unknown wording; otherwise the tiles decide. Chrome asks once per origin before a page may connect to localhost; until that prompt is accepted for `app.zoom.us` the panel shows the speaker but the bridge receives nothing. Tier 1 only. See `docs/plans/2026-10-04-zoom-platform.md`.

**Zoom application (zoom, macOS, opt-in)** — the native application is AppKit, not a web page, so neither the extension nor `recog/desktop/` (Electron-only) applies. `rust/recog-zoom` runs a Swift helper (source `recog/native/macos/zoom_ax_probe.swift`) that polls the application's Accessibility tree and writes JSON snapshots. **The helper only reports the tree** — every described `AXTabGroup` with the role, description and value of its children, the meeting window title, the account button, and during a meeting the titles of the menu bar's items as paths — because any change of its code changes its signature and costs the user the Accessibility grant; what a tab group means is decided in recog-zoom (`tile_name()`, `parse_tile()`), following the code that words the tree in Zoom's `zVideoUI` binary (`docs/zoom-app.md`, "How the tree is read"). A participant tile has a description such as `Alice, Computer audio unmuted, Video off, active speaker` and a name-tag button described by the display name or by `View <name>'s profile`; a tab group that words a participant's state but names nobody is left out and logged once per meeting (`ZoomAppSession.unread`). `ZoomAppSession` (pure, no I/O) turns snapshots into ordinary bridge frames (`meeting_join` with `platform: "zoom"`, `participants_update`, `speaker_update`, `meeting_leave`; no title — the toolbar's info button, the only element resembling a topic, showed the free-plan timer on a live meeting, so the helper does not read it and the ended recording gets an AI title like an orphan: `_wants_generated_title()` in `rust/recog-core/src/meeting_storage/` covers any conference that brought none) sent over a socket **without an Origin header**, so the bridge needs no Zoom-specific code and treats the source as an extension client (its disconnect ends the meeting). The application exposes no meeting number, so the URL is the constant `https://zoom.us/app` (`live_id=app`), separate from the web client's `https://zoom.us/j/<id>`. Descriptions are in the Zoom UI language: `load_vocabulary()` reads the audio wording for every language from the installed Zoom's own `*.lproj/Localizable.strings` (keys `LN_Plist_Accessibility_{VoIP,Tele}_{Muted,Unmuted}_*`, `LN_Audio_{Muted,Unmuted}_Accessibility_*`, and `LN_Plist_Accessibility_No_Audio_*`, which counts as muted), English built in as the fallback, while the speaker mark `active speaker`, `Video on`/`Video off` and the profile wording of the name tag are literals the Zoom binary passes through `unlocalized`. Wording outside the vocabulary means "mute unknown, not speaking", never a guess. The meeting is entered on the first participant tile (the join preview is already `zm.meeting.window.main`, with no tile) and leave is debounced 6 s (minimal view replaces the meeting window with an `AXSystemDialog` holding one tile, the current speaker; without the meeting window the helper reports the tab groups of every such dialog, and they count as the minimal view only when one of them is a participant tile). The tree holds only on-screen tiles, so the roster accumulates over the meeting and is never trimmed; Zoom keeps the speaker on screen in speaker view. The user's own microphone comes from the meeting menu's audio item, which follows it with the menus closed while the toolbar leaves the tree whenever the pointer is off the window: its title is the `Mute Audio` key of the strings files while the microphone is open and `Unmute Audio` (or `Join Audio`) while it is closed; `own_mic_muted()` reads it with positive evidence only, `localMicMuted` carries it to the bridge, other wording leaves it to the own tile (logged once per meeting), and with no tile of the user's own (a webinar attendee, who also has no audio item) it counts as closed — only when the account name is known, since without it the own tile cannot be told apart. The own tile is the one named like the account: the account button of the main window, or with that window closed the single `Name(email)` item of the application menu's account submenus (`menu_account_name()`). A `meeting_join_rejected` silences the source for 30 s. Enabled by `live --zoom-app` or `RECOG_ZOOM_APP=1` (daemon thread in the live process, restarted 30 s after a failure) or run standalone as `cargo run --release -p recog-zoom --bin recog-zoom-app` from `rust/`. **The helper is its own application, not a child process**: macOS grants Accessibility to the responsible process, which for a child is its parent — a Python interpreter whose path changes with every upgrade. `find_or_build_helper()` builds `RecogZoomHelper.app` at the repo root (bundle id `com.recog.zoom-helper`, `LSBackgroundOnly`, signed with `RECOG_CODESIGN_IDENTITY` or ad hoc) and `helper_lines()` starts it through LaunchServices (`open -g -n -a … --args --socket <path>`); having no parent to share a pipe with, the helper connects back to a Unix socket in a private temp directory and exits when that socket is gone. The permission therefore belongs to "Recog Zoom Helper", which asks for it itself on first start. `AXIsProcessTrusted()` is answered from a per-process cache, so a helper without the permission reports `not_trusted` and **exits**; `run()` starts a fresh one every `PERMISSION_RETRY_S` (15 s) with `--no-prompt`, which is how a grant is picked up without restarting the service (any other end of the helper's stream is a failure). Rebuild freshness is the source's sha256, **not mtime**, kept in `RecogZoomHelper.source-sha256` **beside** the bundle: an ad-hoc signature is the hash of the bundle, the same code compiles to the same binary, and a stamp inside the bundle would be sealed by the signature and make even a comment edit lose the grant. The build goes to `RecogZoomHelper.app.build` and replaces the working bundle only on success. See `docs/zoom-app.md`. It lives in `rust/recog-zoom` (a library the live process runs, and `recog-zoom-app`, the standalone run).

### System audio backends
Both backends are the same `SubprocessPCMCapture` (`rust/recog-live/src/capture/`): a
child process writes raw mono float32 PCM at 16 kHz to stdout, a reader thread
appends it to the shared `BufferedAudioCapture`. `resolve_system_audio_backend`
turns `auto` into `screencapture` on darwin and `pulse` on linux (and rejects a
backend asked for on the wrong OS); `create_system_capture` builds it.

- **macOS — `screencapture`**: the ScreenCaptureKit Swift helper
  `sck_audio_capture`. `find_or_build_sck_utility()` compiles it on demand from
  `recog/native/macos/sck_audio_capture.swift` (the binary stays at the repo root
  for existing installations and is gitignored). Manual rebuild:
  ```bash
  swiftc -o sck_audio_capture recog/native/macos/sck_audio_capture.swift \
      -framework ScreenCaptureKit -framework CoreMedia -framework AVFoundation
  ```
- **Linux — `pulse`**: `PulseAudioCapture` runs `parec --raw
  --format=float32le --rate=16000 --channels=1 --latency-msec=100
  --device=<monitor>` (PulseAudio or PipeWire via `pipewire-pulse`). A
  non-empty `RECOG_PULSE_MONITOR_SOURCE` pins the source and turns the follow
  logic off; otherwise `start()` resolves the default sink itself and pins
  `parec` to that concrete `<sink>.monitor` (falling back to
  `@DEFAULT_MONITOR@` when `pactl` is unavailable — the first successful poll
  then reconnects to a concrete monitor instead of adopting the current sink
  as its baseline), then **polls
  `pactl get-default-sink`** and explicitly reconnects when the default sink
  changes — the `@DEFAULT_MONITOR@` name alone is resolved once at stream
  creation and does not follow, and taking the *first poll* as the baseline
  would silently adopt a sink switched between the spawn and that poll,
  leaving the capture on the old output. A planned reconnect marks the source
  boundary (`mark_source_boundary()`: buffer dropped, `source_epoch` bumped, so
  no overlap is glued across the gap and `_process_chunk` tells the WAV writer
  to write the first post-switch chunk whole instead of cutting 0.5 s that
  never reached the file) and does not spend the crash-retry budget;
  a transient `pactl` error is rate-limited-logged and retried. Without
  `--device`, `parec` would record the default *source* (the mic), so the
  monitor name is mandatory.

Restart policy is shared: a died child is respawned with a bounded backoff
(1/2/5/10 s) charged **before every respawn attempt**, not only before a retry
after a failed one — a child that starts fine and dies half a second later
otherwise burned the whole budget in ~3 s without a single pause. The wait
**does not leave the reader loop while `running`** (and a `stop()` arriving
during it aborts the respawn instead of orphaning a fresh child); after
`MAX_RESTARTS` the capture is marked lost (`_dead`, with the child's last
stderr lines in the log) and `rust/recog-live/src/live/` writes one system-capture-lost
marker, but the loop keeps running and retries every `LOST_RETRY_S` (60 s):
the cause is often temporary (the Mac slept with no display, a permission
awaiting confirmation), and giving up for good left a service without system
audio for hours. The first data clears `_dead` and live writes a recovery
marker. The loss marker is
separate from `_emit_pcm_transition_marker` on purpose: a mixed-capture death
must not claim a PCM→mixed transition or pause refine.

The ScreenCaptureKit helper never restarts its stream in place: on
`didStopWithError` (e.g. `-3805`, the connection to `replayd` interrupted) or
after 10 s without audio callbacks it **exits**, and the respawn above starts a
fresh process. A stream restarted inside the same process after `-3805` either
never started or reported itself running while delivering only silence, and
only a new process captured again. Silence alone cannot tell a broken stream
apart (ScreenCaptureKit delivers real silence as exact zeros too), so
`LiveTranscriber._restart_silent_system_capture` asks for a planned restart
(`request_restart`, no crash-retry budget spent) when the system capture has
delivered only zeros for `DIGITAL_SILENCE_RESTART_S` (60 s) while a conference
is active (`KTalkBridge.is_conference_active`); a restart during real silence
loses nothing.

On macOS the microphone is a subprocess capture too: `ChildMicCapture`
(`rust/recog-live/src/capture/framed.rs`) reads framed audio from `bin/recog-mic`
(`rust/recog-mic`), each frame carrying its ADC time and wall clock
(`create_mic_capture` in `rust/recog-live/src/live/build.rs` chooses). The worker
became a child process when the Python live process lost samples to any busy thread
holding the interpreter lock; the split stays. It exits on every way a stream stops
capturing (finished callback, inactive, 10 s without a callback), and the shared
respawn starts a new one on a device list read afresh, the device found by name.
A respawn keeps the buffer, so no source boundary is marked.

The live process is `rust/recog-live`'s binary `recog` (`live` with every flag the
Python CLI had, `meetings`, and `replay <session> <out>`, the golden player), its main loop
(`src/live/`) over the ported captures, ingest, refine worker and `recog-bridge` on
its own runtime thread in the same process. It runs ASR on the `wavo` crate, or with
`[asr] backend = "remote"` on the gateway with wavo as the lazy local fallback
(`src/asr/remote.rs`, held to the answers of the Python client by
`tests/asr_remote_frozen.rs`); `local` (PyTorch) is not in it. On Linux its microphone
is `parec` on the default source instead of PortAudio. Before the Python live process
was deleted it was held to it by golden replay of synthetic and recorded sessions
(same rows, markers and `meta.json` fields); the recorder and the player keep their
own tests (`tests/golden.rs`).

`rust/recog-mic` is the only worker (built by `setup.sh`; a missing build is said
in the log; `[live] mic_worker` is still accepted and does nothing), with the
arguments, frames and exit codes of the former Python worker. Its cpal callback takes no lock and only copies into
wait-free `rtrb` rings; the main thread writes every 20 ms. cpal sets the device's
nominal rate and physical format to what a stream asks for, which would change them
for every other process, so the worker asks for the device's current rate and a
sample type for which cpal's format matching cannot write (`physical.rs` repeats that
matching on the device's format list; no safe type, no stream), and converts to the
parent's rate itself (`resample.rs`: windowed sinc centred on each output sample's
exact input position, so no group delay). A frame is stamped from the input block its
first sample lies in (`mHostTime` of the block, PortAudio's `inputBufferAdcTime` at the
device's rate); a block that does not continue the previous one flushes the converter,
so a hole stays a hole in the stamps. At 16 kHz PortAudio's stamps are 1 ms early
against its audio (AUHAL's converter delay, not accounted), so a Rust take is placed
16 samples earlier than a Python take of the same sound.

Echo cancellation (`[live] echo_cancel`, off by default until it passes the checks
in `docs/plans/2026-10-30-mic-echo-cancel-design.md`) is `EchoStage`
(`rust/recog-live/src/echo/`, Speex through its port in `rust/speexdsp-aec`) between the
microphone's frames and its buffer.
With a stage `create_system_capture` runs `sck_audio_capture --frames`
(`FramedSystemCapture`), whose frames also carry the presentation time and whether
the default output can make an echo (the built-in speakers, audible, read by the
helper every 100 ms). Both streams are placed by their stamps, which share the host
uptime clock, not by counting samples, holes filled with zeros. The microphone comes
out raw, byte for byte, unless the output can make an echo, the echo path is present
(`EchoPathDetector`: a call can play elsewhere while the default output is the
speakers), the reference has not been quiet for 300 ms and a reference exists;
paused, Speex keeps its filter. An exception in the stage also leaves the microphone
raw. `[mic] Echo cancelling` / `Back to the raw microphone` in the log tell which is
on. The inverse of Speex's low cut is measured from the canceller at start; the port
is bit-exact with the pyaec 1.0.1 build the chain's numbers were measured with.

sounddevice captures in the live process (`SingleDeviceCapture`: the microphone
on Linux and `device:<id>`) fail differently: a configuration change of the device (e.g. another process
changing the built-in microphone's sample rate) makes CoreAudio fail a render
call with `-10863`, and PortAudio stops the stream for good — finished
callback, `stream.active` False, no further callbacks, no zeros. The capture
passes a `finished_callback` (ignored for its own `stop()`) and
`LiveTranscriber._reopen_stopped_device_captures` calls `reopen_if_broken()` on
every loop iteration while recording is allowed: a stream that finished, went
inactive, or has not called back for `CALLBACK_STALL_S` (10 s) is closed and
reopened on the device found again **by name** (indices shift when headphones
connect; the PortAudio device list is re-read with `_terminate()`/`_initialize()`
unless another sounddevice stream is open, since that would close it). A failed
reopen is retried with `RESTART_BACKOFF_S` and never raises into the loop.

## Architecture

The sections below were written for the Python modules each Rust crate ports
(the table in docs/plans/2026-11-07-rust-migration.md maps them); the modules are
deleted, and their ports keep the structure, names and log lines described here.

### Data directory

`recog.config.get_data_dir()` resolves the shared data root: `~/.recog`, or
the absolute/`~/…` `RECOG_DATA_DIR` override. Meetings, logs and `speakers*.json`
live under it; `.env` stays in the checkout. Python consumers, setup, backup
and service rendering share this resolver via `scripts/data-dir.sh`.
Without an explicit override, legacy data keeps the entire root in the
checkout with a migration warning; data in both roots is a fatal conflict.
Service units pin the resolved root in their environment; re-render and
restart after changing it. Working directories still point at code.

`scripts/migrate-data.py` defaults to a dry run; `--apply` requires stopped
services and manual writers, then renames all data on the same filesystem
without overwriting or merging entries. Re-run after an interruption to
finish remaining moves, then `./recog-service.sh install` and
`./recog-service.sh restart`. `setup.sh` never migrates existing data.

### Local user profile

`rust/recog-core/src/user_profile.rs` owns validation, atomic private persistence and identity
resolution. `user-profile.json` lives under `get_data_dir()`, is backed up and
migrated with other personal data, and is never committed. The web UI owns writes
through GET/PUT/DELETE `/api/user-profile`; initial onboarding and
**Settings → Мой профиль** share `recog/web/static/user-profile.js`.

Precedence: explicit CLI `--mic-speaker-name` > saved name > `MIC_SPEAKER_NAME` >
`Me`. Live supplies the same resolved snapshot to its microphone and bridge.
`user_profile_refresh` is subscriber-only and makes Live reread the file;
`user_profile_applied` acknowledges the actual revision only after application.
`rust/recog-web/src/user_profile.rs` distinguishes persisted from applied, rejects stale
acknowledgements and reconciles after bridge reconnect. `user_profile_updated`
updates an already running Copilot (including manual sessions); meeting/tick/chat
events and transcript tool responses carry the complete identity snapshot.
`build_copilot_context()` in the MCP channel renders one literal profile block
for push events, long-poll results and transcript snapshots. Legacy identity
metadata remains for compatibility; the skill consumes the rendered context.
Agent instructions treat profile text as data, replace old aliases and do not invent names. Profile edits never rewrite
historical transcript labels or stop the session.

### Speaker identification
Speaker identification comes from conference participant signals via the Chrome extension. Microphone audio uses the user profile; system audio without conference identity stays uncertain. The former dual-venv pyannote subprocess architecture (ADR-0006) is obsolete.

### Remote ASR offload (opt-in)
`FallbackAsr` (`rust/recog-live/src/asr/remote.rs`) runs the three GigaAM paths of the live process (`transcribe_batch` per-speaker, `transcribe_with_timestamps` refine, and the direct mic/mixed `transcribe_chunk`) over a network gateway (OpenAI-Whisper-compatible); the Python client's offline `transcribe_simple` had no caller and was not ported. ASR comes exclusively from `build_asr()` (`rust/recog-live/src/live/build.rs`): `RECOG_ASR_BACKEND="auto"` (committed default) and `"wavo"` return `WavoAsr`; `"remote"` returns a `FallbackAsr` over a `RemoteAsrClient` with `WavoAsr` as the local model; `"local"` (PyTorch, gone) runs on wavo with a line in the log. `FallbackAsr` loads the local model **lazily in a background thread** — triggered by the first remote failure. While neither backend can serve (circuit open + local still loading) it raises `ASRTemporarilyUnavailable` and exposes `available=False`; consumers (`_process_chunk`, `PerSpeakerIngest.tick`, `RefineWorker`) gate on it and keep their audio buffered instead of dropping chunks (see `docs/plans/2026-06-02-asr-fallback-buffering-design.md`). Live (chunk/batch) and background (refine/offline) requests have independent circuit breakers sharing one lazy local model. Each breaker is thread-safe and trips after `RECOG_ASR_CIRCUIT_FAILS` failures within the last 60 seconds (open for `RECOG_ASR_CIRCUIT_OPEN_S`, then one half-open probe). Live chunk/batch requests use the shorter `RECOG_ASR_LIVE_TIMEOUT_S`; refine/offline keep `RECOG_ASR_REMOTE_TIMEOUT_S`. The optional server is `rust/recog-asr-server` (binaries `recog-asr-gateway` and `recog-asr-gigaam`); installation and configuration are described in the [ASR server guide](docs/asr-server.md). These server processes are separate from the four client services and are not started by `setup.sh`.

### ASR fallback buffering (always-on)
No speech is lost when ASR is temporarily unavailable (remote down + local loading): capture buffers (`SingleDeviceCapture`, `SubprocessPCMCapture`) are not consumed while `asr.available` is False (capped at `ASR_PENDING_BUFFER_CAP_S`), `get_chunk()` returns `(chunk, capture_start_ts)` so backlog segments get spoken-time timestamps (while the buffer holds ≤ `CAPTURE_RESYNC_MAX_S` the start timestamp is re-anchored to wall-clock on every captured block — sample counting alone drifts forever because the 0.5 s overlap never lets the buffer empty), and a chunk caught mid-failure is held in a single-slot retry (`SourceState.pending_chunk`, mirrors `PerSpeakerIngest._pending_retry_batch`) and re-dispatched first once ASR recovers — without re-writing it to the meeting WAV. Live dedup applies only within `DEDUP_MAX_GAP_S` of the previous segment (a phrase repeated after a pause is real speech). Swallowed ASR exceptions are logged via `rust/recog-live/src/log.rs` (rate-limited).

### Meeting audio recording (24h retention, on by default)
`live` records per-meeting WAVs (`audio_mic.wav` / `audio_system.wav`, 16 kHz mono int16) for segment playback via `GET /api/meetings/{id}/audio/{segment_index}`; disable with `--no-save-audio`. Load-conscious design:
- Silent chunks (`rms < VAD_RMS_THRESHOLD`) are not written — EXCEPT while PCM-primary, where PCM segment offsets are extrapolated from a wall-clock↔WAV anchor and need a continuous real-time timeline in `audio_system.wav`. Mixed-path segments capture their offset at write time, so gaps are safe for them.
- PCM-primary segment offsets (`_on_pcm_batch`): `audio_offset_ms` is extrapolated from `_system_wav_anchor` = `(wall_ms of the last system write's first sample, WAV offset before it)`, updated on every `_process_chunk` system write (`write_chunk` returns the skipped-overlap sample count). Offset = `anchor_wav + (chunk_start_ms - anchor_wall)`, keeping consecutive segments' offsets tracking their real capture times. The old `wav_offset_now - chunk_age` formula drifted 0–5 s (the WAV sample count advances in `chunk_duration` steps while chunk age is continuous), collapsing consecutive segments onto one offset. Fallback to the old formula when the anchor is absent (before the first write) or stale (`> PCM_WAV_ANCHOR_MAX_AGE_MS` — degraded-mode silence-skip broke the timeline ahead of it).
- WAV headers are re-patched after every chunk (`IncrementalWavWriter._patch_header_sizes`), so a SIGKILL (a service-manager restart — launchd or `systemd --user` — does not run teardown; sessions end as `stale_session`) never leaves a header claiming 0 data bytes. Per-file cap `MAX_WAV_DATA_BYTES` (2 GiB) protects the format's 32-bit sizes.
- Retention: `MeetingStorage.cleanup_old_audio(retention_hours=24)` deletes WAVs by file mtime (an actively-written file is always fresh — the current meeting is safe) with a stat-only scan (meta.json parsed only for dirs that still have audio). Runs at `start_session` and hourly from the live loop.
- Every meeting switch must rotate writers via `_on_meeting_change`: the KTalk bridge callback does it, and `_check_silence_split` does it for orphan silence-splits (with `continuation=True` + re-writing the triggering chunk so the first segment points into the new meeting's own file). A switch path that skips rotation reproduces "No audio file available for this meeting". Writer rotation also resets `_system_wav_anchor` (the new WAV starts a fresh timeline); the first system write of the new meeting re-seeds it.
- Playback slice duration is bounded per track (`compute_segment_duration_ms` in `rust/recog-web/src/transcript.rs`): mic/system WAV timelines diverge under silence-skip, so only same-`is_local` segments bound each other.

### Per-speaker ASR path (ADR-0018)
For any active KTalk meeting the pipeline routes through per-participant PCM streams (no mixed-audio fallback as a configurable mode):

- Chrome extension attaches `AudioWorkletNode` to each remote `MediaStream` (`browser-extension/src/audio-worklet/downsampler.js`, `src/pcm_transport.js`), downsamples 48 kHz → 16 kHz Int16 PCM and sends chunks via WS (`type: "pcm_chunk"`) to `rust/recog-bridge`.
- Bridge dispatches chunks to `PerSpeakerIngest` (`rust/recog-live/src/per_speaker.rs`) which per-pid reorders by `seq`, batches across speakers (200 ms window, max 8), and calls `ASRModule.transcribe_batch()` (`rust/recog-live/src/asr/`). Attribution comes from `speaker_name` already in the frame — no `get_dominant_speaker_for_period()` call.
- Watchdog: if PCM is silent for `PCM_SILENCE_WATCHDOG_S` (30 s) during an active meeting, `PerSpeakerIngest._degraded` flips true, `LiveTranscriber._process_chunk` falls back to the existing mixed-path with `source="fallback_mixed"` and a gap-marker written to the transcript. Recovery marker is inserted when PCM returns.
- Screen share: a participant sharing a screen with sound has a second audio track, which the adapter and both `attachAllFromConf` skip (`isScreenShareTrack` in `src/utils.js`, the client's own rule: `audioType`, else the source name `-a1`). The track gets no `trackRemoved` when sharing stops, so listening to it would silence the participant until the meeting ends.
- Local mic (`MIC_SPEAKER_NAME`) stays on `sounddevice` — it is not routed through the worklet (D9).

See `docs/plans/2026-04-22-per-speaker-asr-pipeline.md` for the decisions table and task breakdown.

### Per-speaker refine contour
A tier-2 refine pass re-transcribes per-pid PCM windows with word-level timestamps and replaces live-segment text in `transcript.jsonl` with `refined: true`:

- State ownership (design.md D11):
  - `rust/recog-live/src/refine.rs` — `RefineWorker` daemon thread + queue + `make_abortable_decode` decoder monkey-patch + `RefineTask` / `RefineResult` dataclasses.
  - `rust/recog-live/src/per_speaker.rs` — per-pid PCM ring (`_pcm_rings`), monotonic audio counter (`_pid_audio_ms`), silence/continuous-speech counters (`_silence_ms`, `_continuous_speech_ms`), refine watermark. Trigger via `get_refine_candidate(pid)`.
  - `rust/recog-live/src/live/` — per-pid live-segment registry `_pcm_live_segments[pid] = [(segment_id, start_ms, end_ms), ...]`, `_pcm_segment_texts` cache for identical-text skip, `_apply_refine` alignment + `update_segments` call.
  - `rust/recog-core/src/meeting_storage/` — `TranscriptSegment.refined: bool = False`; `update_segments(...)` accepts optional `text` per row, sets `refined=True` when present.
- Flow: PCM arrives via `PerSpeakerIngest.enqueue` → per-pid ring + counters update → tick loop calls `get_refine_candidate(pid)` → gap (`SPEAKER_TURN_GAP=3s` silence) or monolog cap (`PCM_REFINE_MONOLOG_CAP_S=15s`) assembles a 20 s trailing window → `RefineWorker.submit` → worker runs the route-aware callback; remote HTTP holds no model lock, local decode acquires `asr._model_lock` → `_apply_refine` rebases words via `task.offset_base_ms`, intersects with live segments under `_state_lock` + meeting-generation check → `update_segments` rewrites JSONL rows.
- Shared model lock: `ASRModule._model_lock` is a `threading.RLock` (not `Lock`). The local refine callback acquires it around decoder patching and inference to serialise with the live batched-forward path; remote HTTP runs without it; `transcribe_with_timestamps` re-acquires internally — recursion must not deadlock.
- Cooperative abort: default `abort_mode="cooperative"`. `LiveTranscriber._refine_transcribe_fn` installs `make_abortable_decode` on `model.decoding.decode` for the duration of one refine call; the hook polls `should_abort()` at the top of the RNN-T time-step loop and raises `RefineAborted`. Worker catches, releases the lock, re-enqueues the task. `LiveTranscriber._on_pcm_batch` calls `refine_worker.set_live_pending()` / `clear_live_pending()` around `asr.transcribe_batch` so live gets priority. Per the spike (`docs/experiments/2026-04-24-refine-abort-spike.md`), cooperative abort saves ~770 ms per preempted refine on MPS (~80 % of inference cost).

On refine arrival the webapp renders tier-1 (opacity 0.6, dashed border) → tier-2 (solid) segments via `data-refined` on `.transcript-segment`; the KTalk extension overlay does a plain text swap with a brief `.refine-fade` pulse.

See `docs/plans/completed/2026-04-24-per-speaker-refine-backend.md`, `openspec/changes/per-speaker-stream-refine/`, and ADR-0019 for the decisions table and task breakdown.

### Auto-spawn Copilot supervisor (meeting-triggered; code default on, `.env.example` ships it off)
A service daemon (`deploy/macos/com.recog.copilot-supervisor.plist.in` or `deploy/linux/recog-copilot-supervisor.service.in` → `rust/recog-copilot/src/supervisor/`) spawns a fresh headless Claude Code copilot session per conference meeting (any `is_conference_source`: KTalk / Jitsi vas3k·jitsi / Matrix / Google Meet / Zoom) and tears it down after the meeting, so the manual `./recog-service.sh claude` step disappears. Lifecycle model is "по триггеру" (variant B): fresh session per meeting, zero cost between meetings, ~6–10 s cold start.

- `claude -p` is impossible (`--dangerously-load-development-channels` isn't parsed by print mode, `channel_enable` needs a marketplace plugin) — the only path is an interactive TUI in a PTY. The supervisor runs it in a detached tmux on a dedicated socket (`tmux -L recog-copilot`) so its env is deterministic (from the service unit) and isolated from the personal tmux.
- **State machine** (`SessionLifecycle`, pure/no-I/O): `IDLE → SPAWNING → RUNNING → GRACE`. Inputs `meeting_started(id)/meeting_ended(id)/timer/session_died/stop_now(id)` yield commands `spawn(id)/kill/none`. A flap (`meeting_ended` then `meeting_started` with the **same** id within grace — guaranteed by `meeting_storage._find_same_day_meeting`) keeps the session alive; a different id kills + respawns. `RECOG_COPILOT_GRACE_S` (default 90) is the kill delay after `meeting_ended`; an explicit webapp Stop (`stop_now`) kills immediately with no grace — the grace window only shields conference rejoin flaps.
- **BridgeClient**: WS to `ktalk_bridge` (default `ws://127.0.0.1:8766`) with reconnect/backoff. On connect sends `supervisor_hello`; bridge replies `meeting_state {active, meeting_id, meeting_title}` for reconciliation. `active` means a copilot session should exist: either an `is_conference_source(meta["source"])` meeting (ktalk / vas3k / jitsi / matrix / gmeet / zoom) with a live extension client, or the bridge's armed manual-session mirror — never merely `current_meeting_path`, which is always populated. Reconcile branches: live session + no meeting → synthetic `meeting_ended` → GRACE (not instant kill, so a bridge restart mid-meeting doesn't kill the copilot — rejoin returns the same id); no session + active meeting → spawn; live session + active meeting → adopt (no respawn).
- **Watchdog** (~2 s while a session should live): `classify_screen(text) → spawning|online|working|blocked|offline` by fixed-order rules; the known startup dialogs (dev-channels, workspace-trust) are auto-dismissed tmux-natively (dev-channels: `send-keys 1`; trust: cursor moved to the accept option + `Enter`, since claude 2.1.270 starts it on "No, exit" without option numbers; per-dialog latch, resent every 10 s while the dialog stays up); `spawning` past a 60 s deadline → `blocked` (fail-visible). Reports `copilot_agent_status {status, meeting_id}` on change + a ~5 s heartbeat; `offline` feeds `session_died` → respawn with backoff (5/15/30 s, ≤3/meeting).
- **Status merge lives in bridge (D4)**: two authors — supervisor (screen) via `copilot_agent_status`, and `rust/recog-copilot/src/channel/` (MCP-session count) via `claude_sessions {count|null}` (`null` = unknown, not a confident 0). Bridge computes the effective status (`offline` if the supervisor is silent > 10 s OR a debounced `sessions==0` while either a conference or manual session should be alive) and broadcasts `copilot_status {status: "agent_state", ...}` to extension and subscriber clients only on effective-status change. The extension and webapp render their chips (🟢 online / 🟡 working / 🔴 blocked / ⚫ offline) independently of per-request indicators.
- **Linux session lifecycle (independent of the supervisor)**: on Linux `TmuxRunner` wraps `tmux new-session -d` in a transient user unit — `systemd-run --user --collect --unit=recog-copilot-session --property=Type=oneshot --property=RemainAfterExit=yes --property=KillMode=control-group` — so the session lives outside the supervisor's cgroup and `systemctl --user restart recog-copilot-supervisor` does not take a live Copilot down with it; the restarted supervisor re-adopts it through the usual `alive()` probe. `Type=oneshot` + `RemainAfterExit=yes` match `tmux new-session -d` (it forks the server and returns, so without them the unit would be considered finished and garbage-collected the moment tmux detaches); no `Restart=` — respawn is the supervisor's decision. Env the user manager would not otherwise pass is forwarded explicitly (`SESSION_ENV_KEYS`: PATH, HOME, TERM, SHELL, LANG). A `RemainAfterExit` unit stays *loaded* after its session dies and `systemd-run` refuses to reuse a loaded name, so `_reset_session_unit()` stops it and waits for the name to drop before the next spawn. The unit is never installed or enabled; `restart`/`restart-supervisor` do not touch it, explicit Stop/`uninstall` clear it plus both tmux names. Detection is `platform.system() == "Linux" and shutil.which("systemd-run")` (`_linux_session_manager`), so a systemd-less Linux falls back to plain tmux; macOS keeps the plain tmux path unchanged. The watchdog still probes tmux, not the oneshot unit's `active` state.
- **Master switch** `RECOG_COPILOT_AUTOSPAWN=1|0` (default 1) keeps the daemon connected; `0` makes only the meeting frames (`meeting_started`/`meeting_state`/`meeting_ended`) inert — manual spawn/stop and crash-respawn (`on_session_died`) work regardless of the flag (webapp plan D3).
- **Manual sessions from the webapp** (`#/live`): a "Recog Copilot" button with agent-state chip + chat panel in `recog/web/static/app.js`. Spawn/stop travel as `copilot_spawn_request {request_id}` / `copilot_stop_request {request_id}` (browser → `/api/live/stream` → frontend server → bidirectional `BridgeSubscriber` → bridge → supervisor); the supervisor maps them onto the constant synthetic lifecycle id `"manual"` (`MANUAL_MEETING_ID`, mirrored in both `rust/recog-bridge` and `rust/recog-copilot/src/supervisor/`), so a repeated spawn is the existing same-id flap (an explicit click also resets the respawn backoff budget when no retry is pending — Start revives a session parked after 3 exhausted respawn attempts) and Stop is `stop_now` → immediate kill (no grace; the webapp reports a stop as lost if no ⚫ offline arrives within 15 s). Closing or reloading the tab does NOT stop the session — the browser holds no session state and no stop frame is sent on WS close. The dialog is shared by all tabs: the bridge echoes each delivered question to subscribers as `copilot_chat_echo`, and the frontend server keeps the last 200 echo and `copilot_reply` frames in memory (`_copilot_dialog`, stamped with a `dialog_id`) and replays them as `copilot_history` after `init`; a tab shows each `dialog_id` once. The panel moves by its header and resizes by its edges, its place saved in `localStorage` (`recog.copilot-panel.rect`); **Открепить** moves the panel's DOM into a Document Picture-in-Picture window, so panel lookups go through `copilotEl()`, its buttons use listeners rather than `onclick` attributes (which would compile in the floating document), and the live stream stays open while detached, whatever view the tab shows. Error echoes bound to the originating click's `request_id`: `bridge_disconnected` (frontend server, bridge link down), `supervisor_disconnected` (bridge, no supervisor client), `conference_active` (bridge rejects spawn — never forwarded while a conference is already active; a conference starting mid-relay may see the frame delivered, but the manual flag is not armed and the originator still gets `conference_active`). Shared-audience Copilot broadcasts (`copilot_reply` plus broadcast `copilot_status` frames) fan out to `_subscriber_clients` as well as extension clients; per-request rejection echoes remain originator-only and `manual_preempted` is subscriber-only. Manual chat captures `meeting_id` as None when no conference is active, so an orphan-meeting silence-split rotation cannot drop the reply as stale.
- **Conference precedence & known v1 desync**: with meeting-driven autospawn enabled, a conference `meeting_started` preempts a live manual session (kill + respawn under the meeting); the bridge clears `_manual_copilot_active` and broadcasts `copilot_status {status: "manual_preempted"}` to subscriber clients. With `AUTOSPAWN=0` the supervisor ignores meeting frames, so on preemption the bridge still clears its flag and emits the notice but the `"manual"` tmux session lingers untracked — only killable via explicit Stop (documented v1 limitation).
- **Manual-session restart recovery (webapp plan D5)**: manual-session truth has one runtime owner — the supervisor (it physically holds the tmux session). `supervisor_hello` carries `current_meeting_id` — the id of a session the supervisor intends to keep alive (SPAWNING/RUNNING; GRACE and IDLE report null); a hello with `"manual"` and no active conference re-arms the bridge's `_manual_copilot_active` BEFORE `meeting_state` is computed, so reconcile replies `{active: true, meeting_id: "manual"}` and adopts instead of killing. This covers a bridge-only restart while the supervisor stays alive. A full `./recog-service.sh restart` is covered too (DW-9): the manual session carries its kind in its tmux session name — manual spawns create `copilot-manual`, every meeting-driven spawn keeps `copilot` — and the fresh supervisor probes the manual name at startup and reads the type back: `copilot-manual` alive (and `copilot` dead) → `lifecycle.adopt("manual")` + runner rebinds, so hello reports `"manual"`; both names alive → conference wins and the stale `copilot-manual` is killed. `tmux -L recog-copilot ls` therefore shows `copilot` or `copilot-manual` depending on the session kind; `./recog-service.sh stop`/`uninstall` reap both names. (The former residual — a supervisor restart during the manual-stop grace window resurrecting a stopped session — is gone: manual Stop kills immediately, so a stopped session leaves no tmux marker to re-adopt.)
- **No proactive ticks in manual sessions (webapp plan D4)**: `_copilot_tick_state` is seeded only on conference join paths (the `meeting_started` emit and the same-meeting reconnect re-arm) — never by manual frames, so a manual session gets manual chat only; transcript context still flows via `get_recent_transcript`.

See `docs/plans/completed/2026-07-02-auto-copilot-session.md` for the state machine diagram, decisions table (D1–D7), and known v1 limitations (one copilot per machine, unknown startup screen → `blocked`, positive classifiers frozen on claude 2.1.197). See `docs/plans/completed/20260724-webapp-copilot-live.md` for the webapp manual-session decisions (D1–D5) and protocol frames.

### Module Organization
- `rust/` - The Rust workspace; each item names the port of a former Python module and keeps its names
  - `recog-ai/src/provider.rs` - Shared Claude and Codex providers and settings-driven factory; CLI analysis preserves subscription login and isolates personal instructions/plugins.
  - `recog-ai/src/settings.rs` / `defaults.rs` - Revisioned private `ai-settings.json` store and shipped LLM prompts/models. Web Settings edits global models and scenario overrides (analysis, titles, saved prompts, Copilot). Model precedence: scenario > saved global > legacy environment > shipped default. Each generation resolves one snapshot; saved increments and generated titles record model and revision. `rust/recog-web/src/ai_settings.rs` coordinates bridge refresh/ack for Copilot policy; prompt changes reach the next event, model changes the next session. Stable file lock and atomic replacement protect concurrent writes; malformed settings fail visibly.
  - `recog-live/src/asr/` - GigaAM on wavo (`wavo.rs`) and on a remote gateway (`remote.rs`)
  - `recog-live/src/capture/buffer.rs` - `BufferedAudioCapture`: the buffer/overlap/re-anchor machinery shared by the mic (`SingleDeviceCapture`) and the system-audio subprocess captures. Owns `_append_locked`, `clear_buffer`, `get_chunk() -> (chunk, capture_start_ts)`; the contract is 1-D mono float32
  - `recog-live/src/capture/framed.rs` / `recog-mic` - The macOS microphone in a child process: `ChildMicCapture` in the live process, the worker holding the stream; `FramedSystemCapture` for the system audio with time stamps
  - `recog-live/src/echo/` - `EchoStage`: optional echo cancellation on the microphone (`speexdsp_aec`, `rust/`), placement by time stamps, `EchoPathDetector`
  - `recog-live/src/capture/` - `SubprocessPCMCapture` (generic child-process PCM reader) + `PulseAudioCapture` (Linux `parec`, follows the default sink) + `find_or_build_sck_utility` (macOS ScreenCaptureKit helper, `sck_audio_capture` resolved from `REPO_ROOT` — the binary stays at the repo root; its source is `recog/native/macos/sck_audio_capture.swift`) + `resolve_system_audio_backend` / `create_system_capture`. 
  - `recog-live/src/live/` - Real-time transcription (LiveTranscriber with decomposed methods: _setup_meeting_storage, _setup_ktalk_bridge, _start_audio_captures, _process_chunk, _handle_mic_chunk, _handle_ktalk_chunk, _teardown_session; module-level helpers: normalize_word, deduplicate)
  - `recog-ai/src/increment.rs` - Auto-generate versioned AI summaries on ktalk_leave; multi-prompt support via generate_all(), stores per-prompt results in meetings/<id>/increments/<prompt-id>/vN.md
  - `recog-ai/src/prompt_registry.rs` - Load and filter prompt definitions from recog/prompts/*.md files (YAML frontmatter + template body)
  - Node/nvm PATH (`recog-ai/src/provider.rs`) - Utilities for subprocess calls under the service manager (launchd / `systemd --user`) (shared by recog-web, recog-bridge and the title generator)
  - `recog-copilot/src/supervisor/` - Auto-spawn Copilot supervisor: asyncio daemon (`SessionLifecycle` state machine + `TmuxRunner` + `Watchdog`) that spawns a headless Claude session per conference meeting (`is_conference_source`: ktalk/vas3k/jitsi/matrix/gmeet/zoom) via `tmux -L recog-copilot`, drives lifecycle from bridge `meeting_started/ended`, and reports `copilot_agent_status` to the bridge
  - `recog-copilot/src/channel/` - copilot MCP daemon (channel push); `SessionCountReporter` polls `session_manager._server_instances` and reports `claude_sessions {count}` to the bridge for the status merge
  - `recog-core/src/platforms.rs` - The one definition of the platform ids (`PLATFORMS`, also the meeting storage's `CONFERENCE_SOURCES`) and the built-in conference hosts (`BUILTIN_HOSTS`, `BUILTIN_WILDCARD_DOMAINS`), shared by the installation settings, the meeting storage and the origin allowlist; a test in `ws_origin.rs` holds the hosts equal to the extension `manifest.json`
  - `recog-core/src/ws_origin.rs` - Shared WS Origin allowlist (`origin_allowed`): conference hosts mirroring the extension `manifest.json` (`*.ktalk.ru` and `*.zoom.us` wildcards via hostname parse, exact Jitsi/Element/Google Meet hosts) + webapp origin (9080); no-Origin clients (Python daemons) pass, `Origin: null` and everything else is rejected. Used by recog-bridge and recog-web
- `rust/recog-live/src/cli/` - The `recog` binary's commands (live, meetings, replay, check, config, launcher, prefetch, desktop)
- `rust/recog-zoom` - Optional signal source for the native Zoom application on macOS: `RecogZoomHelper.app` snapshots over a Unix socket → `ZoomAppSession` → bridge frames as a no-Origin client (`recog-zoom-app`, `live --zoom-app`, `RECOG_ZOOM_APP=1`). See `docs/zoom-app.md`
- `rust/recog-desktop` - Optional attachment to the official KTalk desktop app (Electron): `DesktopIntegration` (`service.rs`) is an observer task started from the web server and exposed as `GET /api/desktop/status`; `worker.rs` is the short-lived inspector worker, `recog/desktop/bootstrap.js` the code it evaluates inside the app, `cli.rs` the `recog desktop {enable,disable,attach,status}` subcommands (`bin/recog-desktop`). State lives under `<data dir>/desktop/`. See `docs/desktop-ktalk.md`
- `rust/recog-core/src/meeting_storage/` - Meeting metadata, transcript segments and recorded audio storage
- `rust/recog-ai/src/meeting_chat.rs` - Saved dialog about one meeting (`chat.jsonl`, one whole exchange per line) and the prompt of the next question; the `meeting_chat` task, one-shot provider call. Routes `/api/meetings/{id}/chat` in `rust/recog-web/src/router.rs`, panel in `recog/web/static/meeting-chat.js`, which follows the route itself and needs nothing from `app.js` beyond `renderMarkdown`, `escapeHtml` and `views.meeting`
- `rust/recog-asr-server` - Optional, independently launched gateway and GigaAM HTTP backend (`recog-asr-gateway` / `recog-asr-gigaam`, GigaAM on `wavo`, no speaker diarization: answered as the former Python server answered without pyannote), held to that server's answers by frozen tests against a fake backend and a fake model (`docs/asr-server.md`)
- `rust/recog-bridge` - WebSocket server receiving speaker signals from Chrome extension; merges `copilot_agent_status` (supervisor) + `claude_sessions` (daemon) into the effective agent status broadcast to extension and subscriber clients; fans out `copilot_reply` and broadcast `copilot_status` frames to `_subscriber_clients` while keeping per-request rejection echoes originator-only, relays webapp `copilot_spawn_request`/`copilot_stop_request` to the supervisor, and owns `_manual_copilot_active` (conference precedence, `supervisor_hello.current_meeting_id` re-arm). WS connections are gated at accept-time by the Origin allowlist (`rust/recog-core/src/ws_origin.rs`): browser pages must come from a conference host or the webapp, no-Origin Python clients pass. `is_local_mic_muted()` (checked by the live loop before transcribing the microphone) prefers the optional `localMicMuted` of the latest `participants_update`, the source's own word on the user's microphone, which needs no local participant in the list; without it the local participant's `micMuted` decides, and with neither the microphone counts as open. While a meeting has an owner, `participants_update` and `speaker_update` count only from `_meeting_owner_ws`: a tab whose join was refused (a second conference in the same browser) keeps sending its own call's frames, which must not replace the recorded meeting's participants, speaker or `localMicMuted`. A meeting ends (`_end_meeting_on_extension_gone`) when the extension WS that sent `meeting_join` drops — `_meeting_owner_ws` — not only when the last extension client is gone: the content script runs on every conference host, so an idle `example.ktalk.ru` tab holds a WS with no call and would otherwise pin the meeting LIVE forever
- `rust/recog-web` - The web server + SPA (the static UI in `recog/web/static/`; WebSocket for live updates); `/api/live/stream` is gated by the same Origin allowlist (`rust/recog-core/src/ws_origin.rs`) — handshake denied before `accept()` for disallowed browser origins
  - `bridge_subscriber.rs` - bidirectional WS client that connects to `ktalk_bridge` (default `ws://localhost:8766`, overridable via `RECOG_BRIDGE_WS_URL`), forwards `FORWARD_TYPES` frames (`transcript_refine`, `copilot_reply`, `copilot_chat_echo`, `copilot_status`) to webapp live-stream clients on port 9080, and exposes best-effort `send(raw)` for webapp→bridge copilot frames (returns False while disconnected — no queueing across reconnects). See D10 in `openspec/changes/per-speaker-stream-refine/design.md`.
  - `live_watcher.rs` - JSONL tail watcher with inode + size-shrink rewrite detection and a seen-id watermark. Survives atomic `os.replace` rewrites from `MeetingStorage.update_segments` without stalling or re-broadcasting previously-seen rows.
- `browser-extension/` - Chrome extension for KTalk, Jitsi (vas3k/jitsi), Matrix/Element Call, Google Meet, and the Zoom web client
  - `src/` - Source modules (edit these)
  - `src/platforms/` - Per-platform adapters (`ktalk.js`, `vas3k.js`, `matrix.js`, `gmeet.js`, `zoom.js`) + `index.js` (`detectPlatform`, `isJitsiPlatform` — true for every platform except `ktalk`, gating the floating panel and the conference retry-forever loop)
  - `injected.js` - Built bundle (generated by webpack)
  - `content.js` - Injects bundle into the conference page (KTalk top frame, or the Element Call iframe scoped via `all_frames`)
  - `manifest.json` - Extension manifest (Manifest V3); one `content_scripts` entry per platform host

### Coding Style
- Python 3.12, 4-space indentation
- CLI wiring in `rust/recog-live/src/cli/`, reusable logic in the library crates
- `snake_case` for functions/vars, `PascalCase` for classes
- `*_worker.py` suffix for subprocess helpers
- Prefer `pathlib.Path` and type hints for new code
- JavaScript: start a callback on its own line, never on the line of the function enclosing it (`.filter(…)` and `.map(…)` on separate lines). The quality gate joins coverage to functions by line span and stops with a measurement error ("cannot distinguish functions with the same line span") on two functions sharing one; see `docs/quality.md`

## Environment Variables

Non-secret settings belong in the installation configuration `~/.config/recog/config.toml` (`deploy/config.example.toml`, `docs/configuration.md`): each key of its `[asr]`, `[live]`, `[copilot]`, `[web]` and `[macos]` tables is one variable below, and the process environment and `.env` still override it; a different value in both places is reported on stderr at startup. `rust/recog-core/src/install_config.rs` loads `.env`, resolves file + environment once at import into the typed `INSTALL_CONFIG` (`INSTALL_CONFIG.asr.backend`, each field declaring its variable, type and default), and code reads that, never `os.environ`; tests monkeypatch a field (`monkeypatch.setattr(INSTALL_CONFIG.asr, "backend", "local")`). A malformed value stops startup with its name. `.env` keeps secrets and legacy fallbacks. Copy `.env.example` → `.env` and `recog/web/.env.example` → `recog/web/.env` (`setup.sh` does both). `RECOG_TORCH_INDEX_URL`, `RECOG_GIGAAM_URL`/`RECOG_GIGAAM_REF`/`RECOG_GIGAAM_EDITABLE` are deliberately **not** in `.env`: `.env` is read by Python (`load_dotenv` in `rust/recog-core/src/install_config.rs`), no shell script reads it, and `setup.sh` needs those variables before it creates `.env` — they are call-shell variables (`RECOG_TORCH_INDEX_URL=… ./setup.sh`).

- `HF_TOKEN` - HuggingFace token (optional, was required for pyannote diarization which is now removed)
- `CLAUDE_MODEL` / `CODEX_MODEL` - legacy model defaults; saved AI settings take precedence.
- `MIC_SPEAKER_NAME` - Legacy microphone-name fallback before a user profile is saved (default: `Me`)
- `RECOG_ASR_DEVICE` - `auto` (cuda > mps > cpu) | `cuda` | `mps` | `cpu` (default `auto`). An explicitly requested device that is unavailable raises instead of quietly landing on CPU; only an auto-resolved `cuda` is demoted to `cpu` on OOM.
- `RECOG_TORCH_THREADS` - Optional `torch.set_num_threads()` cap for CPU inference (live and refine share one `_model_lock` and all cores). A value that is not a positive integer stops startup.
- `RECOG_PULSE_MONITOR_SOURCE` - Pins the PulseAudio/PipeWire monitor for the `pulse` backend (Linux) and disables follow-the-default-sink. Empty (default) = follow the current default sink.
- `RECOG_ZOOM_APP` - `1` starts the native Zoom application source inside the live process (macOS only, default `0`; same as `live --zoom-app`). "Recog Zoom Helper" needs the Accessibility permission; see `docs/zoom-app.md`.
- `RECOG_ECHO_CANCEL` - `1` cancels what the built-in speakers play out of the microphone (macOS with `--system-audio screencapture`, default `0`).
- `RECOG_MIC_WORKER` - no effect: the macOS microphone's child process is always `bin/recog-mic` (`rust/recog-mic`); still accepted from older configurations.
- `RECOG_LIVE_RUNTIME` - no effect: the live process is always `bin/recog` (through Recog.app's launcher on macOS, so the permissions stay with "Recog"); still accepted from older configurations.
- `RECOG_GOLDEN_RECORD` - `1` records each live session for replay (`rust/recog-live/src/golden.rs`: bridge frames; microphone and system chunks and ASR answers only while a conference is on, since the service listens all day) under `<data dir>/golden/<start>/` (default `0`; personal audio; sessions older than 14 days are deleted when the next starts, unless the directory name ends with `-keep`).
- `RECOG_CODESIGN_IDENTITY` - Code-signing identity for `RecogZoomHelper.app` (default: ad hoc, `-`). With a real identity the Accessibility grant survives a rebuild of the helper.
- `RECOG_EXTRA_WS_ORIGINS` - Comma-separated extra exact origins allowed to open the bridge / live-stream WS. Kept as a separate set so `BUILTIN_HOSTS` and `BUILTIN_WILDCARD_DOMAINS` in `rust/recog-core/src/platforms.rs` stay a literal mirror of the extension's `manifest.json` (a test in `rust/recog-core/src/ws_origin.rs` asserts that equality); a new conference host must be added here **and** to `manifest.json` (separate rebuild).
- `RECOG_CONFIG_FILE` - Path of the installation configuration (default `$XDG_CONFIG_HOME/recog/config.toml`, i.e. `~/.config/recog/config.toml`): non-secret per-installation settings such as conference hosts, source aliases and the web UI names of importer sources, parsed only by `rust/recog-core/src/install_config.rs` (`docs/configuration.md`). The old `.local/conference-hosts.json` (`RECOG_CONFERENCE_HOSTS_FILE`) is still read with a warning while `config.toml` is absent; both present is a startup error.
- `RECOG_BRIDGE_WS_URL` - Overrides the bridge WebSocket URL the frontend subscriber connects to (default `ws://localhost:8766`). Used by `rust/recog-web/src/bridge_subscriber.rs` to forward `transcript_refine` events from `rust/recog-bridge` to webapp clients (D10 — JSONL tail cannot see refine rewrites).
- `RECOG_WEB_RUNTIME` - no effect: the web server is always `bin/recog-web --host 127.0.0.1 --port 9080 --repo <checkout>`, which also runs the KTalk desktop integration's observer; still accepted from older configurations.
- `RECOG_ASR_BACKEND` - `auto` (default) | `wavo`: GigaAM on the Rust runtime wavo inside the live process (`rust/recog-live/src/asr/wavo.rs`; no batch, refine not abortable mid-decode) | `remote` (offload GigaAM to a configured ASR gateway through `FallbackAsr` in `rust/recog-live/src/asr/remote.rs`, built by `build_asr()`, with wavo as the lazy local fallback + circuit breaker) | `local` (the PyTorch model, gone: runs on wavo with a line in the log).
- `RECOG_ASR_REMOTE_URL` - Gateway base URL. **No default**: `RECOG_ASR_BACKEND=remote` with an empty URL makes `build_asr()` raise at startup instead of silently serving from the local model (different RAM and latency). `scripts/deps.sh` checks the pair before any service is activated.
- `RECOG_ASR_REMOTE_MODEL` - Model alias sent to the gateway (default `gigaam`).
- `RECOG_ASR_REMOTE_TIMEOUT_S` - Per-request HTTP timeout (default `15`).
- `RECOG_ASR_CIRCUIT_FAILS` - Remote failures within the last 60 seconds before the circuit opens (default `3`); intermittent successes do not erase recent failures.
- `RECOG_ASR_CIRCUIT_OPEN_S` - Seconds the circuit stays open before a half-open probe (default `60`).
- `RECOG_ASR_LIVE_TIMEOUT_S` - Remote timeout for live chunk/batch requests (default `5`). Refine/offline keep `RECOG_ASR_REMOTE_TIMEOUT_S`.
- `DEDUP_MAX_GAP_S` - Live dedup window (default `2.0`): last-words dedup applies only to chunks captured within this many seconds of the previous segment.
- `ASR_PENDING_BUFFER_CAP_S` - Max audio buffered in memory while ASR is unavailable (default `600`).
- `RECOG_COPILOT_AUTOSPAWN` - Gate for meeting-driven auto-spawn only (code default `1`, but `.env.example` ships `0` so a fresh install never spawns Claude unattended | `0` keeps the daemon connected and makes meeting frames inert; manual webapp spawn/stop and crash-respawn work regardless — webapp plan D3). Read by `rust/recog-copilot/src/supervisor/`.
- `RECOG_COPILOT_GRACE_S` - Seconds the supervisor waits after `meeting_ended` before killing the copilot tmux session (default `90`); cancelled by a same-id `meeting_started` (flap). Not used for an explicit webapp Stop — that kills immediately.
- `RECOG_COPILOT_MCP_URL` - URL of the copilot MCP daemon the spawned Claude session connects to (default `http://127.0.0.1:8767/mcp`).
- `RECOG_COPILOT_RUNTIME` - no effect: the supervisor and the MCP channel are always `bin/recog-copilot-supervisor` / `bin/recog-copilot-channel` with `--repo-root <checkout>`; still accepted from older configurations.

### Call-shell variables (read by the scripts, never from `.env`)

- `RECOG_SKIP_READINESS=1` - bypass the `scripts/deps.sh` gate that `recog-service.sh start` / `restart` / `restart-*` run before touching a service. For deliberately running a subset, and for tests driving stub service managers.
- `RECOG_SERVICE_OS` - overrides the `uname -s` backend choice (which templates and `svc_*` verbs are used), so a Mac can render Linux units.
- `RECOG_PYTHON` - the interpreter baked into the rendered units (`scripts/service/common.sh`). Deliberately **not** `DEPS_PYTHON`, which only selects the interpreter `scripts/deps.sh` uses for its library probes.
- `RECOG_HOST_OS` - overrides `uname -s` for the `scripts/deps.sh` probes (lets a Mac exercise the Linux ones).
- `RECOG_CHECK_PYTHON` - interpreter override for `scripts/check.sh`.
- `RECOG_QUALITY_BASE` - when set, `scripts/check.sh` runs the full merge-base quality gate (`scripts/quality.py`, baseline + head pytest with branch coverage, `docs/quality.md`) instead of plain pytest; CI sets it on both hosts. Unset, `check.sh` runs pytest once plus only the gate's Ruff and ESLint check of lines changed since the local `origin/main` (`quality.py --style-only`, no fetch; skipped when that ref is absent). Either way it runs shellcheck, actionlint and gitleaks first (`docs/quality.md`); ESLint needs `npm ci --prefix deploy/node`, the full gate's JavaScript coverage also `npm ci --prefix browser-extension`, gitleaks a package-manager install.
- `RECOG_QUALITY_OUTPUT` - output root for those quality runs (default `.local/quality`).
- `RECOG_TORCH_INDEX_URL`, `RECOG_GIGAAM_URL` / `RECOG_GIGAAM_REF` / `RECOG_GIGAAM_EDITABLE` - see Setup above.

## Data Files

- `~/.recog/meetings/` - Stored meeting transcripts (JSONL format)
- `recog/prompts/` - Prompt definitions for AI analysis (.md files with YAML frontmatter); prompt ID = filename stem
- `~/.recog/logs/` - Service logs

## Testing

```bash
./scripts/check.sh                        # shellcheck/actionlint/gitleaks, cargo fmt/clippy/test of rust/, the Ruff/ESLint check of lines changed since the local origin/main, the CLI smoke test, the unit render and pytest
RECOG_QUALITY_BASE=origin/main ./scripts/check.sh   # what CI runs: the full quality gate replaces plain pytest

(cd rust && cargo test --workspace)       # the Rust tests
python -m pytest tests/ -q -m "not e2e"   # the shell scripts' and quality tools' tests
./recog-service.sh render --dry-run "$(mktemp -d)"   # unit templates, both backends via RECOG_SERVICE_OS
```

## ADR Documents

Architecture Decision Records in `docs/adr/`:
- **ADR-0006**: Dual venv subprocess (obsolete — pyannote diarization removed, single venv now)
- **ADR-0010**: Live transcription mode (audio-capture part superseded by ADR-0021)
- **ADR-0011**: Meeting storage
- **ADR-0013**: AI transcript analysis
- **ADR-0018**: Per-speaker ASR pipeline (PCM transport from Chrome extension; always-on for KTalk meetings)
- **ADR-0019**: Stream-refine — tier-2 re-transcription with 20 s context (always-on)
- **ADR-0020**: Remote GigaAM offload with lazy local fallback (opt-in)
- **ADR-0021**: Cross-platform capture and services (parec system audio, `systemd --user`, rendered unit templates)
