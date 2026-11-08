# Changelog

Recog has no releases: every installation runs `main`. This file lists, newest
first, what an update asks of an existing installation after `git pull`
(**Action required**) and changes of behaviour you would notice (**Changed**).
Refactors and fixes without a visible effect stay in `git log`. An entry is
written in the same commit as the change; the commit hashes lead to the
reasoning.

## 2026-11-08

### Changed

- **Installing without Python (opt-in).** `runtime = "rust"` under a new `[install]`
  table in `~/.config/recog/config.toml` makes `./setup.sh` build the Rust programs
  with cargo into `bin/` of the checkout, with no venv, pip, torch or GigaAM, and
  makes the service units run those binaries directly (the live one still through
  Recog.app, so the permissions stay with "Recog"). `recog-service.sh` and the
  readiness check then ask `bin/recog` instead of Python. To switch: add the key,
  run `./setup.sh`, check `./recog-service.sh status`; delete it and rerun
  `./setup.sh` to go back. `setup.sh` reads this key itself and never from `.env`:
  setting it there makes the readiness check refuse to start the services. The
  default stays `"python"`, whose units are rendered byte for byte as before
  (docs/usage.md, "Installing without Python").

- **The live process in Rust (opt-in).** `runtime = "rust"` under `[live]` in
  `~/.config/recog/config.toml` (or `RECOG_LIVE_RUNTIME=rust`) makes the live
  service run `rust/recog-live` instead of the Python live process: the same flags,
  meetings, WAVs and bridge, in one process. `./setup.sh` now builds it when cargo is
  installed (on Linux only with the setting set). To try it: `./setup.sh`, set the
  key, `./recog-service.sh restart`. It runs ASR on wavo: with another `[asr]
  backend`, or without the build, the service stays on Python and the log says why.
  No new permission: it still runs under Recog.app. The default stays `"python"`.

- **The web server in Rust (opt-in).** `runtime = "rust"` under `[web]` in
  `~/.config/recog/config.toml` (or `RECOG_WEB_RUNTIME=rust`) makes the frontend
  service run `rust/recog-web` instead of FastAPI, on the same address with the same
  API, live stream and static UI. `./setup.sh` now builds it when cargo is installed
  (on Linux only with the setting set). To try it: `./setup.sh`, set the key,
  `./recog-service.sh restart`. Without the build the service stays on Python and the
  log says so. It also runs the KTalk desktop integration's observer
  (`rust/recog-desktop`). The default stays `"python"`.

- **The Copilot supervisor and MCP channel in Rust (opt-in).** `runtime = "rust"`
  under `[copilot]` in `~/.config/recog/config.toml` (or `RECOG_COPILOT_RUNTIME=rust`)
  makes the copilot-supervisor and copilot-channel services run `rust/recog-copilot`'s
  two binaries instead of Python, with the same tmux session, bridge frames and MCP
  endpoint. `./setup.sh` now builds them when cargo is installed (on Linux only with
  the setting set). To try it: `./setup.sh`, set the key, `./recog-service.sh
  restart`. Without the build both stay on Python and the log says so. The default
  stays `"python"`.

- **macOS: speech recognition runs on wavo by default.** The default of `[asr]
  backend` is now `"auto"`: on a Mac it runs the same GigaAM model on the Rust
  runtime wavo, 6-12 times faster than PyTorch with the same word error rate, once
  `./setup.sh` has built it (it needs cargo: install Rust with rustup). After `git
  pull`: `./setup.sh`, then `./recog-service.sh restart`. Without cargo ASR stays on
  PyTorch and the log says so. To keep PyTorch: `backend = "local"`. Linux keeps
  PyTorch under `"auto"`.

- **macOS: a microphone worker in Rust (opt-in).** `mic_worker = "rust"` under
  `[live]` in `~/.config/recog/config.toml` captures the microphone with
  `rust/recog-mic`, which `./setup.sh` now builds when cargo is installed. The
  Python worker still loses a few milliseconds of audio now and then under the
  service (1-3 holes in half an hour); the Rust one lost none in the same test. To
  try it: `./setup.sh`, set the key, `./recog-service.sh restart`. Without the build
  the Python worker runs and the log says so. The default stays `"python"`.

## 2026-11-07

### Action required

- **macOS, if you use echo cancellation (`[live] echo_cancel`): install Rust and
  run `./setup.sh` after `git pull`.** The canceller is now the Rust port of the same
  Speex build (`rust/speexdsp-aec`), and setup builds its Python module with cargo:
  install it with rustup (https://rustup.rs), then `./setup.sh` and
  `./recog-service.sh restart`. Without cargo setup goes on and the microphone
  stays raw, as it does with echo cancellation off. `pyaec` is no longer a
  dependency; `venv-gigaam/bin/pip uninstall pyaec` removes it.

### Changed

- **Golden session recording (opt-in):** `[live] golden_record = true` records
  each live session into `~/.recog/golden/` for replay when parts of recog are
  rewritten: the extension's frames, and while a conference is on the microphone
  and system audio and the ASR answers. It holds the meeting's voices and words,
  0.5-1 GB per hour of meeting. Sessions older than 14 days are deleted when the
  next one starts; rename a session's directory to end with `-keep` to keep it.

- **With echo cancellation on, what the speakers play rarely reaches your name.**
  A loss of under a millisecond in the microphone left its audio misplaced against
  the speakers' for seconds, and the echo came through in short fragments: on a
  podcast from the MacBook speakers, 14 of 239 lines; now 2. Run
  `./recog-service.sh restart` after `git pull`.
- **System audio is no longer put under your own name.** When the conference marked
  you as the speaker (your microphone hearing what the speakers play, a podcast for
  one), the system audio of that moment was attributed to you; it now goes to a
  remote participant or stays "Unknown".
- **System audio with no one else in the call is "Системный звук".** Alone in a
  call, or outside any call, what the computer plays was "Unknown (?)"; it now has
  its own speaker, without the uncertainty mark. With others in the call, an
  unattributed line is still "Unknown".

- **ASR backend `wavo` (opt-in): the same GigaAM on a Rust runtime, 4-12 times
  faster, with half the memory.** Set `[asr] backend = "wavo"` in
  `~/.config/recog/config.toml` (or `RECOG_ASR_BACKEND=wavo`), install Rust with
  rustup (https://rustup.rs), run `./setup.sh` (it builds `rust/wavo-asr-py`) and
  `./recog-service.sh restart`. The first start downloads the 274 MB model into the
  Hugging Face cache. Its WER matches PyTorch's within 0.06 points
  (`docs/experiments/2026-11-07-rust-asr-runtimes.md`); a refine cannot be stopped
  part-way, which at 60-100 ms it does not need. Back with `backend = "local"`.

- **Echo cancellation gives the same output as before, with less load.** The port
  is bit-exact with pyaec 1.0.1 and runs a block in about a third of the time,
  without holding Python's interpreter lock.

## 2026-11-06

### Action required

- **Restart the services after `git pull`:** `./recog-service.sh restart`.

### Changed

- **A Codex summary or title no longer fails with "Reconnecting... 2/5".** Codex
  reports its own network retries that way; the answer it delivered after the
  retry was thrown away. Generation now fails only when Codex gives up.
- **The meeting page's Спросить button is now Ask**, like the other buttons beside
  it. The send button inside the question panel keeps its name.
- **The Настройки menu in the top right corner is now Settings**, like the Meetings
  and Live links beside it. Its items keep their names.
- **The meeting page shows the source as plain text.** It carried the coloured
  background of the meeting list's source badge, stretched across its column.

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
