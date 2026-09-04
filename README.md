# recog

Russian speech recognition with speaker diarization and identification.

## Features

- **ASR**: GigaAM (v3_e2e_rnnt) for Russian speech recognition
- **Speaker Identification**: KTalk Chrome extension (real-time) + SpeechBrain ECAPA-TDNN embeddings (enrollment)
- **Live Mode**: Real-time transcription from microphone/system audio

## Requirements

- Python 3.12
- macOS (tested on Darwin)
- ~4GB disk space for models
- `tmux` (`brew install tmux`) and the Claude Code `claude` CLI on `PATH` —
  required by the auto-spawn Copilot supervisor, which is installed by
  `./recog-service.sh install` and enabled by default. Without them the
  supervisor logs a spawn error and no copilot starts; set
  `RECOG_COPILOT_AUTOSPAWN=0` in `.env` to disable it entirely.

## Installation

```bash
# Clone and setup
git clone <repo>
cd recog

# Full setup (venv, dependencies, config files, services)
./setup.sh

# Or manual setup:
ln -s ../GigaAM/venv venv-gigaam
```

## System Audio Setup (macOS)

To capture system audio (e.g., from YouTube, Zoom, etc.), you need BlackHole and a Multi-Output Device.

### 1. Install BlackHole

```bash
brew install blackhole-2ch
```

### 2. Create Multi-Output Device

Open **Audio MIDI Setup** (Spotlight → "Audio MIDI Setup"):
1. Click **"+"** → **Create Multi-Output Device**
2. Check both:
   - ☑️ MacBook Air Speakers (or your output device)
   - ☑️ BlackHole 2ch

### 3. Switch audio output

**Option A: Via terminal (recommended)**
```bash
brew install switchaudio-osx
SwitchAudioSource -s "Multi-Output Device"
```

**Option B: Via System Settings**
1. System Settings → Sound → Output
2. Select **Multi-Output Device**

### 4. Verify setup

```bash
python mp4_notes.py live --list-devices
# Should show: BlackHole 2ch (device 0 or similar)
```

Now system audio will be routed to both speakers and BlackHole for capture.

## Usage

### Live transcription

```bash
source venv-gigaam/bin/activate

# From microphone
python mp4_notes.py live --speaker-db speakers_ecapa.json

# From system audio (BlackHole) - use device ID from --list-devices
python mp4_notes.py live --speaker-db speakers_ecapa.json --system-device 0

# DUAL MODE: both mic and system audio (processed separately)
# Mic uses MIC_SPEAKER_NAME from .env (default: "Me"), system audio uses speaker diarization
python mp4_notes.py live --speaker-db speakers_ecapa.json --mic-device 1 --system-device 0

# List audio devices
python mp4_notes.py live --list-devices
```

### Add speaker to database

```bash
source venv-gigaam/bin/activate

# Record 5 seconds from microphone
python add_speaker_live.py "Speaker Name"

# Record 10 seconds
python add_speaker_live.py "Speaker Name" 10
```

## Output Format

```
[21:15:00] Владимир Алипов : Первая фраза спикера
                             Продолжение той же реплики
                             Ещё продолжение
[21:15:30] Вячеслав        : Другой спикер начинает говорить
                             Его продолжение
```

- Timestamp and speaker name shown only for new speaker turns
- Continuation lines indented without timestamp
- Speaker turn detected by: different speaker OR pause > 3 seconds

## Environment Variables

Copy `.env.example` → `.env` and edit:

- `MIC_SPEAKER_NAME` — speaker label for microphone input (default `Me`).
- `CODEX_MODEL` — Codex model for transcript analysis, auto-increments,
  anchors, and titles. Supported GPT-5.6 models: `gpt-5.6-sol` (default),
  `gpt-5.6-terra`, and `gpt-5.6-luna`; requires Codex CLI 0.144.0 or newer.
- `RECOG_BRIDGE_WS_URL` — override the bridge WebSocket URL the frontend
  subscribes to (default `ws://localhost:8766`).
- `RECOG_COPILOT_AUTOSPAWN` — master switch for the auto-spawn Copilot
  supervisor (`1` default | `0` disables per-meeting spawn).
- `RECOG_COPILOT_GRACE_S` — grace period (seconds) before the supervisor
  kills the copilot session after a meeting ends (default `90`).
- `RECOG_COPILOT_MCP_URL` — copilot MCP daemon URL for the spawned Claude
  session (default `http://127.0.0.1:8767/mcp`).

The per-speaker ASR path (per-participant PCM streams from the KTalk
Chrome extension) and the tier-2 refine pass are always-on for KTalk
meetings — no env flags. See `CLAUDE.md` for architectural notes.

When installed as a macOS service (`./recog-service.sh install`), a
supervisor daemon auto-spawns a headless Claude Code copilot session per
KTalk meeting and tears it down after the meeting — the manual
`./recog-service.sh claude` step is no longer required (it stays available
for debugging). See `CLAUDE.md` → "Auto-spawn Copilot supervisor".

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
- ADR-0010: Live transcription mode
- ADR-0011: Meeting storage
- ADR-0013: AI transcript analysis
- ADR-0018: Per-speaker ASR pipeline (PCM transport from Chrome extension)
- ADR-0019: Stream-refine — tier-2 re-transcription with 20 s context

## Files

- `mp4_notes.py` - Main CLI tool (commands: live, meetings)
- `ktalk_bridge.py` - WebSocket server for KTalk Chrome extension
- `add_speaker_live.py` - Add speakers via microphone
- `speakers_ecapa.json` - Speaker embedding database

## License

MIT
