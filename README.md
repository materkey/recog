# recog

Russian speech recognition with live speaker identification, meeting storage
and a web UI. Runs on macOS and Linux.

## Features

- **ASR**: GigaAM (`v3_e2e_rnnt`) for Russian speech recognition, on CUDA, MPS,
  CPU, or offloaded to a remote gateway.
- **Speaker identification**: the bundled Chrome extension names speakers from
  the conference itself (KTalk, Jitsi, Element Call); SpeechBrain ECAPA-TDNN
  embeddings cover enrollment from the microphone.
- **Live mode**: real-time transcription of the microphone and the system audio
  (what you hear), stored per meeting and streamed to the web UI.
- **Copilot** (optional): a headless Claude Code session attached to the live
  meeting, spawned manually from the web UI or automatically per meeting.

## Requirements

| | macOS | Linux |
|---|---|---|
| OS | 13+, Apple Silicon (Intel is not supported: no macOS x86_64 torch wheels past 2.2.2) | any distro with systemd (Ubuntu/Debian, Arch/Manjaro tested) |
| Python | 3.12+ | 3.12+ |
| System audio | ScreenCaptureKit — built in, needs `swiftc` (`xcode-select --install`) to build the helper | PulseAudio **or** PipeWire with `pipewire-pulse`, plus `parec`/`pactl` (`pulseaudio-utils` / `libpulse`) |
| Microphone | built in (PortAudio ships with the wheel) | PortAudio (`libportaudio2` / `portaudio`) |
| Required | `ffmpeg` | `ffmpeg` |
| Optional | `node`+`npm` (extension build, AI summaries), `tmux`+`claude` (Copilot), `jq` (`debug` output) | same |
| Disk | ~4 GB for models | ~4 GB for models |
| GPU | Apple Silicon → MPS automatically | NVIDIA → CUDA automatically; CPU works too (measured RTF 0.05–0.2) |

`./setup.sh` prints a found/missing table for all of the above with the install
command for your package manager (apt / pacman / brew), so you do not have to
pre-install anything by hand — run it first and let it tell you what is
missing.

## Quick start

```bash
git clone <repo-url> recog && cd recog
./setup.sh
# → chrome://extensions → Developer mode → Load unpacked →
#   kontur-diarize/ktalk-speaker-extension
# → open http://127.0.0.1:9080
```

`./setup.sh` creates `venv-gigaam`, installs torch/torchaudio and GigaAM (from
a pinned git commit — no sibling checkout needed), copies `.env.example` →
`.env`, builds the Chrome extension, prepares the GigaAM weight cache, renders
the service units and starts them. It is idempotent: re-running it is how you
update after a `git pull`.

If something required is missing, setup renders the units but **does not
activate anything** and exits non-zero (previously running services are left
running). Install what it lists and run `./setup.sh` again.

Flags: `--no-services` (prepare only, do not touch launchd/systemd),
`--no-prefetch` (skip the weight-cache preparation), `--no-extension`.

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
python mp4_notes.py live --ktalk --system-audio auto

# System audio only
python mp4_notes.py live --ktalk --system-audio auto --mic-device none

# Microphone only, speakers from the embedding database
python mp4_notes.py live --speaker-db ~/.recog/speakers_ecapa.json

# What audio devices do I have?
python mp4_notes.py live --list-devices

# Enroll a speaker (records 5 seconds from the mic; 10 with a trailing "10")
python add_speaker_live.py "Speaker Name"

# Browse stored meetings
python mp4_notes.py meetings
```

`--system-audio` takes `auto` (per OS), `screencapture` (macOS,
ScreenCaptureKit), `pulse` (Linux, PulseAudio/PipeWire), `device:<id>` (an
input device, `device:auto` looks for BlackHole), or `none` (default).
`--mic-device` takes a device id, `auto` (default input), or `none`.

The startup banner prints which sources are live, naming the resolved mic
device and the backend in full, e.g.
`Mode: DUAL (Mic: 3 + System: PulseAudio/PipeWire (parec))`.

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
in `deploy/{launchd,systemd}/` with your repo path, interpreter and log
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
logout — `status` prints the reminder.

After editing backend code (`frontend/server.py`, `recog/*.py`) run
`./recog-service.sh restart`; for `recog/copilot_supervisor.py` alone,
`./recog-service.sh restart-supervisor` is enough.

## Copilot

All four units — including the copilot supervisor — are installed everywhere;
there is no separate flag. What is off by default is the *automatic* spawn per
meeting: `.env.example` ships `RECOG_COPILOT_AUTOSPAWN=0`, so a fresh install
never starts a Claude session unattended. The manual Start/Stop button and the
agent-status chip in the web UI work regardless.

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
OpenAI-Whisper-compatible ASR gateway (see `deploy/z270xx/` for the gateway
patch we run):

```dotenv
RECOG_ASR_BACKEND=remote
RECOG_ASR_REMOTE_URL=https://<asr-gateway>/asr
```

There is no default URL: `remote` without a URL fails at startup instead of
silently running on-device with different RAM and latency. The local model
stays unloaded while the gateway is healthy and loads lazily on the first
failure; a circuit breaker routes around a dead gateway. Only worth it on a
real LAN (`ping` RTT < 5 ms).

## Environment variables

Copy `.env.example` → `.env` (setup does it for you) — it carries an inline
comment per variable. The ones you are most likely to touch:

- `MIC_SPEAKER_NAME` — speaker label for microphone input (default `Me`).
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
- `CODEX_MODEL` / `AI_PROVIDER` (also in `frontend/.env`) — model used for
  transcript analysis, auto-increments, anchors and titles.

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

**No system audio (macOS).** Check Screen Recording permission for the terminal
(or for the launchd service's parent) and that `sck_audio_capture` exists and
is executable — `./setup.sh` rebuilds it when `swiftc` is available.

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
Missing `ffmpeg`, `parec`/`pactl`, PortAudio, or a macOS helper that will not
build (`swiftc`) all block activation, as does `RECOG_ASR_BACKEND=remote` with
an empty URL. Fix what it lists and re-run `./setup.sh`. The same probes gate
`./recog-service.sh start|restart|restart-*`; if you deliberately want a subset
(the web UI on a box with no audio stack, say), `RECOG_SKIP_READINESS=1
./recog-service.sh start` bypasses the gate.

**The extension never connects.** The WS handshake is Origin-gated. If your
conference host is not one of the built-in ones, add it to
`RECOG_EXTRA_WS_ORIGINS` *and* to the extension's `manifest.json`, then rebuild
the extension (`cd kontur-diarize/ktalk-speaker-extension && npm run build`) and
reload it in `chrome://extensions`.

## Personal data outside the checkout

New installs store meetings in `~/.recog/meetings`, service logs in
`~/.recog/logs`, and speaker databases in `~/.recog/speakers*.json`.
`RECOG_DATA_DIR` overrides that root; use an absolute path (or `~/…`), since
the CLI and services have different working directories. `.env` and
`frontend/.env` stay in the checkout. An explicit `--speaker-db PATH` still
uses exactly the path you pass.

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

## Updating an existing macOS install

1. **Before pulling**, if you run `RECOG_ASR_BACKEND=remote`: write your actual
   gateway URL into `.env` as `RECOG_ASR_REMOTE_URL=…`. The personal default is
   gone, and setup refuses to activate services with `remote` + empty URL
   (nothing edits your `.env` for you).
2. `git pull && ./setup.sh` — the venv and the weight cache are reused; macOS
   pip steps run under `constraints/macos.txt`, which pins the verified
   torch/torchaudio/numpy/pyobjc set, so a Linux port does not silently bump
   your ML stack. Weight-cache preparation streams and hashes files without
   deserializing them, so no temporary model is created in RAM.
3. `./recog-service.sh install && ./recog-service.sh restart` if you skipped
   services during setup — the units must be re-rendered from the templates
   (the committed `.plist` files are gone).
4. **Check the speaker database after the pull.** `speakers_ecapa.json` used to be tracked, so
   untracking it deletes your working copy (`.gitignore` has no power over an
   already-tracked file) and setup seeds an empty `{}` in its place — speaker
   identification then silently does nothing. Setup says so when it happens;
   the embeddings stay in git history, so restore them from the last commit
   that still carries the file, using the destination printed by setup (normally
   `git show <sha>:speakers_ecapa.json > ~/.recog/speakers_ecapa.json`). A copy you had
   modified since its last commit stops the pull instead of being deleted.

Memory behaviour is meant to be unchanged, but that is verified by a separate
before/after comparison, not promised by setup.

## Testing

```bash
./scripts/check.sh                    # everything below, in one go
```

`scripts/check.sh` is what CI runs on macOS and Linux: `compileall`, `bash -n`
over the shell scripts, the CLI smoke test, a dry-run render of the service
units, and the unit suite. It starts no service, captures no audio and loads no
model weights — those belong to manual acceptance on a real host.

The individual steps:

```bash
source venv-gigaam/bin/activate
python -m pytest tests/ -v            # unit tests
python -m compileall recog frontend   # syntax check
python mp4_notes.py --help            # CLI smoke test
```

## Architecture

See [ADR documents](adr/) for architecture decisions:

- ADR-0001: GigaAM as ASR engine
- ADR-0002: pyannote.audio for diarization (obsolete — replaced by KTalk extension)
- ADR-0003: Parallel diarization pipeline (obsolete)
- ADR-0004: Speaker database with embeddings
- ADR-0005: Speaker post-clustering
- ADR-0006: Dual venv subprocess architecture (obsolete — single venv now)
- ADR-0007: Unified diarization pipeline (obsolete)
- ADR-0008: SpeechBrain ECAPA-TDNN embeddings
- ADR-0009: Remove double clustering
- ADR-0010: Live transcription mode (superseded by ADR-0021 for audio capture)
- ADR-0011: Meeting storage
- ADR-0013: AI transcript analysis
- ADR-0018: Per-speaker ASR pipeline (PCM transport from Chrome extension)
- ADR-0019: Stream-refine — tier-2 re-transcription with 20 s context
- ADR-0020: Remote GigaAM offload with lazy local fallback
- ADR-0021: Cross-platform capture and services (parec, systemd --user, templates)

## Files

- `mp4_notes.py` — main CLI (commands: `live`, `meetings`)
- `setup.sh` / `recog-service.sh` — install and service management
- `ktalk_bridge.py` — WebSocket server for the conference Chrome extension
- `add_speaker_live.py` — enroll speakers via the microphone
- `frontend/` — FastAPI server + SPA on http://127.0.0.1:9080
- `~/.recog/speakers_ecapa.json` — local speaker embedding database

## License

MIT
