# Changelog

Recog has no releases: every installation runs `main`. This file lists, newest
first, what an update asks of an existing installation after `git pull`
(**Action required**) and changes of behaviour you would notice (**Changed**).
Refactors and fixes without a visible effect stay in `git log`. An entry is
written in the same commit as the change; the commit hashes lead to the
reasoning.

## 2026-11-06

### Action required

- **Restart the services after `git pull`:** `./recog-service.sh restart`.

### Changed

- **A Codex summary or title no longer fails with "Reconnecting... 2/5".** Codex
  reports its own network retries that way; the answer it delivered after the
  retry was thrown away. Generation now fails only when Codex gives up.
- **The meeting page's Спросить button is now Ask**, like the other buttons beside
  it. The send button inside the question panel keeps its name.

## 2026-11-05

### Action required

- **Restart the services after `git pull`:** `./recog-service.sh restart`.

### Changed

- **A large KTalk meeting no longer loses the participants who joined first.**
  When speech recognition fell behind, their audio was dropped for the rest of the
  meeting while later arrivals were still transcribed. The oldest audio is now
  recognised first, and audio dropped because recognition is behind is reported in
  the live log.
- **Silent participants no longer hold up speech recognition.** Each attached
  participant streams audio, muted or not, and all of it went to recognition,
  which fell behind with dozens of participants. Silence is no longer recognised.

## 2026-11-02

### Action required

- **Run `./setup.sh` after `git pull`, then reload the extension:** the KTalk fix
  below is in the extension.
- **Restart the web service after `git pull`:** `./setup.sh` does it, or run
  `./recog-service.sh restart-web`. Playing an imported call or a call recorded as
  one file needs the new server code.

### Changed

- **An imported phone call can be played.** The play buttons use the call's
  `audio_me.m4a` and `audio_them.m4a`, each for its own side.
- **A phone call recorded by Samsung says so.** Its details show `Записано: Samsung`.
- **A call recorded as one file can be played.** The play buttons fall back to the
  meeting's `audio_mix.m4a`, and a line stops where the other side's reply starts.
- **A KTalk participant who shares a screen with sound stays in the transcript.**
  The extension used to switch to the screen's audio track and stay on it until
  the end of the meeting, so the participant's speech was lost. It now listens
  only to the microphone track, and the screen's sound is not transcribed as
  their speech.

- **The meeting list can hide orphan recordings.** The source filter has a new
  choice, **All except Orphan**.

- **A meeting page can answer questions about the meeting.** **Спросить** opens a
  panel beside the transcript; times in an answer scroll to the replica they cite.
  Run `./recog-service.sh restart-web` after `git pull`.

## 2026-10-31

### Action required

- **Run `./setup.sh` after `git pull`:** it installs the new dependency `pyaec`
  and rebuilds `sck_audio_capture`. Without `pyaec` nothing breaks; echo
  cancellation just stays off with a line in the log.

### Changed

- **Echo cancellation for the built-in speakers, off by default.** With
  `echo_cancel = true` under `[live]` in `~/.config/recog/config.toml` and
  `./recog-service.sh restart`, what the speakers play no longer reaches the
  transcript under your name. Headphones, another output, a muted output and a call
  playing elsewhere leave the microphone exactly as it was. The log says
  `[mic] Echo cancelling` and `[mic] Back to the raw microphone`. It stays off by
  default until it has been checked on real calls and headphones.

- **On macOS the microphone runs in a child process.** The live process lost
  0.4–0.6% of the microphone's audio whenever it was busy, in pieces that no log
  showed; the transcript and `audio_mic.wav` now hold all of it. The process
  appears as `mic_worker.py` next to the live service and needs no new permission.
  A hole that still happens is logged as `[mic] … ms hole in the captured audio`.
  Run `./recog-service.sh restart` after `git pull`.

## 2026-10-30

### Changed

- **The Copilot dialog in the web UI is shared between tabs.** Every tab on
  `#/live` shows the questions and replies of the others, and a new or reloaded
  tab gets the last 200 of them when it connects, also questions asked from the
  extension's panel. The history lives in the frontend service's memory and
  ends with it. Run `./recog-service.sh restart` after `git pull`, then reload
  open tabs.
- **The Copilot panel moves, resizes and detaches.** Drag it by the header and
  resize it by any edge or corner; the place is remembered. **Открепить** opens
  it in a window of its own on top of other tabs and applications (Chrome and
  Edge, on `127.0.0.1` or HTTPS; elsewhere the button is hidden). The tab that
  opened the window has to stay open. Reload the page after `git pull`.

## 2026-10-27

### Action required

- **One installation config, `~/.config/recog/config.toml`**
  ([docs/configuration.md](docs/configuration.md); `990fdb7`, `ade4dbe`).
  Copy [`deploy/config.example.toml`](deploy/config.example.toml) there and
  move into it:
  - conference hosts and aliases from `.local/conference-hosts.json`
    (`jitsi`/`matrix` → `[conferences]`, `source_aliases` →
    `[sources.aliases]`), then delete the old file. It is still read, with a
    warning, while `config.toml` does not exist; with both present the
    services refuse to start;
  - non-secret settings from `.env`: `RECOG_ASR_*`, `ASR_PENDING_BUFFER_CAP_S`,
    `DEDUP_MAX_GAP_S`, `RECOG_PULSE_MONITOR_SOURCE`, `RECOG_ZOOM_APP`,
    `RECOG_COPILOT_*`, `RECOG_BRIDGE_WS_URL`, `RECOG_EXTRA_WS_ORIGINS`,
    `EXTERNAL_MEETINGS_PATHS`, `RECOG_CODESIGN_IDENTITY` → `[asr]`, `[live]`,
    `[copilot]`, `[web]`, `[macos]`, then delete the lines from `.env`.

  `.env` and the process environment still override the file; a setting with
  a different value in both places is reported at every start
  (`recog: … overrides … of config.toml`). `.env` keeps secrets.
- **Importer source names are no longer built into the web UI** (`c125a01`,
  `ec4c87a`). Limitless, KTalk Export and Zoom Export show their raw source
  name until named in `[sources.labels]`, e.g. `limitless = "Limitless"`.
  `RECOG_SOURCE_LABELS`, which existed for part of that day, is gone.
- **A malformed setting stops startup**, naming the variable (`3ee76f6`):
  `RECOG_TORCH_THREADS` that is not a positive integer (was ignored), a flag
  outside `1/0`, `true/false`, `yes/no`, `on/off` (an unknown word used to
  turn `RECOG_COPILOT_AUTOSPAWN` on), a number that does not parse. Check the
  output of `./recog-service.sh restart`.
- **The extension build needs Python 3.11 or newer** (`990fdb7`): it asks
  `python -m recog.install_config` for the conference hosts, using
  `RECOG_PYTHON`, else `venv-gigaam/bin/python`, else `python3`.
- After pulling: `./recog-service.sh restart`, and
  `npm run build --prefix browser-extension` when conference hosts moved.
  [Updating an install](docs/usage.md#updating-an-existing-install) now reads
  this file instead of carrying one update's steps.

### Changed

- `RECOG_ZOOM_APP=true` enables the Zoom application source; only `1` did.
- `scripts/backup-data.sh` also saves `config.toml` (`b898bb4`).

### Development

- Quality gate: the CRAP ceiling is 10 (was 15), and production JavaScript is
  measured too. Install `npm ci --prefix deploy/node` and
  `npm ci --prefix browser-extension`; start a JavaScript callback on its own
  line ([docs/quality.md](docs/quality.md)).
- Code reads settings from `INSTALL_CONFIG` (`recog/install_config.py`),
  never from `os.environ`; tests monkeypatch a field.

## 2026-10-21

### Action required

- **macOS: the live service runs under `Recog.app`** (`a9e95fa`), so Screen
  Recording and the microphone belong to "Recog" and survive Homebrew Python
  upgrades. Run `./setup.sh` (it builds the bundle), then
  `./recog-service.sh install` and `./recog-service.sh restart`, and allow
  "Recog" when macOS asks. `scripts/deps.sh` refuses to start the live service
  without the bundle.

## 2026-09-05

### Action required

- **Personal data moved out of the checkout** to `~/.recog` (`c54af8b`).
  An existing install keeps its data in the checkout, with a warning, until
  migrated: stop the services, run `scripts/migrate-data.py` (dry run), then
  `--apply`, `./recog-service.sh install` and `./recog-service.sh restart`
  (CLAUDE.md, "Data directory").
- **Repository layout reorganized** (`f65bccb`): re-render and restart the
  services ([docs/repository-layout.md](docs/repository-layout.md#updating-an-existing-checkout)).
- **Conference hosts are installation settings** (`d9dc137`): a Jitsi or
  Element server other than the built-in ones must be configured locally
  ([docs/conference-hosts.md](docs/conference-hosts.md)) and the extension
  rebuilt. Chrome loads the built extension from `browser-extension/dist/`;
  replace a registration that points at the source directory.
