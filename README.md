# recog

Russian speech recognition with speaker diarization and identification.

## Features

- **ASR**: GigaAM (v3_e2e_rnnt) for Russian speech recognition
- **Speaker Diarization**: pyannote.audio 4.x (speaker-diarization-community-1)
- **Speaker Identification**: SpeechBrain ECAPA-TDNN embeddings with known speaker database
- **Live Mode**: Real-time transcription from microphone/system audio
- **File Processing**: Transcribe audio/video files with speaker labels

## Requirements

- Python 3.12
- macOS (tested on Darwin)
- ~4GB disk space for models

## Installation

```bash
# Clone and setup
git clone <repo>
cd recog

# Create symlink to GigaAM venv (must exist at ../GigaAM/venv)
ln -s ../GigaAM/venv venv-gigaam

# Create pyannote venv
python3.12 -m venv venv-pyannote
source venv-pyannote/bin/activate
pip install pyannote.audio soundfile speechbrain "huggingface_hub>=0.28.1,<0.30"
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

### Process audio/video file

```bash
source venv-gigaam/bin/activate

# Basic transcription
python mp4_notes.py input.mp4

# With speaker identification
python mp4_notes.py input.mp4 --speaker-db speakers_ecapa.json

# Output to file
python mp4_notes.py input.mp4 --speaker-db speakers_ecapa.json -o output.md
```

### Live transcription

```bash
source venv-gigaam/bin/activate

# From microphone
python mp4_notes.py live --speaker-db speakers_ecapa.json

# From system audio (BlackHole) - use device ID from --list-devices
python mp4_notes.py live --speaker-db speakers_ecapa.json --system-device 0

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

### Build speaker database from file

```bash
source venv-gigaam/bin/activate
python build_speaker_db.py input.mp4 speakers_ecapa.json
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

## Architecture

See [ADR documents](adr/) for architecture decisions:

- ADR-0001: GigaAM as ASR engine
- ADR-0002: pyannote.audio for diarization
- ADR-0003: Parallel diarization pipeline
- ADR-0004: Speaker database with embeddings
- ADR-0005: Speaker post-clustering
- ADR-0006: Dual venv subprocess architecture
- ADR-0007: Unified diarization pipeline
- ADR-0008: SpeechBrain ECAPA-TDNN embeddings
- ADR-0009: Remove double clustering
- ADR-0010: Live transcription mode

## Files

- `mp4_notes.py` - Main CLI tool
- `diarize_worker.py` - pyannote diarization subprocess worker
- `add_speaker_live.py` - Add speakers via microphone
- `build_speaker_db.py` - Build speaker DB from audio files
- `speakers_ecapa.json` - Speaker embedding database

## License

MIT
