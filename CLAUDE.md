# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Recog** is a Russian speech recognition system with live speaker identification via KTalk. It combines:
- **GigaAM** (v3_e2e_rnnt) for Russian ASR
- **SpeechBrain ECAPA-TDNN** for speaker embeddings

## Commands

### Setup
```bash
# Full setup (creates venv and .env)
./setup_venvs.sh

# Or manual setup:
# GigaAM venv (requires ../GigaAM to exist)
python3.12 -m venv venv-gigaam
./venv-gigaam/bin/pip install -e ../GigaAM torch torchaudio soundfile sounddevice
```

### Running
```bash
# Always use venv-gigaam for main CLI
source venv-gigaam/bin/activate

# Live transcription from system audio (BlackHole)
python mp4_notes.py live --system-device 0 --speaker-db speakers_ecapa.json

# Live with KTalk Chrome extension (speaker names from conference)
python mp4_notes.py live --ktalk --screencapture

# List audio devices
python mp4_notes.py live --list-devices

# Add speaker to database (records 5 seconds from mic)
python add_speaker_live.py "Speaker Name"
```

### Frontend (web UI)
```bash
cd frontend
pip install -r requirements.txt
python server.py  # http://127.0.0.1:9080
```

### macOS Services
```bash
./recog-service.sh install   # Install launchd services
./recog-service.sh start     # Start both live + frontend
./recog-service.sh status    # Show service status
./recog-service.sh logs      # Tail live transcription logs
./recog-service.sh restart   # Restart after code changes
```

**Important**: After modifying `frontend/server.py` or other backend code, run `./recog-service.sh restart` to apply changes.

### Chrome Extension (KTalk speaker detection)
```bash
cd kontur-diarize/ktalk-speaker-extension
npm install        # First time only
npm run build      # Rebuild after changes to src/
```
After rebuild, reload in Chrome: `chrome://extensions` → Developer Mode → Reload.

The extension connects to `ws://localhost:8765` (ktalk_bridge.py) and sends speaker signals from KTalk conferences.

### Swift Binary (screen capture)
If modifying `sck_audio_capture.swift`, rebuild:
```bash
swiftc -o sck_audio_capture sck_audio_capture.swift \
    -framework ScreenCaptureKit -framework CoreMedia -framework AVFoundation
```

## Architecture

### Single-venv Architecture
The project uses a single `venv-gigaam` virtual environment. Speaker identification is handled via the KTalk Chrome extension (real-time) and SpeechBrain ECAPA-TDNN embeddings (enrollment). The former dual-venv pyannote subprocess architecture (ADR-0006) is obsolete.

### Remote ASR offload (opt-in)
`FallbackASR` implements all four GigaAM paths (`transcribe_batch` per-speaker, `transcribe_simple` offline, `transcribe_with_timestamps` refine, and the direct mic/mixed `transcribe_chunk`) over a network gateway (Z270XX, OpenAI-Whisper-compatible). Three are reached today through `build_asr()` (the single constructor, `recog/remote_asr.py`) via the live pipeline; the offline `transcribe_simple` path is implemented but not yet wired to a CLI command (`mp4_notes.py` has only `live`/`meetings`). When constructed, ASR comes exclusively from `build_asr()`: `RECOG_ASR_BACKEND="local"` (committed default — repo behavior unchanged) returns a plain `ASRModule`; `"remote"` returns a `FallbackASR` wrapping a `RemoteASRClient`. `FallbackASR` constructs the local `ASRModule` eagerly but loads its weights **lazily in a background thread** — triggered by the first remote failure. While neither backend can serve (circuit open + local still loading) it raises `ASRTemporarilyUnavailable` and exposes `available=False`; consumers (`_process_chunk`, `PerSpeakerIngest.tick`, `RefineWorker`) gate on it and keep their audio buffered instead of dropping chunks (see `docs/plans/2026-06-02-asr-fallback-buffering-design.md`). The circuit breaker is thread-safe and trips after `RECOG_ASR_CIRCUIT_FAILS` consecutive failures (open for `RECOG_ASR_CIRCUIT_OPEN_S`, then one half-open probe). Live chunk/batch requests use the shorter `RECOG_ASR_LIVE_TIMEOUT_S`; refine/offline keep `RECOG_ASR_REMOTE_TIMEOUT_S`. It delegates `model` / `device` / `_model_lock` / `_maybe_empty_mps_cache` to the local module so `RefineWorker` keeps working; refine skips the abortable-decode monkey-patch when `asr.using_remote`. Gateway patch + deploy steps live in-repo at `deploy/z270xx/`.

### ASR fallback buffering (always-on)
No speech is lost when ASR is temporarily unavailable (remote down + local loading): capture buffers (`SingleDeviceCapture`, `ScreenCaptureAudio`) are not consumed while `asr.available` is False (capped at `ASR_PENDING_BUFFER_CAP_S`), `get_chunk()` returns `(chunk, capture_start_ts)` so backlog segments get spoken-time timestamps, and a chunk caught mid-failure is `requeue()`d. Live dedup applies only within `DEDUP_MAX_GAP_S` of the previous segment (a phrase repeated after a pause is real speech). Swallowed ASR exceptions are logged via `recog/log_utils.py` (rate-limited).

### Per-speaker ASR path (ADR-0018)
For any active KTalk meeting the pipeline routes through per-participant PCM streams (no mixed-audio fallback as a configurable mode):

- Chrome extension attaches `AudioWorkletNode` to each remote `MediaStream` (`kontur-diarize/ktalk-speaker-extension/src/audio-worklet/downsampler.js`, `src/pcm_transport.js`), downsamples 48 kHz → 16 kHz Int16 PCM and sends chunks via WS (`type: "pcm_chunk"`) to `ktalk_bridge.py`.
- Bridge dispatches chunks to `PerSpeakerIngest` (`recog/per_speaker.py`) which per-pid reorders by `seq`, batches across speakers (200 ms window, max 8), and calls `ASRModule.transcribe_batch()` (`recog/asr.py`). Attribution comes from `speaker_name` already in the frame — no `get_dominant_speaker_for_period()` call.
- Watchdog: if PCM is silent for `PCM_SILENCE_WATCHDOG_S` (30 s) during an active meeting, `PerSpeakerIngest._degraded` flips true, `LiveTranscriber._process_chunk` falls back to the existing mixed-path with `source="fallback_mixed"` and a gap-marker written to the transcript. Recovery marker is inserted when PCM returns.
- Local mic (`MIC_SPEAKER_NAME`) stays on `sounddevice` — it is not routed through the worklet (D9).

See `docs/plans/2026-04-22-per-speaker-asr-pipeline.md` for the decisions table and task breakdown.

### Per-speaker refine contour
A tier-2 refine pass re-transcribes per-pid PCM windows with word-level timestamps and replaces live-segment text in `transcript.jsonl` with `refined: true`:

- State ownership (design.md D11):
  - `recog/refine.py` — `RefineWorker` daemon thread + queue + `make_abortable_decode` decoder monkey-patch + `RefineTask` / `RefineResult` dataclasses.
  - `recog/per_speaker.py` — per-pid PCM ring (`_pcm_rings`), monotonic audio counter (`_pid_audio_ms`), silence/continuous-speech counters (`_silence_ms`, `_continuous_speech_ms`), refine watermark. Trigger via `get_refine_candidate(pid)`.
  - `recog/live.py` — per-pid live-segment registry `_pcm_live_segments[pid] = [(segment_id, start_ms, end_ms), ...]`, `_pcm_segment_texts` cache for identical-text skip, `_apply_refine` alignment + `update_segments` call.
  - `meeting_storage.py` — `TranscriptSegment.refined: bool = False`; `update_segments(...)` accepts optional `text` per row, sets `refined=True` when present.
- Flow: PCM arrives via `PerSpeakerIngest.enqueue` → per-pid ring + counters update → tick loop calls `get_refine_candidate(pid)` → gap (`SPEAKER_TURN_GAP=3s` silence) or monolog cap (`PCM_REFINE_MONOLOG_CAP_S=15s`) assembles a 20 s trailing window → `RefineWorker.submit` → worker acquires `asr._model_lock`, runs `transcribe_with_timestamps` → `_apply_refine` rebases words via `task.offset_base_ms`, intersects with live segments under `_state_lock` + meeting-generation check → `update_segments` rewrites JSONL rows.
- Shared model lock: `ASRModule._model_lock` is a `threading.RLock` (not `Lock`). The refine worker acquires it externally to serialise with the live batched-forward path; `transcribe_with_timestamps` re-acquires internally — recursion must not deadlock.
- Cooperative abort: default `abort_mode="cooperative"`. `LiveTranscriber._refine_transcribe_fn` installs `make_abortable_decode` on `model.decoding.decode` for the duration of one refine call; the hook polls `should_abort()` at the top of the RNN-T time-step loop and raises `RefineAborted`. Worker catches, releases the lock, re-enqueues the task. `LiveTranscriber._on_pcm_batch` calls `refine_worker.set_live_pending()` / `clear_live_pending()` around `asr.transcribe_batch` so live gets priority. Per the spike (`experiments/2026-04-24-refine-abort-spike.md`), cooperative abort saves ~770 ms per preempted refine on MPS (~80 % of inference cost).

On refine arrival the webapp renders tier-1 (opacity 0.6, dashed border) → tier-2 (solid) segments via `data-refined` on `.transcript-segment`; the KTalk extension overlay does a plain text swap with a brief `.refine-fade` pulse.

See `docs/plans/2026-04-24-per-speaker-refine-backend.md`, `openspec/changes/per-speaker-stream-refine/`, and ADR-0019 for the decisions table and task breakdown.

### Module Organization
- `recog/` - Core Python package
  - `ai_provider.py` - AIProvider ABC, CodexCliProvider, resolve_cli() — shared AI provider interface used by increment.py and title_generator.py
  - `asr.py` - GigaAM ASR wrapper
  - `speaker_db.py` - ECAPA-TDNN embeddings matching
  - `live.py` - Real-time transcription (LiveTranscriber with decomposed methods: _setup_meeting_storage, _setup_ktalk_bridge, _start_audio_captures, _process_chunk, _handle_mic_chunk, _handle_ktalk_chunk, _handle_embedding_chunk, _process_pending, _teardown_session; module-level helpers: normalize_word, deduplicate)
  - `increment.py` - Auto-generate versioned AI summaries on ktalk_leave; multi-prompt support via generate_all(), stores per-prompt results in meetings/<id>/increments/<prompt-id>/vN.md
  - `prompt_registry.py` - Load and filter prompt definitions from prompts/*.md files (YAML frontmatter + template body)
  - `node_env.py` - Node/nvm PATH utilities for subprocess calls under launchd (shared by frontend/, ktalk_bridge.py, and title_generator.py)
- `mp4_notes.py` - Main CLI entry point (commands: live, meetings)
- `ktalk_bridge.py` - WebSocket server receiving speaker signals from Chrome extension
- `frontend/` - FastAPI server + SPA (WebSocket for live updates)
  - `bridge_subscriber.py` - WS client that connects to `ktalk_bridge` (default `ws://localhost:8766`, overridable via `RECOG_BRIDGE_WS_URL`) and forwards `transcript_refine` frames to webapp live-stream clients on port 9080. See D10 in `openspec/changes/per-speaker-stream-refine/design.md`.
  - `live_watcher.py` - JSONL tail watcher with inode + size-shrink rewrite detection and a seen-id watermark. Survives atomic `os.replace` rewrites from `MeetingStorage.update_segments` without stalling or re-broadcasting previously-seen rows.
- `kontur-diarize/ktalk-speaker-extension/` - Chrome extension for KTalk
  - `src/` - Source modules (edit these)
  - `injected.js` - Built bundle (generated by webpack)
  - `content.js` - Injects bundle into KTalk page
  - `manifest.json` - Extension manifest (Manifest V3)

### Coding Style
- Python 3.12, 4-space indentation
- CLI wiring in `mp4_notes.py`, reusable logic in `recog/`
- `snake_case` for functions/vars, `PascalCase` for classes
- `*_worker.py` suffix for subprocess helpers
- Prefer `pathlib.Path` and type hints for new code

## Environment Variables

Copy `.env.example` → `.env` and `frontend/.env.example` → `frontend/.env`:

- `HF_TOKEN` - HuggingFace token (optional, was required for pyannote diarization which is now removed)
- `AI_PROVIDER` - `claude` | `codex` | `openrouter` (for transcript analysis)
- `CLAUDE_MODEL` - `haiku` | `sonnet` | `opus`
- `OPENROUTER_API_KEY` - Required if using OpenRouter
- `MIC_SPEAKER_NAME` - Speaker label for microphone input in live mode (default: `Me`)
- `RECOG_BRIDGE_WS_URL` - Overrides the bridge WebSocket URL the frontend subscriber connects to (default `ws://localhost:8766`). Used by `frontend/bridge_subscriber.py` to forward `transcript_refine` events from `ktalk_bridge.py` to webapp clients (D10 — JSONL tail cannot see refine rewrites).
- `RECOG_ASR_BACKEND` - `local` (default — repo behavior unchanged) | `remote` (offload GigaAM to the Z270XX gateway via `recog/remote_asr.py`'s `build_asr()`, with lazy local fallback + circuit breaker).
- `RECOG_ASR_REMOTE_URL` - Gateway base URL (default `https://materkey-z270x-1.tail4799d.ts.net/asr`). Only read when `RECOG_ASR_BACKEND=remote`.
- `RECOG_ASR_REMOTE_MODEL` - Model alias sent to the gateway (default `gigaam`).
- `RECOG_ASR_REMOTE_TIMEOUT_S` - Per-request HTTP timeout (default `15`).
- `RECOG_ASR_CIRCUIT_FAILS` - Consecutive remote failures before the circuit opens (default `3`).
- `RECOG_ASR_CIRCUIT_OPEN_S` - Seconds the circuit stays open before a half-open probe (default `60`).
- `RECOG_ASR_LIVE_TIMEOUT_S` - Remote timeout for live chunk/batch requests (default `5`). Refine/offline keep `RECOG_ASR_REMOTE_TIMEOUT_S`.
- `DEDUP_MAX_GAP_S` - Live dedup window (default `2.0`): last-words dedup applies only to chunks captured within this many seconds of the previous segment.
- `ASR_PENDING_BUFFER_CAP_S` - Max audio buffered in memory while ASR is unavailable (default `600`).

## Data Files

- `speakers_ecapa.json` - Speaker embedding database
- `meetings/` - Stored meeting transcripts (JSONL format, gitignored)
- `prompts/` - Prompt definitions for AI analysis (.md files with YAML frontmatter); prompt ID = filename stem
- `logs/` - Service logs (gitignored)

## Testing

```bash
python -m pytest tests/ -v           # Unit tests (ktalk_bridge, live speaker attribution, retroactive correction)
python -m compileall recog frontend  # Syntax check
python mp4_notes.py --help           # CLI smoke test
```

## ADR Documents

Architecture Decision Records in `adr/`:
- **ADR-0006**: Dual venv subprocess (obsolete — pyannote diarization removed, single venv now)
- **ADR-0010**: Live transcription mode
- **ADR-0011**: Meeting storage
- **ADR-0013**: AI transcript analysis
- **ADR-0018**: Per-speaker ASR pipeline (PCM transport from Chrome extension; always-on for KTalk meetings)
- **ADR-0019**: Stream-refine — tier-2 re-transcription with 20 s context (always-on)
