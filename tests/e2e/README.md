# E2E Diarization Tests

Tests speaker attribution through the full pipeline: real KTalk conference → Chrome extension → ktalk_bridge → LiveTranscriber.

## Prerequisites

1. **Kontur account** with KTalk access
2. **BlackHole** virtual audio device installed
3. **Chrome extension** built: `cd kontur-diarize/ktalk-speaker-extension && npm run build`
4. **Playwright**: `./venv-gigaam/bin/pip install playwright pytest-playwright && ./venv-gigaam/bin/playwright install chromium`
5. **Audio fixtures**: `./venv-gigaam/bin/python tests/e2e/prepare_fixtures.py`

## Running

```bash
export KONTUR_LOGIN="your_login"
export KONTUR_PASSWORD="your_password"
./venv-gigaam/bin/python -m pytest tests/e2e/ -m e2e -v -s --timeout=120
```

## Test Scenarios

| Test | What it verifies |
|------|-----------------|
| `test_two_speakers_alternating` | Basic: two guests speak in turns, both attributed |
| `test_ttl_regression_speaker_resets_after_gap` | TTL fix: after 10s gap, speaker resets to Unknown |
| `test_long_pause_between_speakers` | Long pause: second speaker not attributed as first |
| `test_three_speakers` | Three guests speak sequentially, all three attributed |
