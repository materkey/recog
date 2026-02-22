# E2E Diarization Test with Real KTalk

## Problem

When two microphones are active in a KTalk meeting, all speech gets attributed to one speaker (cascading mis-attribution via `state.last_speaker` without TTL). We need an automated test that reproduces this scenario using real KTalk infrastructure.

## Architecture

```
┌─────────────────────────────────────┐
│ Playwright Browser 1 (Host)         │
│ - Kontur login                      │
│ - Creates public KTalk room         │
│ - Chrome extension loaded           │
│   → sends speaker_update to bridge  │
│ - fake audio: silence.wav           │
└──────────┬──────────────────────────┘
           │ room URL
┌──────────┴──────────────────────────┐
│ Playwright Browser 2 (Guest A)      │
│ --use-file-for-fake-audio=a.wav     │
│ - Joins as guest "Алексей"          │
│ - Mute/unmute controlled by test    │
└─────────────────────────────────────┘
┌──────────┴──────────────────────────┐
│ Playwright Browser 3 (Guest B)      │
│ --use-file-for-fake-audio=b.wav     │
│ - Joins as guest "Мария"            │
│ - Mute/unmute controlled by test    │
└─────────────────────────────────────┘

┌─────────────────────────────────────┐
│ LiveTranscriber (Python, parallel)  │
│ - System audio via BlackHole        │
│ - KTalk bridge ws://localhost:8766  │
│ - Records transcript                │
└─────────────────────────────────────┘
```

## Key Mechanism: Fake Audio in Chromium

Each Playwright browser instance uses Chromium flags:
- `--use-fake-device-for-media-stream` — fake devices instead of real mic
- `--use-file-for-fake-audio-capture=/path/to/file.wav` — specific WAV as "microphone"
- `--use-fake-ui-for-media-stream` — auto-accept media permissions

KTalk (Jitsi internally) detects dominant speaker based on audio levels from WebRTC. The Chrome extension reads `_activeSpeaker$` observable and sends `speaker_update` events via WebSocket to `ktalk_bridge.py`.

## How KTalk Speaker Detection Works

1. Each participant's browser sends audio via WebRTC to Jitsi server
2. Jitsi server determines dominant speaker by comparing audio levels
3. Jitsi pushes `conference.dominantSpeaker` event to all clients
4. KTalk wraps this in `_activeSpeaker$` RxJS observable on `window.$conf._participantsMap`
5. Chrome extension (`speaker.js`) reads this and calls `sendWsSpeakerUpdate({name, id, isLocal})`
6. `ktalk_bridge.py` receives via WebSocket, tracks in `_speaker_history`
7. `LiveTranscriber` queries `get_dominant_speaker_for_period()` for each audio chunk

## Test Scenarios

### 1. Basic: Two speakers alternating
- Guest A unmuted (0-15s), Guest B muted
- Guest B unmuted (15-30s), Guest A muted
- **Assert**: both speakers present in transcript with correct names

### 2. TTL regression: KTalk bridge gap
- Guest A speaks (0-10s)
- Both muted for >10s (10-25s) — simulates KTalk data gap
- Guest B speaks (25-35s)
- **Assert**: after TTL (10s), speaker resets to "Unknown", Guest B correctly attributed

### 3. Long pause between speakers
- Guest A speaks (0-10s)
- 15s silence
- Guest B speaks (25-35s)
- **Assert**: Guest B is NOT attributed as Guest A

### 4. Three speakers
- Guest A (0-10s), Guest B (10-20s), Guest C (20-30s)
- **Assert**: all three present in transcript

## Audio Fixtures

Source: `example_5min.wav` (5 min, multi-speaker, 16kHz mono)

Preparation script (`tests/e2e/prepare_fixtures.py`):
1. Extract 3 non-overlapping 15-second segments from example_5min.wav
2. Generate silence.wav (30s of zeros) for host
3. Save to `tests/e2e/fixtures/`

## Components

### `tests/e2e/conftest.py`
- Pytest fixtures for Playwright browsers
- KTalk room URL fixture
- LiveTranscriber fixture (runs in thread)

### `tests/e2e/ktalk_orchestrator.py`
- `KTalkOrchestrator` class
- Creates KTalk room via host browser
- Launches guest browsers with fake audio
- Controls mute/unmute timing
- Collects transcript results

### `tests/e2e/test_diarization_e2e.py`
- `@pytest.mark.e2e` marker (skip in CI)
- Test functions for each scenario
- Assertions on transcript speaker attribution

## Prerequisites

- Kontur account credentials (env vars: `KONTUR_LOGIN`, `KONTUR_PASSWORD`)
- BlackHole virtual audio device installed
- Chrome extension built (`npm run build` in `kontur-diarize/ktalk-speaker-extension/`)
- Playwright installed (`pip install playwright && playwright install chromium`)
- `example_5min.wav` present in project root

## Running

```bash
# Install dependencies
pip install playwright pytest-playwright
playwright install chromium

# Set credentials
export KONTUR_LOGIN="..."
export KONTUR_PASSWORD="..."

# Run e2e tests
pytest tests/e2e/ -m e2e -v --timeout=120
```
