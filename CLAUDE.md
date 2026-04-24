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

### Per-speaker ASR path (ADR-0018)
When `RECOG_PCM_TRANSPORT=1` and a KTalk meeting is active, the pipeline switches from mixed-audio ASR (BlackHole/ScreenCaptureKit) to per-participant PCM streams:

- Chrome extension attaches `AudioWorkletNode` to each remote `MediaStream` (`kontur-diarize/ktalk-speaker-extension/src/audio-worklet/downsampler.js`, `src/pcm_transport.js`), downsamples 48 kHz → 16 kHz Int16 PCM and sends chunks via WS (`type: "pcm_chunk"`) to `ktalk_bridge.py`.
- Bridge dispatches chunks to `PerSpeakerIngest` (`recog/per_speaker.py`) which per-pid reorders by `seq`, batches across speakers (200 ms window, max 8), and calls `ASRModule.transcribe_batch()` (`recog/asr.py`). Attribution comes from `speaker_name` already in the frame — no `get_dominant_speaker_for_period()` call.
- Gate is server-authoritative: `recog/config.py:pcm_transport_enabled()` reads the env once, bridge sends `{"type": "pcm_transport_enabled"}` to extension on WS-handshake; extension emits PCM only when `enabled: true`.
- Watchdog: if PCM is silent for `PCM_SILENCE_WATCHDOG_S` (30 s) during an active meeting, `PerSpeakerIngest._degraded` flips true, `LiveTranscriber._process_chunk` falls back to the existing mixed-path with `source="fallback_mixed"` and a gap-marker written to the transcript. Recovery marker is inserted when PCM returns.
- Local mic (`MIC_SPEAKER_NAME`) stays on `sounddevice` — it is not routed through the worklet (D9).

See `docs/plans/2026-04-22-per-speaker-asr-pipeline.md` for the decisions table and task breakdown.

### Per-speaker refine contour (RECOG_PCM_REFINE)
When `RECOG_PCM_REFINE=1` (also requires `RECOG_PCM_TRANSPORT=1` — see `recog/config.py:pcm_refine_enabled()`), a tier-2 refine pass re-transcribes per-pid PCM windows with word-level timestamps and replaces live-segment text in `transcript.jsonl` with `refined: true`:

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
- `RECOG_PCM_TRANSPORT` - `1` enables per-speaker PCM transport from the KTalk extension (ADR-0018); default `0`. Requires extension rebuild + `./recog-service.sh restart` to apply.
- `RECOG_PCM_REFINE` - `1` enables the tier-2 stream-refine contour on top of PCM transport (ADR-0019); default `0`. Gated on `RECOG_PCM_TRANSPORT=1` — `pcm_refine_enabled()` returns False unless both flags are on. Adds a `refined: bool` field to `transcript.jsonl` rows (default `false`).
- `RECOG_PCM_VAD` - `1` enables the Silero VAD v6 prefilter in `LiveTranscriber._on_pcm_batch`; default `0`. Gated on `RECOG_PCM_TRANSPORT=1` via `pcm_vad_enabled()`. Drops near-silence PCM batch items before `ASRModule.transcribe_batch` so GigaAM RNN-T does not emit short filler hallucinations ("Да/Угу/Вот/Ок") on quiet windows. Symmetric with the mic-path RMS VAD in `_process_chunk`. Fails open on load/inference error — a chunk is kept rather than dropped when VAD itself fails. Suppressions are logged to stderr as `[PerSpeaker] VAD dropped N/M silent chunks`. Tunable via `RECOG_PCM_VAD_THRESHOLD` (0..1, default `0.3`).
- `RECOG_BRIDGE_WS_URL` - Overrides the bridge WebSocket URL the frontend subscriber connects to (default `ws://localhost:8766`). Used by `frontend/bridge_subscriber.py` to forward `transcript_refine` events from `ktalk_bridge.py` to webapp clients (D10 — JSONL tail cannot see refine rewrites).

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
- **ADR-0018**: Per-speaker ASR pipeline (PCM transport from Chrome extension, gated by `RECOG_PCM_TRANSPORT`)
- **ADR-0019**: Stream-refine — tier-2 re-transcription with 20 s context, gated by `RECOG_PCM_REFINE`
