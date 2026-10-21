# Installation and usage reference

See the [quick start](../README.md#installation) for the shortest installation path.
Commands below run from the repository root.

## Requirements

| | macOS | Linux |
|---|---|---|
| OS | 13+, Apple Silicon (Intel is not supported: no macOS x86_64 torch wheels past 2.2.2) | any distro with systemd (Ubuntu/Debian, Arch/Manjaro tested) |
| Python | 3.12 | 3.12 — see [Python on Linux](#python-on-linux) |
| System audio | ScreenCaptureKit — built in, needs `swiftc` (`xcode-select --install`) to build the helper | PulseAudio **or** PipeWire with `pipewire-pulse`, plus `parec`/`pactl` (`pulseaudio-utils` / `libpulse`) |
| Microphone | built in (PortAudio ships with the wheel) | PortAudio (`libportaudio2` / `portaudio`) |
| Required | `ffmpeg`, `tmux`+`claude` (Copilot) | same |
| Optional | `node`+`npm` (extension build, AI summaries), `jq` (`debug` output) | same |
| Disk | ~4 GB for models | ~4 GB for models |
| GPU | Apple Silicon → MPS automatically | NVIDIA → CUDA automatically; CPU works too (measured RTF 0.05–0.2) |

`./setup.sh` prints a found/missing table for all of the above with the install
command for your package manager (apt / pacman / brew), so you do not have to
pre-install anything by hand — run it first and let it tell you what is
missing.

### Python on Linux

Use **Python 3.12**, the version used in CI. Setup tries `python3.12`,
`python3.13`, then `python3`; it rejects versions below 3.12 and warns above
3.13. Acceptance by the installer is not a dependency compatibility guarantee.

**Arch Linux:** the stock Python 3.14.7, tested on 2026-09-19, fails during GigaAM installation
with `No matching distribution found for onnxruntime==1.23.*`. Use Python 3.12
via uv below; upgrading the system `python` package does not solve this pin.

On Ubuntu 22.04, the stock Python is 3.10 and the default repositories do not
provide `python3.12` / `python3.12-venv`. If the required Python is unavailable,
install it for your user with [uv](https://docs.astral.sh/uv/guides/install-python/)
and rerun setup from the repository root (Bash):

```bash
# Requires curl and CA certificates; skip the first line if uv is installed.
curl -LsSf https://astral.sh/uv/install.sh | env UV_UNMANAGED_INSTALL="$HOME/.local/bin" sh
export PATH="$HOME/.local/bin:$PATH"
uv python install 3.12
python3.12 --version
./setup.sh
```

Repeat the `PATH` line in a new shell if `~/.local/bin` is not already on your
PATH. The system `python3` stays unchanged.

If a failed attempt already created `venv-gigaam` with another Python version,
setup will reuse it. With services and manual Recog processes stopped, preserve
that environment and let setup create a new one (choose an unused backup name):

```bash
mv venv-gigaam venv-gigaam.previous
./setup.sh
```

The Python 3.12.14 dependency installation, `pip check`, CLI and ASR/web/MCP
imports passed in clean Ubuntu 22.04 and Arch containers on 2026-09-19; audio
capture and service activation were not part of these checks.

## Quick start

```bash
git clone https://github.com/materkey/recog.git && cd recog
./setup.sh
# → chrome://extensions → Developer mode → Load unpacked →
#   browser-extension/dist
# → open http://127.0.0.1:9080
```

`./setup.sh` creates `venv-gigaam`, installs torch/torchaudio and GigaAM (from
a pinned git commit — no sibling checkout needed), copies `.env.example` →
`.env`, builds the Chrome extension, prepares the GigaAM weight cache, renders
the service units and starts them. It is idempotent: re-running it is how you
update after a `git pull`. Existing environment files are preserved.

If something required is missing, setup renders the units but **does not
activate anything** and exits non-zero (previously running services are left
running). Install what it lists and run `./setup.sh` again.

Flags: `--dry-run` (show the plan without changes; does not verify readiness),
`--no-services` (prepare only, do not touch launchd/systemd),
`--no-prefetch` (skip the weight-cache preparation), `--no-extension`,
`--no-env` (keep existing environment files, do not create them from the examples).

### Install-time environment variables

These are read from the calling shell, not from `.env` (`.env` is a Python-side
file created by setup itself, later than these are needed):

```bash
RECOG_TORCH_INDEX_URL=https://download.pytorch.org/whl/cu124 ./setup.sh
```

- `RECOG_TORCH_INDEX_URL` — pip index for torch/torchaudio. On Linux without an
  NVIDIA GPU setup defaults to the CPU index (plain PyPI torch drags in ~2–3 GB
  of CUDA runtime). Set it to your CUDA index for a GPU box, or to an empty
  string to force plain PyPI.
- `RECOG_GIGAAM_URL` / `RECOG_GIGAAM_REF` — git source and commit of GigaAM
  (defaults: upstream at the pinned commit the ASR code is written against).
- `RECOG_GIGAAM_EDITABLE` — path to a local GigaAM checkout to install with
  `pip install -e`, for hacking on the ASR library itself.

## Usage

```bash
source venv-gigaam/bin/activate

# Live: mic + system audio + speaker names from the conference extension.
# The mic is on by default (--mic-device auto); --mic-device none turns it off.
python -m recog live --ktalk --system-audio auto

# Capture only while joined to a KTalk meeting (implies --ktalk)
python -m recog live --ktalk-only --system-audio auto

# System audio only
python -m recog live --ktalk --system-audio auto --mic-device none

# Microphone only, labelled with your profile name
python -m recog live --system-audio none

# What audio devices do I have?
python -m recog live --list-devices


# Browse stored meetings
python -m recog meetings
```

`--system-audio` takes `auto` (per OS), `screencapture` (macOS,
ScreenCaptureKit), `pulse` (Linux, PulseAudio/PipeWire), `device:<id>` (an
input device, `device:auto` looks for BlackHole), or `none` (default).
`--mic-device` takes a device id, `auto` (default input), or `none`.

The startup banner prints which sources are live, naming the resolved mic
device and the backend in full, e.g.
`Mode: DUAL (Mic: 3 + System: PulseAudio/PipeWire (parec))`.

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
ExecStart=/absolute/path/recog/venv-gigaam/bin/python -m recog live --ktalk-only --system-audio auto
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

The service asks as **Recog**: its launchd job starts `Recog.app`, a small
launcher that `./setup.sh` builds and signs, and the launcher runs the Python
interpreter as its child. macOS attributes Screen Recording and the
microphone to the program the job starts, so without the launcher the dialogs
would name `python3.12` and a Homebrew upgrade of the interpreter would void
the grant of the running service. `Recog.app` is rebuilt only when
`recog/native/macos/recog_launcher.swift` changes, and macOS then asks once
more.

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

`./recog-service.sh` manages four permanent services — live transcription, the
web UI, the copilot MCP channel and the copilot supervisor — through launchd on
macOS and `systemd --user` on Linux. The units are rendered from the templates
in `deploy/{macos,linux}/` with your repo path, interpreter and log
directory substituted in.

```bash
./recog-service.sh install    # render + register the units, WITHOUT starting them
./recog-service.sh start      # start (after a dependency readiness check)
./recog-service.sh status     # PIDs of all installed services
./recog-service.sh restart    # pick up new code / a re-rendered unit
./recog-service.sh logs       # tail live transcription logs (logs-web, logs-supervisor, …)
./recog-service.sh uninstall  # stop and remove
```

`install` deliberately never activates anything — that is what lets `setup.sh`
verify dependencies before starting a pipeline that cannot work. `start`,
`restart` and the targeted `restart-web` / `restart-live` / `restart-copilot` /
`restart-supervisor` all run the same readiness probes as setup and refuse to
activate an incomplete install rather than take a working one down.

On Linux, add `loginctl enable-linger $USER` so the user services survive
logout — `status` prints the reminder. After rebuilding the extension, reload
it in `chrome://extensions`.

After editing backend code (`recog/web/server.py`, `recog/*.py`) run
`./recog-service.sh restart`; for `recog/copilot_supervisor.py` alone,
`./recog-service.sh restart-supervisor` is enough.

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
agent-status chip in the web UI work regardless.

Before setup, install `tmux` and Claude Code and make both commands available
on `PATH`. Setup preserves their discovered directories in the service PATH,
including a custom npm prefix. Run `claude` interactively as the same user to
sign in before starting Copilot; setup checks CLI availability, not authentication.
After moving or installing a CLI in a new directory, rerun `./setup.sh`.

If Copilot is not needed, run `RECOG_SKIP_COPILOT_CHECK=1 ./setup.sh` to skip
only its CLI checks. Use the same prefix for later service commands, for example
`RECOG_SKIP_COPILOT_CHECK=1 ./recog-service.sh restart`. This is a per-command
check override, not a persistent Copilot setting; audio dependency checks remain active.

To enable auto-spawn: put `tmux` and the `claude` CLI on `PATH`, set
`RECOG_COPILOT_AUTOSPAWN=1` in `.env`, then
`./recog-service.sh restart-supervisor`.

On both operating systems `restart` and `restart-supervisor` keep a live
copilot session — on Linux the session runs in its own transient
`recog-copilot-session.service`, outside the supervisor's cgroup, and the
restarted supervisor re-adopts it. An explicit Stop in the web UI (or
`./recog-service.sh stop` / `uninstall`) ends it.

## Remote ASR

Instead of running GigaAM locally you can point recog at your own
OpenAI-Whisper-compatible ASR gateway. The optional server implementation lives
in `recog/asr_server/`; see the [ASR server guide](asr-server.md) for
installation and configuration:

```dotenv
RECOG_ASR_BACKEND=remote
RECOG_ASR_REMOTE_URL=https://<asr-gateway>/asr
```

There is no default URL: `remote` without a URL fails at startup instead of
silently running on-device with different RAM and latency. The local model
stays unloaded while the gateway is healthy and loads lazily on the first
failure; a circuit breaker routes around a dead gateway. Only worth it on a
real LAN (`ping` RTT < 5 ms).

## Your name and Copilot profile

On your first web UI visit, **Как к вам обращаться?** asks for your name and
optional forms of address (one per line). **Позже** keeps recording and manual
Copilot chat available. Open **Настройки → Мой профиль** later to edit or clear
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

Copy `.env.example` → `.env` (setup does it for you) — it carries an inline
comment per variable. The ones you are most likely to touch:

- `MIC_SPEAKER_NAME` — legacy microphone-name fallback before a profile is saved (default `Me`).
- `RECOG_ASR_DEVICE` — `auto` (cuda > mps > cpu) | `cuda` | `mps` | `cpu`. An
  explicitly requested device that is unavailable fails fast instead of quietly
  landing on CPU.
- `RECOG_TORCH_THREADS` — cap `torch.set_num_threads()` on a CPU-only box.
- `RECOG_PULSE_MONITOR_SOURCE` — pin one PulseAudio monitor (Linux).
- `RECOG_EXTRA_WS_ORIGINS` — comma-separated extra origins allowed to open the
  bridge / live-stream WebSockets. The shipped conference hosts are built in
  (`recog/ws_origin.py`, mirroring the extension's `manifest.json`); if your
  conferences live on another host, add it **both** here and to the extension's
  `manifest.json` (that is a separate rebuild).
- `RECOG_ASR_BACKEND`, `RECOG_ASR_REMOTE_URL` — see [Remote ASR](#remote-asr).
- `RECOG_COPILOT_AUTOSPAWN`, `RECOG_COPILOT_GRACE_S`, `RECOG_COPILOT_MCP_URL` —
  see [Copilot](#copilot).
- `CLAUDE_MODEL` and `CODEX_MODEL` — legacy
  defaults when no corresponding AI settings override has been saved.

Open **Settings → Промпты и модели** to configure every LLM scenario: AI
Analysis, meeting titles, saved summary templates and live Copilot.
Set common model defaults, then override the prompt, provider and model for
individual scenarios. Claude defaults to `claude-opus-5-5`, Codex to
`gpt-6-astra`; existing environment defaults remain effective until overridden
in Settings. Claude and Codex continue to use their CLI subscription login.

The **Инкремент** scenario configures both new analysis and saved increments.
New analysis compares the selected providers; a saved version uses Codex when
selected, otherwise the first selected provider. Existing saved versions keep
their history under the same prompt ID.

Claude and Codex have separate reasoning effort controls. Claude supports
`low`, `medium`, `high`, `xhigh`, `max`. Both providers offer Auto, which lets
their CLI choose the reasoning effort.
Copilot applies effort changes, like model changes, at the next session start.

The Copilot scenario also has an **Copilot включён** switch. Save it off to stop
the active agent and block manual starts, conference starts and crash retries.
Transcription continues. The supervisor checks the saved switch on startup and
every two seconds even while disconnected from the bridge; enabling it permits
manual starts and the existing conference autostart policy again.

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
and `Recog.app` exist — `./setup.sh` builds both when `swiftc` is available.

**It runs on CPU instead of CUDA.** `RECOG_ASR_DEVICE=cuda` makes the mismatch
loud (it raises instead of falling back). If torch itself has no CUDA, you
installed the CPU wheels — re-run setup with
`RECOG_TORCH_INDEX_URL=https://download.pytorch.org/whl/cu124 ./setup.sh`.

**"CUDA out of memory … retrying on cpu" in the logs.** With `RECOG_ASR_DEVICE`
left at `auto`, a busy or small card demotes the model to CPU instead of
crash-looping the service. That is a working mode, not a broken one: measured
RTF is 0.05–0.2 (a 3 s chunk takes 0.15–0.6 s), which keeps up with live
transcription. Free the card or set the device explicitly if you want the
failure to be loud instead.

**Setup says the install is not complete.** Nothing was activated on purpose.
Missing `ffmpeg`, `tmux`, `claude`, `parec`/`pactl`, PortAudio, or a macOS helper that will not
build (`swiftc`) all block activation, as does `RECOG_ASR_BACKEND=remote` with
an empty URL. Fix what it lists and re-run `./setup.sh`. The same probes gate
`./recog-service.sh start|restart|restart-*`; if you deliberately want a subset
(the web UI on a box with no audio stack, say), `RECOG_SKIP_READINESS=1
./recog-service.sh start` bypasses the gate.

**The extension never connects.** The WS handshake is Origin-gated. If your
conference host is not one of the built-in ones, add its exact HTTPS origin to
[`.local/conference-hosts.json`](conference-hosts.md), rebuild the extension
(`cd browser-extension && npm run build`), restart Recog and reload
`browser-extension/dist/` in `chrome://extensions`.

## Personal data outside the checkout

New installs store meetings in `~/.recog/meetings`, service logs in
`~/.recog/logs`, and profile/AI settings in the same data directory.
`RECOG_DATA_DIR` overrides that root; use an absolute path (or `~/…`), since
data paths must not depend on the caller's working directory. `.env` and
`recog/web/.env` stay in the checkout. Historical speaker database files are
preserved by data migration and backup.

Existing installs continue using their entire old data root and print a
migration hint. If both the checkout and `~/.recog` contain data, startup
refuses to choose silently. An explicit `RECOG_DATA_DIR` selects that root
without legacy detection. The shell scripts use the same resolver as Python;
service installation records the resolved root in the units, so re-run
`install` and `restart` after changing it.

To migrate an existing install, finish the current recording and stop any
manually launched recording or speaker-enrollment commands, then run:

```bash
./venv-gigaam/bin/python scripts/migrate-data.py  # inspect the proposed moves
./recog-service.sh stop
./venv-gigaam/bin/python scripts/migrate-data.py --apply
./recog-service.sh install
./recog-service.sh restart
```

Migration moves `meetings/`, `logs/`, and every `speakers*.json` using renames
on the same filesystem. It leaves no old directories or symlinks, does not
copy data, and refuses collisions or a destination inside the checkout.
An interrupted migration can be re-run to move the remaining entries; it
never merges two meeting archives or overwrites a database. `setup.sh` does
not migrate existing data. `scripts/backup-data.sh` backs up the resolved
data root plus the environment files in the checkout (`--with-logs` includes
logs).

## Updating an existing install

For the repository layout change, follow the [upgrade steps](repository-layout.md#updating-an-existing-checkout):
re-render service units, update any manual launch commands, and reload the
extension from `browser-extension/dist/` if Chrome still points to its old path.
The macOS dependency and data safeguards below still apply.

1. **Before pulling**, if you run `RECOG_ASR_BACKEND=remote`: write your actual
   gateway URL into `.env` as `RECOG_ASR_REMOTE_URL=…`. The personal default is
   gone, and setup refuses to activate services with `remote` + empty URL
   (nothing edits your `.env` for you).
2. `git pull && ./setup.sh` — the venv and the weight cache are reused; macOS
   pip steps run under `deploy/python/constraints/macos.txt`, which pins the verified
   torch/torchaudio/numpy/pyobjc set, so a Linux port does not silently bump
   your ML stack. Weight-cache preparation streams and hashes files without
   deserializing them, so no temporary model is created in RAM.
3. `./recog-service.sh install && ./recog-service.sh restart` if you skipped
   services during setup — the units must be re-rendered from the templates
   (the committed `.plist` files are gone).

Memory behaviour is meant to be unchanged, but that is verified by a separate
before/after comparison, not promised by setup.

## Testing

```bash
./scripts/check.sh                    # everything below, in one go
```

`scripts/check.sh` is what CI runs on macOS and Linux: `compileall`, `bash -n`
and shellcheck over the shell scripts, actionlint over the CI workflow, the
CLI smoke test, a dry-run render of the service
units, and the unit suite. The linters come from the quality tools in
[docs/quality.md](quality.md). It starts no service, captures no audio and loads no
model weights — those belong to manual acceptance on a real host.

The individual steps:

```bash
source venv-gigaam/bin/activate
python -m pytest tests/ -v -m "not e2e"  # unit tests
python -m compileall recog               # syntax check
python -m recog --help                   # CLI smoke test
```
