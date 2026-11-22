# Changelog

This file lists, newest
first, what an update asks of an existing installation after `git pull`
(**Action required**) and changes of behaviour you would notice (**Changed**).
Refactors and fixes without a visible effect stay in `git log`. An entry is
written in the same commit as the change; the commit hashes lead to the
reasoning.

## 2026-11-22

### Changed

- Linux releases include downloadable packages for apt and pacman. Install the
  file with your package manager, then open **Set up Recog** in the applications
  menu to configure the services for your user.

## 2026-11-21

### Changed

- The Finder app now checks GitHub Releases with Sparkle. Choose **Recog →
  Проверить обновления…** to check manually or change automatic checks in the
  same menu. Users of the original 0.2.0 disk image need to replace Recog.app
  once from the first Sparkle-enabled disk image.
- Homebrew downloads the prebuilt Mac archive directly from the public GitHub release.
- A macOS disk image includes the Finder application, release runtime and FFmpeg
  for playback. Drag Recog into Applications and open it to set up the services
  and their permissions. Meetings and settings stay in their user directories.

- The macOS release includes ad-hoc-signed Recog and Zoom helper apps. Homebrew
  and archive installs no longer need Swift. Run `recog update` as usual.
  Setup preserves an existing valid app with the same source stamp and bundle
  metadata, including a locally built app from 0.1.5. If a future update
  changes the permission holder itself, macOS may ask for access again.
- On a Mac, interactive setup now shows a native permissions window with the
  current state of each grant, individual request buttons and links to System
  Settings. Reopen it from the local web readiness panel or with
  `recog setup --permissions`. Remote terminals keep the text guide.

## 2026-11-19

### Changed

- Recog has a microphone icon in macOS permission settings. Run setup to rebuild
  the launcher. With ad-hoc signing, macOS may ask for the capture permissions
  again after this update.

## 2026-11-18

### Changed

- The prebuilt macOS audio and permission helpers target macOS 13 on Apple
  Silicon, independently of the release builder’s macOS version.

- A conference now records the default microphone alongside system audio on
  Linux. Previously an install using the default input opened only the output
  monitor. After upgrading, both tracks start when the conference joins.

- With native Zoom enabled, setup now checks Accessibility through Recog Zoom
  Helper itself. The terminal and web readiness checks show its current grant.

- Archive installations now use the current GitHub CLI login to update from a
  private release. Install `gh` and sign in with repository read access. To
  upgrade from the 0.1.0 standalone archive, download the newer archive once
  with `gh release download` and run its `bin/recog setup`.

- **Action required after pulling this update:** run `cargo setup` once. Use
  `recog service start|stop|restart|status`, `recog service logs [name]` and
  `recog copilot` in place of `./recog-service.sh`.
- Release archives include the programs and unpacked Chrome extension. Their
  `bin/recog setup` installs into a stable user directory.
- Open Settings → Готовность установки in the web interface to check capture
  permissions, required programs and services.
- On macOS, `recog setup --permissions` walks through microphone and system
  audio access under the installed Recog launcher. `recog doctor` reports each
  grant separately. Run setup once to install the permission probe.
- From a checkout, use `cargo setup` to build and install, and `recog update`
  for a fast-forward update. Both defer installation during a conference.
- After rebuilding, `recog service` can install, start, stop and restart the user
  services, display status and follow logs. `recog doctor` checks the installed
  programs, configuration and running transcription services.
- **Action required:** replace `scripts/backup-data.sh` and `scripts/migrate-data.py`
  in scheduled commands with `recog backup` and `recog migrate-data`.
- `recog backup` creates a snapshot of meetings and private settings. The
  `--with-logs`, `--dest` and `--remote` options are available after rebuilding.
- `recog migrate-data` previews a move of legacy data out of the checkout.
  After stopping writers, `--apply` moves the entries on the same filesystem.
- New installations open audio capture only during supported conferences. Setup
  keeps the recording flag of an installed live unit. After `git pull`, rebuild
  with `./setup.sh` to use `recog live --conferences-only`.
- Run `bin/recog copilot` after setup to launch Claude Code in the terminal with
  the same agent, model and channel as a meeting session. Answer startup prompts
  directly in the terminal.
- Run `recog service render DIR` after rebuilding to preview the four service
  definitions in a directory without contacting the service manager.
- Setup and service activation now work without tmux or Claude Code. After
  `git pull`, run `./setup.sh` as usual. Only Copilot needs these tools.

## 2026-11-12

### Changed

- **Calls on the conference hosts of `config.toml` are transcribed again.** Since
  the live process became Rust (2026-11-08) its bridge admitted only the built-in
  hosts and `[web] extra_ws_origins`, and refused the extension on every host of
  `[conferences]` (`Rejected WS connection: disallowed origin` in the live log):
  a call on your own Jitsi or Element had no transcript. Run `./setup.sh`, which
  rebuilds `bin/` and restarts the services.
- **A Copilot whose model is out of usage shows 🔴 blocked.** When Claude refused
  every message for a usage limit (`You've reached your Fable limit`), the chip
  stayed 🟢 online and a question waited for an answer that never came. Switch the
  Copilot's model in Settings, or `/model` in its session; the chip turns green
  with the next answer.
- **A summary or other AI result that Claude refused says why.** It showed
  "Claude CLI error: Unknown error" when Claude was out of usage or did not know the
  model, because Claude prints the reason where its answer would go; that reason is
  now the error, e.g. "You've reached your Fable limit".

## 2026-11-11

### Changed

- **The Privacy button shows again whether it is on.** Pressed, it is filled with
  the accent colour on the meeting page and on the live page. The setting itself
  was saved all along; only the colour was missing. Reload the open page once.
- **Buttons that delete or stop are red again, and the Copilot panel's buttons
  small.** Delete on the meeting page, "Очистить профиль", "Сбросить сценарий"
  and the Copilot's Stop looked like ordinary buttons; Stop and "Открепить" were
  as large as the buttons of a page.
- **Every meeting's source is shown as a badge.** In the meetings list a source
  without a colour of its own (`callrec`, `obs-export`, `zoom-export`, `jitsi`,
  `vas3k`, any importer's) was plain text; it now has the grey badge that a
  meeting without a source had.

## 2026-11-10

### Changed

- **The installation's Claude and Codex models are settings of `config.toml`.**
  `[ai] claude_model` and `codex_model` name the model each provider's scenarios
  run on until Settings saves one; `CLAUDE_MODEL` and `CODEX_MODEL`, from the
  environment or `.env`, still override them, so nothing has to be moved. The
  summary and the title written when a meeting ends now use these models too:
  the live process read neither `.env` nor the file and ran on the shipped
  defaults, while the web server honoured `.env`. `.env.example` no longer sets
  `CODEX_MODEL`, so a new installation runs Codex on the shipped model. A `.env`
  created from the earlier example still holds the line: delete it if those
  summaries and titles should stay on the shipped model. A model with a space or
  a leading dash stops `./recog-service.sh start` with the key's name.
- **`setup.sh` no longer creates `recog/web/.env`.** No program reads that file:
  only the `.env` at the checkout's root is loaded. Its example
  `recog/web/.env.example` is gone, and with it the last tracked file of `recog/`.
  An existing `recog/web/.env` has no effect: `CLAUDE_MODEL`, `CODEX_MODEL` or
  `CODEX_REASONING_EFFORT` you meant to set there belong in the root `.env`, and
  `AI_PROVIDER` and the `OPENROUTER_*` keys are read by nothing. You may delete the
  file and the `recog/` directory, where only Python's `__pycache__` can remain;
  `scripts/backup-data.sh` still copies the file while it exists.

## 2026-11-09

### Action required

- **The Rust workspace is at the checkout's root.** `Cargo.toml` and `Cargo.lock`
  moved there from `rust/`, and the crates to `crates/`. After `git pull` run
  `./setup.sh` as usual: cargo now builds into `target/` at the root and compiles
  everything once more. To keep the dependencies' builds, move the old directory
  first (`mv rust/target target`); otherwise delete it, nothing reads it any more
  (`rm -rf rust`). cargo runs from the root without `cd rust` (`cargo test
  --workspace`), a relative `CARGO_TARGET_DIR` counts from the root, and the
  optional ASR server's programs are built into `target/release/`: a unit that
  starts them from `rust/target/release/` must name the new place.
- **The web UI, the prompts and the Swift sources lie in their crates.** What
  `recog/` held went to the crate it belongs to: `recog/web/static/` is
  `crates/recog-web/static/`, `recog/prompts/` is `crates/recog-ai/prompts/`,
  `recog/desktop/bootstrap.js` is `crates/recog-desktop/bootstrap.js`, and the Swift
  sources of `recog/native/macos/` are in `crates/recog-live/macos/` (the
  ScreenCaptureKit helper, the launcher) and `crates/recog-zoom/macos/` (the Zoom
  helper). The running services read the web UI and the prompts where they were, so
  the web page and the AI summaries fail between `git pull` and the restart: run
  `./setup.sh` right after the pull (it rebuilds and restarts what was running). A
  prompt of your own kept in `recog/prompts/` must move to `crates/recog-ai/prompts/`.
  The Swift code did not change: `Recog.app` and `RecogZoomHelper.app` are not rebuilt
  and keep their permissions; `sck_audio_capture` is compiled once more.
- **The quality gate measures Rust.** `RECOG_QUALITY_BASE=… ./scripts/check.sh` and
  `scripts/quality.py` (docs/quality.md) now score the CRAP and duplicates of the
  production Rust under `crates/<crate>/src/` (test modules left out) next to the
  JavaScript, with coverage from `cargo llvm-cov` over the workspace's tests; the
  Python coverage is gone and pytest runs once, pass or fail. A contributor running
  the gate needs `cargo install cargo-llvm-cov` (CI pins 0.9.1) and `rustup component
  add llvm-tools-preview` (a toolchain without rustup: point `LLVM_COV` and
  `LLVM_PROFDATA` at its own `llvm-cov` and `llvm-profdata`), then `python3 -m pip
  install -r deploy/python/requirements-quality.txt` again: `pytest-cov` and
  `coverage` left it, `pytest` and `tree-sitter-rust` are pinned in it.
- **Rebuild and reload the browser extension.** Its debug panel put the current
  speaker's name and the meeting URL into the conference page as markup, so a
  participant whose display name held HTML could run script on that page; the
  popup did the same with names, times and transcript text. Both now insert them
  as text. `./setup.sh` rebuilds `browser-extension/dist/`; then press Reload on
  the extension in `chrome://extensions`.

### Changed

- **Meeting page: playing a segment while another is still loading** no longer
  leaves the first play button disabled until the page re-renders; it returns to
  play at once.
- **A KTalk participant's transcript audio stays on one source.** Once the
  extension had moved a participant's audio to the call's receiver track, every
  later update of that participant's track moved it back to the stream wrapping
  the track, and the 4 s pause between moves kept it there, so the source flipped
  at every update. It now stays on the receiver track while that track lives.
  Rebuild and reload the extension: `./setup.sh`, then Reload in
  `chrome://extensions`.
- **A corrected line in the floating panel loses its `(?)` mark.** When the
  bridge named the speaker of an uncertain line on Jitsi, Element Call, Google
  Meet or Zoom, the panel renamed the line but left `(?)` or `(🔇?)` after its
  text. Rebuild and reload the extension as above.
- **The extension attaches to a call once.** About 2 s into every call it let go
  of everything it had attached to the call and attached it again; on KTalk that
  tore down and rebuilt every participant's transcript audio, dropping what had
  not yet been sent. It now attaches once and again only when the call changes or
  ends. Rebuild and reload the extension as above.
- **`meetings/.lock` in the data directory.** The live process and the web server
  now rewrite a meeting's `meta.json` and `index.json` in turn, under an `flock` of
  this empty file, so a title stored from the web UI no longer undoes what the live
  process wrote at the same moment, or the reverse. Nothing to do; leave the file.

## 2026-11-08

### Action required

- **recog no longer installs or runs Python.** The Python modules that had a Rust
  replacement are gone, and so is the Python installation: `./setup.sh` always
  builds the programs of `rust/` with cargo into `bin/` and the units run them (what
  `[install] runtime = "rust"` did). After `git pull`: install Rust with
  [rustup](https://rustup.rs) if `cargo` is missing, delete `[install] runtime =
  "python"` from `~/.config/recog/config.toml` if you set it (setup stops on it),
  run `./setup.sh`, then check `./recog-service.sh status`. `venv-gigaam` is no
  longer used and can be deleted once the services run on the new units. The
  `runtime` keys of `[live]`, `[web]` and `[copilot]` and `[live] mic_worker` are
  still accepted and do nothing; ASR runs on wavo, or with `[asr] backend =
  "remote"` on the gateway with wavo as its fallback, and `"local"` is ignored with
  a line in the live log. What stays in
  Python is not part of the installation: the quality gate's tools and the
  maintenance scripts under `scripts/`.
- **The optional ASR server is Rust only.** `recog/asr_server/` is gone with its
  requirements file (`deploy/python/asr-server.txt`), and with it speaker
  diarization. A server still started as `python -m recog.asr_server.gateway` or
  `python -m recog.asr_server.gigaam_backend` stops starting after `git pull`: build
  `cd rust && cargo build --release -p recog-asr-server` and start
  `rust/target/release/recog-asr-gigaam` and `recog-asr-gateway` instead, with the
  same environment (docs/asr-server.md); its venv can then be deleted.

### Changed

- **The optional ASR server without Python (opt-in).** `rust/recog-asr-server`
  builds `recog-asr-gateway` and `recog-asr-gigaam`, which replace `python -m
  recog.asr_server.gateway` and `python -m recog.asr_server.gigaam_backend` with the
  same environment variables, ports and answers; the backend runs GigaAM on wavo and
  downloads its model into the Hugging Face cache on first start. They have no
  speaker diarization: `/healthz` says `"diarization": false` and diarized requests
  get 503, as from the Python backend without pyannote. Nothing changes for an
  existing server until you switch: `cd rust && cargo build --release -p
  recog-asr-server`, then start the binaries from `rust/target/release/` instead of
  the Python modules (docs/asr-server.md).

- **The Rust live process runs remote ASR.** With `[asr] backend = "remote"` the
  Rust live process now sends speech to the gateway of `[asr] remote_url`, falling
  back to wavo while the gateway is down, as the Python one falls back to PyTorch. Before, `[live] runtime = "rust"` stayed on Python with this backend, and
  `[install] runtime = "rust"` ignored it and ran wavo. Only `backend = "local"` now
  keeps the live process in Python. After `git pull`: `./setup.sh` (it rebuilds the
  binary), then `./recog-service.sh restart`. Without `remote_url` the readiness
  check refuses to start the services in both installations.

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
