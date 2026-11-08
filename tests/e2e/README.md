# E2E Diarization Tests

Tests speaker attribution through the full pipeline: real KTalk conference → Chrome extension → the Rust live process (`recog live`, its bridge included).

## Prerequisites

1. **conference SSO account** with KTalk access (example.ktalk.ru)
2. **BlackHole** virtual audio device installed
3. **Chrome extension** built: `cd browser-extension && npm run build`
4. **The `recog` binary**: `bin/recog` of an installation with `[install] runtime = "rust"`,
   else `cd rust && cargo build --release -p recog-live` (`rust/target/release/recog`)
5. **Test tools** in any Python 3.12 (the suite does not import the recog package):
   `pip install pytest pytest-timeout playwright websocket-client numpy soundfile && playwright install chromium`
6. **Audio fixtures**: `python tests/e2e/prepare_fixtures.py`

The orchestrator starts `recog live --ktalk --system-audio device:auto` (BlackHole found by
name) with `RECOG_DATA_DIR` set to a fresh `recog-e2e-*` directory under the system temp
directory, waits for its bridge, and after SIGINT reads the transcript from that directory's
`meetings/`. The directory is kept for diagnosis, with the process output in
`live-process.log`. The bridge port is fixed at 8766 in the binary and in the extension, so
the installed live service is still stopped for the run (`conftest.py`).

`pytest tests/e2e` uses `tests/e2e/pytest.ini`, so the repository's `tests/conftest.py`,
which imports recog, is not loaded.

## Running

```bash
export SSO_LOGIN="your_login"
export SSO_PASSWORD="your_password"
python -m pytest tests/e2e/ -m e2e -v -s --timeout=120
```

For the per-speaker PCM pipeline tests (longer run, needs
`RECOG_PCM_TRANSPORT=1` which the orchestrator sets for you):

```bash
python -m pytest tests/e2e/test_per_speaker_overlap.py -m e2e -v -s --timeout=180
```

## Test Scenarios

| Test | What it verifies |
|------|-----------------|
| `test_two_speakers_alternating` | Basic: two guests speak in turns, both attributed |
| `test_ttl_regression_speaker_resets_after_gap` | TTL fix: after 10s gap, speaker resets to Unknown |
| `test_long_pause_between_speakers` | Long pause: second speaker not attributed as first |
| `test_three_speakers` | Three guests speak sequentially, all three attributed |
| `test_per_speaker_overlap_attribution` | Per-speaker PCM: 15s overlap → both speakers distinct, no word bleed |
| `test_per_speaker_degraded_mode_on_extension_reload` | Per-speaker PCM: host reload → watchdog fires → mixed-fallback marker + `fallback_mixed/*` segments |
