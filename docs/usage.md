# Installation and usage reference

See the [quick start](../README.md#установка) for the shortest installation path.
Commands below run from the repository root.

## Requirements

| | macOS | Linux |
|---|---|---|
| OS | 13+, Apple Silicon | any distro with systemd (Ubuntu/Debian, Arch/Manjaro tested) |
| Build | `cargo` from [rustup](https://rustup.rs) | the same |
| System audio | ScreenCaptureKit — built in, needs `swiftc` (`xcode-select --install`) to build the helper | PulseAudio **or** PipeWire with `pipewire-pulse`, plus `parec`/`pactl` (`pulseaudio-utils` / `libpulse`) |
| Microphone | built in (`recog-mic`) | `parec` on the default source |
| Required | `ffmpeg` | same |
| Optional | `tmux` with `claude` or `codex` (Copilot), `node`+`npm` (extension build, AI summaries), `jq` (`debug` output) | same |
| Disk | ~0.3 GB for the model, ~3 GB for the Rust build | same |
| GPU | Metal | Vulkan |

`cargo setup` prints a found/missing table for all of the above with the install
command for your package manager (apt / pacman / brew), so you do not have to
pre-install anything by hand — run it first and let it tell you what is
missing. There is no Python to install.

## Quick start

```bash
git clone https://github.com/materkey/recog.git && cd recog
cargo setup
# → chrome://extensions → Developer mode → Load unpacked →
#   browser-extension/dist
# → open http://127.0.0.1:9080
```

`cargo setup` builds the programs with cargo into `bin/`, copies `.env.example` →
`.env`, builds the Chrome extension, downloads the ASR model, renders the service
units and starts them. It is idempotent: re-running it is how you
update after a `git pull`. An existing `.env` is preserved.

If something required is missing, setup renders the units but **does not
activate anything** and exits non-zero (previously running services are left
running). Install what it lists and run `cargo setup` again.

Flags: `--dry-run` (show the plan without changes; does not verify readiness),
`--no-services` (prepare only, do not touch launchd/systemd),
`--no-prefetch` (skip the model download), `--no-extension`,
`--no-env` (keep an existing `.env`, do not create it from the example).

### Install-time environment variables

These are read from the calling shell, not from `.env` (`.env` is created by
setup itself, later than these are needed):

- `CARGO_TARGET_DIR` — where cargo builds (default `target`); setup takes
  the programs from its `release/`.
- `RECOG_INSTALL_RUNTIME` — `[install] runtime` for one run; only `rust` is left.

### Installing without Python

Every service is a program of `crates/`, and recog has no Python. `cargo setup`:

1. builds `recog`, `recog-web`, `recog-copilot-supervisor`, `recog-copilot-channel`,
   `recog-desktop` and, on macOS, `recog-mic` with `cargo build --release` (the first
   build takes several minutes);
2. checks the configuration with the `recog` just built (`recog check`: `config.toml`,
   `.env`, the user profile, the AI settings) and stops, with nothing installed, on a
   problem;
3. copies the programs into `bin/` of the checkout (gitignored), each renamed over
   the old one, so a running service keeps the file it started from, and a cleaned
   `target` cannot take them away;
4. builds the extension (the hosts come from `bin/recog config conferences`),
   `sck_audio_capture` and `Recog.app` (`bin/recog launcher`, rebuilt only when its
   source changed, so the permissions of "Recog" stay), downloads the ASR model
   `gigaam-v3` into the Hugging Face cache (`bin/recog prefetch`), and installs and
   starts the units.

The units then run the binaries: the live one as
`Recog.app/Contents/MacOS/recog_launcher bin/recog live --system-audio auto --ktalk`
on macOS, the web server as `bin/recog-web --host 127.0.0.1 --port 9080 --repo
<checkout>`, the Copilot ones as `bin/recog-copilot-{supervisor,channel} --repo-root
<checkout>`. `recog doctor` reports readiness and `recog service` manages units.
The configuration parser accepts legacy per-service runtime keys. The installation
runtime must be `rust`.

An installation that ran on Python: install Rust, delete `[install] runtime =
"python"` if you set it, run `cargo setup` and check `recog service status`;
`venv-gigaam` can then be deleted.

## Usage

```bash
# Live: mic + system audio + speaker names from the conference extension.
# The mic is on by default (--mic-device auto); --mic-device none turns it off.
bin/recog live --ktalk --system-audio auto

# Capture only while joined to a KTalk meeting (implies --ktalk)
bin/recog live --ktalk-only --system-audio auto

# System audio only
bin/recog live --ktalk --system-audio auto --mic-device none

# Microphone only, labelled with your profile name
bin/recog live --system-audio none

# What audio devices do I have?
bin/recog live --list-devices


# Browse stored meetings
bin/recog meetings
```

`--system-audio` takes `auto` (per OS), `screencapture` (macOS,
ScreenCaptureKit), `pulse` (Linux, PulseAudio/PipeWire), `device:<id>` (an
input device, `device:auto` looks for BlackHole), or `none` (default).
`--mic-device` takes a device id, `auto` (default input), or `none`.

The startup banner prints which sources are live, naming the resolved mic
device and the backend in full, e.g.
`Mode: DUAL (Mic: 3 + System: PulseAudio/PipeWire (parec))`.

### Recording during conferences

A newly installed live service runs with `--conferences-only`. Both audio inputs stay
closed until a supported KTalk, Jitsi, Element Call, Google Meet or Zoom call joins.
Leaving or disconnecting the owning conference closes both inputs. Updating an
existing live service preserves its recording mode.

### Recording only during KTalk calls

`--ktalk` alone records continuously, including between conferences.
Use `--ktalk-only` to keep both microphone and system capture closed until
an accepted KTalk `meeting_join` arrives from the Chrome extension. Leaving
the call, closing its tab, losing the owner WebSocket, or stopping the bridge
closes capture and discards buffered audio and pending recognition results.
An idle KTalk tab, a Jitsi/Element/Google Meet/Zoom meeting, or a manual Copilot
session cannot start recording in this mode. The extension must be installed and enabled;
the native KTalk application does not supply these browser events.
Rebuild and reload the extension together with the backend: KTalk membership
is checked through Jitsi's `isJoined()` API, including after a WebSocket
reconnect. A missing or failing membership API keeps recording off.
If a capture device cannot start, the join is rejected, the live status stays
inactive, and the extension retries while the user remains in the call.

The policy follows call membership, not working hours or calendar titles.
While joined, the selected microphone and system output are still captured,
including unrelated sounds; muting in KTalk suppresses mic transcription but
does not close the local microphone or exclude its audio from the saved WAV.
Leave the call to stop capture. An undetected disconnect stops capture when
the WebSocket keepalive times out; the bridge cannot react before it learns
that the call ended. Call membership is polled every two seconds (browser
timer throttling can delay this). The last partial chunk is discarded at the
boundary, together with queued live/refine work. Already running remote ASR
requests cannot be recalled; their results and retries are discarded.

Manual acceptance requires the updated backend and extension in a real KTalk
browser call: check idle/prejoin, join, leave without closing the tab, and
rejoin after a bridge reconnect. `/api/live/status` must be inactive outside
the call, the backend's microphone/system capture must be closed, and WAVs
must stop growing after leave. Use a test call and an isolated data directory;
automated lifecycle tests replace the audio devices and ASR.

For an installed service, add `--ktalk-only` to its live command. On Linux,
use a `systemctl --user edit recog-live.service` override (replace the paths):

```ini
[Service]
ExecStart=
ExecStart=/absolute/path/recog/bin/recog live --ktalk-only --system-audio auto
```

Then run `systemctl --user daemon-reload` and
`systemctl --user restart recog-live.service`. On macOS, add a
`<string>--ktalk-only</string>` item to the live plist's `ProgramArguments`
and reload the service. Re-rendering that plist replaces manual edits.

### Output format

```
[21:15:00] Speaker One    : Первая фраза спикера
                            Продолжение той же реплики
[21:15:30] Speaker Two    : Другой спикер начинает говорить
```

The timestamp and the name are printed only when the turn changes; a turn ends
on a different speaker or a pause longer than 3 seconds, continuation lines are
indented.

## System audio

### macOS

ScreenCaptureKit is the default (`--system-audio auto` → `screencapture`): no
virtual device, no rerouting of your output. The first run compiles the
`sck_audio_capture` helper with `swiftc`, and macOS asks for **Screen
Recording** permission (System Settings → Privacy & Security → Screen
Recording) — without it the helper produces silence.

The live service asks as **Recog**. The macOS release ships its signed launcher
and the optional Zoom helper. A checkout builds them locally. Setup keeps a
valid installed bundle with matching inputs at the same path so that an
ordinary update preserves its grants. Changing the app itself can require a
new grant with ad-hoc signing.

Run `recog setup --permissions` on the Mac to reopen the native permissions
window, or open **Settings → Готовность установки → Разрешения macOS…** in
the local web interface. Each row explains its use and links to its System Settings pane.
The microphone request belongs to Recog. Screen & System Audio Recording
accepts the Recog app icon, and Accessibility accepts the separate Zoom helper
when native Zoom detection is enabled. Remote terminals use the text guide.

BlackHole still works as an alternative if you prefer a loopback device:
install it, create a Multi-Output Device in Audio MIDI Setup, then run with
`--system-audio device:auto` (or `device:<id>` from `--list-devices`).

### Linux

`--system-audio auto` → `pulse`: `parec` reads the **monitor** of your current
output. Works on PulseAudio and on PipeWire through `pipewire-pulse`.

Without an override the capture follows the default sink: it resolves the
current monitor itself at start and re-connects when you switch outputs (a
single `@DEFAULT_MONITOR@` name does not guarantee that on its own). To pin one
specific monitor instead:

```bash
pactl list short sources          # find the *.monitor entry you want
# .env:
RECOG_PULSE_MONITOR_SOURCE=alsa_output.pci-0000_00_1f.3.analog-stereo.monitor
```

With the override set, the automatic follow-the-default-sink behaviour is off.

## Services

`recog service` manages four permanent services — live transcription, the
web UI, the copilot MCP channel and the copilot supervisor — through launchd on
macOS and `systemd --user` on Linux. The units are rendered from the templates
in `deploy/{macos,linux}/` with your repo path, the programs in `bin/` and log
directory substituted in.

```bash
recog service install    # render + register the units, WITHOUT starting them
recog service start      # start (after a dependency readiness check)
recog service status     # PIDs of all installed services
recog service restart    # pick up new code / a re-rendered unit
recog service logs       # tail live transcription logs (logs frontend, logs copilot-supervisor, …)
recog service uninstall  # stop and remove
```

`install` deliberately never activates anything — that is what lets `cargo setup`
verify dependencies before starting a pipeline that cannot work. `start`,
`restart` and the targeted `restart frontend` / `restart live` / `restart copilot-channel` /
`restart copilot-supervisor` all run the same readiness probes as setup and refuse to
activate an incomplete install rather than take a working one down.

On Linux, add `loginctl enable-linger $USER` so the user services survive
logout — `status` prints the reminder. After rebuilding the extension, reload
it in `chrome://extensions`.

After changing code in `crates/` run `cargo setup --no-extension` (it rebuilds and
reinstalls `bin/`) and `recog service restart`; for the supervisor alone,
`recog service restart copilot-supervisor` is enough.

## Questions about a meeting

**Ask** on a meeting page opens a panel beside its transcript: ask what someone
said or what was decided, and follow up. The answer comes from the transcript alone,
and each time in it is a button that scrolls the transcript to that replica, so a
quote can be checked by eye. It works during a meeting too, on what has been said so
far.

The dialog is saved in the meeting's `chat.jsonl` and is deleted with the meeting or
by **Начать заново**. A question is a single call of the provider's CLI, like AI
Analysis, not the Copilot session, so it needs no running Copilot. Its prompt, provider
and model are the **Вопросы по встрече** scenario in the AI settings; the default is
Claude. The transcript goes to the model with real speaker names even when **Privacy**
is on, and a meeting over 300,000 characters reaches it only by its end, which the
answer then says.

## Copilot

The **Privacy** toggle in the live and meeting views replaces speaker names with
`speaker_N` in Copy/Export and in Copilot MCP transcript snapshots, queued events
and push notifications. MCP receives a masked user profile with no real-name
aliases. The setting is shared across browsers and persisted in `privacy.json`
under the data directory; a running MCP channel reads it before each delivery.
Transcript text, meeting titles and custom instructions stay unchanged, including
names written or spoken inside them. Saved recordings retain their original
labels, and enabling Privacy cannot retract context already sent to an agent.

All four units — including the copilot supervisor — are installed everywhere;
there is no separate flag. What is off by default is the *automatic* spawn per
meeting: `.env.example` ships `RECOG_COPILOT_AUTOSPAWN=0`, so a fresh install
never starts a Claude session unattended. The manual Start/Stop button and the
agent-status chip in the web UI work regardless. Above both stands **AI-функции**
in **Settings → Промпты и модели**: the Copilot runs only at **Только по кнопке**
or higher, and starts for a conference only at **По кнопке и после каждой
встречи**.

To use Copilot, install `tmux` and Claude Code or Codex and make the commands
available on `PATH`. Setup preserves their discovered directories in the service
PATH, including a custom npm prefix. Run `claude` or `codex` interactively as the
same user to sign in before starting Copilot; setup checks CLI availability, not
authentication. After moving or installing a CLI in a new directory, rerun
`cargo setup`.

Copilot runs on the first provider of its scenario in **Settings → Промпты и
модели** whose CLI is installed: Claude Code by default, Codex when Claude Code
is missing. On Codex the tmux session runs `recog-copilot-codex`, which drives
`codex app-server` with its own `CODEX_HOME` under `<data dir>/copilot/`: its
`auth.json` links to your `~/.codex/auth.json`, so your login holds while your
personal `AGENTS.md` and MCP servers stay out of the meeting. The thread asks for
no approvals, has a read-only sandbox and only the Copilot channel's tools. The
session's last line is its status (`recog-copilot-codex: online`); `tmux -L
recog-copilot attach` shows the dialog above it, and Codex's own log is
`<data dir>/logs/copilot-codex.stderr.log`. A spent subscription or an expired
login shows as 🔴 with the reason on that line.

Setup reports missing Copilot tools as optional. Transcription and service management
work without them. While `tmux` or the Copilot's CLI is missing the supervisor
starts no session for a meeting and its log says which one.

To enable auto-spawn: put `tmux` and the `claude` or `codex` CLI on `PATH`, set
`RECOG_COPILOT_AUTOSPAWN=1` in `.env`, then
`recog service restart copilot-supervisor`.

On both operating systems `restart` and `restart copilot-supervisor` keep a live
copilot session — on Linux the session runs in its own transient
`recog-copilot-session.service`, outside the supervisor's cgroup, and the
restarted supervisor re-adopts it. An explicit Stop in the web UI (or
`recog service stop` / `uninstall`) ends it.

## Remote ASR

Instead of running GigaAM locally the live process can send speech to your own
OpenAI-Whisper-compatible ASR gateway. The optional server implementation is
the crate `crates/recog-asr-server`; see the [ASR server guide](asr-server.md) for
installation and configuration:

```dotenv
RECOG_ASR_BACKEND=remote
RECOG_ASR_REMOTE_URL=https://<asr-gateway>/asr
```

There is no default URL: `remote` without a URL fails at startup instead of
silently running on-device with different RAM and latency. The local model,
wavo, stays unloaded while the gateway is healthy and loads lazily on the first
failure; a circuit breaker routes around a dead gateway. Only worth it on a
real LAN (`ping` RTT < 5 ms).

## ASR on wavo

The live process runs GigaAM v3 e2e_rnnt on [wavo](https://crates.io/crates/wavo),
a Rust runtime, on the GPU: Metal on macOS, Vulkan on Linux, for every `[asr]
backend` but `remote`, where it is the gateway's fallback. `cargo setup` downloads the model (274 MB, Q8_0 GGUF) into the
Hugging Face cache (`bin/recog prefetch`); without it the first start does.

Measured against the PyTorch model (`docs/experiments/2026-11-07-rust-asr-runtimes.md`):
the same WER within 0.06 points and the same word timings, 6-12 times faster on an
M3 Pro, 4.5 times on an RTX PRO 6000, about half the memory. It has no batch (the
per-speaker clips run one after another) and a refine runs to its end instead of
giving way to live mid-decode; at 60-100 ms per refine neither matters.

## Your name and Copilot profile

On your first web UI visit, **Как к вам обращаться?** asks for your name and
optional forms of address (one per line). **Позже** keeps recording and manual
Copilot chat available. Open **Settings → Мой профиль** later to edit or clear
the profile. A non-generic existing `MIC_SPEAKER_NAME` pre-fills the form but is
not saved until you choose **Сохранить**.

The profile is private data in `user-profile.json` under the Recog data directory,
included in backups and data migration. The web UI never modifies `.env`.
New microphone segments and Copilot events use the new profile without restarting
services or the agent. If Live is disconnected, the profile is saved and the UI
shows that application is pending; it is reapplied on reconnect. Past transcripts
keep their original speaker labels.

Name precedence is an explicit `--mic-speaker-name` for this run, then the saved
profile, then `MIC_SPEAKER_NAME`, then `Me`. Configured aliases accompany Copilot
events; clearing/changing them replaces the previous set rather than accumulating
old nicknames. Without a known name, name-addressed proactive replies stay off.

## Environment variables

Settings other than secrets belong in the
[installation configuration](configuration.md) `~/.config/recog/config.toml`:
copy `deploy/config.example.toml` there, it explains each one. Each setting is
the default of the variable below, so a variable in the environment or in
`.env` (setup copies `.env.example`, which keeps secrets and legacy fallbacks)
still overrides it. The ones you are most likely to touch:

- `MIC_SPEAKER_NAME` — legacy microphone-name fallback before a profile is saved (default `Me`).
- `RECOG_ASR_DEVICE` — `auto` (cuda > mps > cpu) | `cuda` | `mps` | `cpu`. An
  explicitly requested device that is unavailable fails fast instead of quietly
  landing on CPU.
- `RECOG_TORCH_THREADS` — cap `torch.set_num_threads()` on a CPU-only box.
- `RECOG_PULSE_MONITOR_SOURCE` — pin one PulseAudio monitor (Linux).
- `RECOG_EXTRA_WS_ORIGINS` — comma-separated extra origins allowed to open the
  bridge / live-stream WebSockets. The shipped conference hosts are built in
  (`crates/recog-core/src/ws_origin.rs`, mirroring the extension's `manifest.json`); if your
  conferences live on another host, add it **both** here and to the extension's
  `manifest.json` (that is a separate rebuild).
- `RECOG_ASR_BACKEND`, `RECOG_ASR_REMOTE_URL` — see [Remote ASR](#remote-asr) and
  [ASR on wavo](#asr-on-wavo).
- `RECOG_COPILOT_AUTOSPAWN`, `RECOG_COPILOT_GRACE_S`, `RECOG_COPILOT_MCP_URL` —
  see [Copilot](#copilot).
- `CLAUDE_MODEL` and `CODEX_MODEL` (`[ai] claude_model` and `codex_model` of
  `config.toml`) — the installation's models, used when no corresponding AI
  settings override has been saved.

Open **Settings → Промпты и модели** to configure every LLM scenario: AI
Analysis, meeting titles, saved summary templates and live Copilot.
Set common model defaults, then override the prompt, provider and model for
individual scenarios. Claude defaults to `claude-opus-5-5`, Codex to
`gpt-6-astra`; the installation's models (`[ai]` of `config.toml`) remain
effective until overridden in Settings. Claude and Codex continue to use their CLI subscription login.

**AI-функции**, at the top of the dialog, decides whether Recog sends transcripts
to Claude Code or Codex at all; the choice is saved at once, as `access` in
`ai-settings.json`:

- **Выключено** — nothing is sent. Ask, Generate Title, Recog Copilot and the
  controls of the AI Analysis window that start a generation are hidden; saved
  summaries stay readable. A request that gets through is refused with
  «AI-функции выключены в настройках recog.», and a running Copilot stops.
- **Только по кнопке** — AI Analysis, Ask, Generate Title and a Copilot started
  from the web UI run when you start them. Nothing runs by itself after a
  meeting and the Copilot does not start for a conference; a Copilot already
  running for one keeps running until the meeting ends. The bridge refuses a
  manual Copilot start while a conference is on, so at this level there is no
  Copilot during a conference unless it was running before.
- **По кнопке и после каждой встречи** — also the saved summaries and the title
  after each meeting, and the Copilot's conference autostart (still behind
  `RECOG_COPILOT_AUTOSPAWN`).

A new installation has no level yet: the web UI asks right after the profile
question and on the first click of an AI control, and nothing is sent until you
answer («Позже» asks again in the next browser session). An installation that
already has a finished recording keeps the last level, which is what it did
before the choice existed.

Meeting titles, questions about a meeting and saved summaries take one answer.
At their default they ask one provider (Codex for titles and summaries, Claude
for questions) and, when its CLI is not installed or it refuses, for example on
a spent subscription, the other one; the title, the answer and the summary
record which provider gave them. Recog therefore works with only Claude Code
or only Codex, Copilot included (see [Copilot](#copilot)). Clear **Если провайдер не
установлен или отказал, ответит другой** in a scenario to send its transcripts
to the chosen provider only.

The **Инкремент** scenario configures both new analysis and saved increments.
New analysis compares the selected providers; a saved version asks Codex first
when it is selected, then the other selected provider. Existing saved versions keep
their history under the same prompt ID.

Claude and Codex have separate reasoning effort controls. Claude supports
`low`, `medium`, `high`, `xhigh`, `max`. Both providers offer Auto, which lets
their CLI choose the reasoning effort.
Copilot applies effort changes, like model changes, at the next session start.

The Copilot scenario also has an **Copilot включён** switch. Save it off to stop
the active agent and block manual starts, conference starts and crash retries.
Transcription continues. The supervisor checks the saved switch on startup and
every two seconds even while disconnected from the bridge; enabling it permits
manual starts and the existing conference autostart policy again, as far as
**AI-функции** allows.

Changes apply to the next request; an in-flight generation keeps its original
settings. Copilot receives edited instructions on its next event, while its
model changes on the next session start. Saving does not restart a live session.
Templates support `{transcript}`, `{user_name}`, `{participants}` and
`{meeting_title}`; the editor lists required fields. Copilot instructions are
literal text. Reset restores the selected scenario's shipped defaults.

Overrides live in private `ai-settings.json` under the data directory, alongside
the user profile, and are included in data backups. They survive updates without
changing `.env` or the shipped prompt files. Saving checks the revision to avoid
overwriting changes made in another tab. ASR retains its existing configuration.

The per-speaker ASR path (per-participant PCM streams from the extension) and
the tier-2 refine pass are always on for conference meetings — no flags. See
`CLAUDE.md` for the architecture notes.

## Troubleshooting

**No system audio in the transcript (Linux).** `pactl list short sources` — is
there a `*.monitor` source, and is `pipewire-pulse` (or PulseAudio) running?
`parec --raw --format=float32le --rate=16000 --channels=1 --device=<monitor> | head -c 100000 > /dev/null`
should produce data while something is playing. Without `--device`, `parec`
records the default *source* (your microphone), which is why the monitor name
matters.

**No system audio (macOS).** Check Screen Recording permission for **Recog**
(the service) or for the terminal (a manual run), and that `sck_audio_capture`
and `Recog.app` exist — `cargo setup` builds both when `swiftc` is available.
After an update that changes the app's ad-hoc signature, System Settings can
still show Recog as enabled while `recog doctor` reports missing system audio.
In **Privacy & Security → Screen & System Audio Recording**, select the Recog
entry, remove it with **−**, then use **+** to add the current `Recog.app`.
Run `recog doctor` again. Microphone access has its own consent prompt in
`recog setup --permissions`.

**Setup says the install is not complete.** Nothing was activated on purpose.
Missing `ffmpeg`, `parec`/`pactl`, PortAudio, or a macOS helper that will not
build (`swiftc`) all block activation, as does `RECOG_ASR_BACKEND=remote` with
an empty URL. Fix what it lists and re-run `cargo setup`. The same probes gate
`recog service start|restart|restart-*`; if you deliberately want a subset
(the web UI on a box with no audio stack, say), `RECOG_SKIP_READINESS=1
recog service start` bypasses the gate.

**The extension never connects.** The WS handshake is Origin-gated. If your
conference host is not one of the built-in ones, add its exact HTTPS origin to
the [installation configuration](conference-hosts.md), rebuild the extension
(`cd browser-extension && npm run build`), restart Recog and reload
`browser-extension/dist/` in `chrome://extensions`.

## Personal data outside the checkout

New installs store meetings in `~/.recog/meetings`, service logs in
`~/.recog/logs`, and profile/AI settings in the same data directory.
`RECOG_DATA_DIR` overrides that root; use an absolute path (or `~/…`), since
data paths must not depend on the caller's working directory. `.env` stays
in the checkout. Historical speaker database files are
preserved by data migration and backup.

Existing installs continue using their entire old data root and print a
migration hint. If both the checkout and `~/.recog` contain data, startup
refuses to choose silently. An explicit `RECOG_DATA_DIR` selects that root
without legacy detection. All programs share the data-root resolver;
service installation records the resolved root in the units, so re-run
`install` and `restart` after changing it.

To migrate an existing install, finish the current recording and stop any
manually launched recording or speaker-enrollment commands, then run:

```bash
bin/recog migrate-data  # inspect the proposed moves
recog service stop
bin/recog migrate-data --apply
recog service install
recog service restart
```

Migration moves `meetings/`, `logs/`, and every `speakers*.json` using renames
on the same filesystem. It leaves no old directories or symlinks, does not
copy data, and refuses collisions or a destination inside the checkout.
An interrupted migration can be re-run to move the remaining entries; it
never merges two meeting archives or overwrites a database. `cargo setup` does
not migrate existing data. `recog backup` backs up the resolved
data root, the [installation configuration](configuration.md) `config.toml`
(as `config.toml` in the snapshot) and the environment files in the checkout
(`--with-logs` includes logs).

## Updating an existing install

1. Read the [changelog](../CHANGELOG.md) from the date of your last update.
   Its **Action required** items say what each update asks of an existing
   installation: moved settings or files, a new dependency, a setup or service
   step.
2. `recog update` — cargo rebuilds the programs into `bin/` (a running
   service keeps its file until it is restarted) and the model cache is reused.
   Setup also rebuilds the extension: reload it in `chrome://extensions`.
3. Do the changelog's steps, then `recog service restart`. If you skipped
   services during setup, run `recog service install` first: the units are
   rendered from the templates. Startup reports a malformed setting or a
   setting `.env` overrides in `config.toml`.

## Testing

```bash
./scripts/check.sh                    # everything below, in one go
```

`scripts/check.sh` is what CI runs on macOS and Linux: `compileall` of what is
left in Python, `bash -n` and shellcheck over the shell scripts, actionlint over
the CI workflow, a gitleaks secret scan, `cargo fmt`, clippy and the tests of
`crates/`, the CLI smoke test (`recog --help`), a dry-run render of the service
units, and the Python suite (the shell scripts' and the quality tools' tests). The linters come from the quality tools in
[docs/quality.md](quality.md). It starts no service, captures no audio and loads no
model weights — those belong to manual acceptance on a real host.

The individual steps:

```bash
cargo test --workspace                   # the Rust tests
python -m pytest tests/ -v -m "not e2e"  # the Python tests
cargo run -q -p recog-live --bin recog -- --help  # CLI smoke test
```
