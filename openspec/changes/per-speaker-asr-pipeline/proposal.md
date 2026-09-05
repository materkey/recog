## Why

Live transcription currently captures **one mixed audio track** (BlackHole or ScreenCaptureKit) and guesses the speaker post-hoc from KTalk dominant-speaker events. When two people talk simultaneously, GigaAM sees garbled overlapped audio and the result is misattributed to whoever happened to be "dominant" at that instant. Short interjections also get attributed to the wrong person because chunk boundaries do not align with speech boundaries.

Investigation on 2026-04-22 confirmed that KTalk's Jitsi SFU already exposes **per-participant MediaStreamTracks** (verified with 2- and 3-participant rooms, zero cross-talk, 29× spectral-flatness separation). GigaAM benchmarked at RTF=0.032 on M-series CPU (30× realtime headroom, 60× when batched) — local processing of N parallel streams is fully viable on the user's MacBook. No remote worker needed.

## What Changes

- **Per-speaker audio capture** for KTalk meetings: Chrome extension extracts each remote participant's `MediaStream`, downsamples 48 → 16 kHz in an `AudioWorkletNode`, and streams Int16 PCM chunks over the existing WebSocket to `recog/ktalk_bridge.py`.
- **New WebSocket frame type**: binary `pcm_chunk {pid, seq, pcm_bytes}` alongside the current JSON control messages.
- **Per-PID ingest pipeline** in `recog/live.py`: ring buffer per participant + batch collector + one shared `GigaAM` instance with batched `forward()` inference. Attribution is now an input (from `pid`), not a guess.
- **Deterministic speaker tagging**: text emitted to transcript is tagged with the SFU-provided participant identity on the chunk it came from — no dominant-speaker timing heuristics.
- **Optional VAD-gate**: extension's existing `isSpeech()` (audio-analyzer.js:66) can gate worklet output so silent participants don't burn CPU. Off by default in this change; toggle via config.
- Local microphone capture **unchanged** — it continues through `SingleDeviceCapture` (browser does not expose local mic to the host tab as a remote track).
- Mixed-capture path (BlackHole + ScreenCaptureKit) **preserved** for non-KTalk use (YouTube, Zoom, offline). Selected automatically: if the KTalk extension is connected and the session is a KTalk meeting, new path wins; otherwise old path is used.

## Capabilities

### New Capabilities

- `per-speaker-transcription`: live ASR pipeline that receives per-participant PCM streams from a browser extension and produces a speaker-tagged transcript without post-hoc attribution. Covers capture (browser-side), transport (WebSocket protocol), ingest (batching + ASR scheduling), and output (tagged transcript). Source-of-truth for speaker identity is the SFU participant ID; ECAPA-TDNN remains the fallback for non-KTalk paths.

### Modified Capabilities

None. The repository currently has no `openspec/specs/` entries; this is a greenfield capability.

## Impact

**Code**:
- `browser-extension/src/` — new `audio-worklet.js` (downsampler processor), new PCM-sink module, wiring in `init.js` / `ws.js`, rebuild of `injected.js`.
- `recog/ktalk_bridge.py` — new binary frame handling, PCM fan-out to live pipeline.
- `recog/live.py` — per-PID ingest path, gated on `use_ktalk` and bridge-reports-new-protocol. Old mixed path stays as fallback.
- `recog/asr.py` — may grow a `transcribe_batch(wavs, lengths)` helper for batched forward (small, additive).
- `tests/e2e/` — new test that asserts correct attribution during overlapping speech.

**APIs**:
- `ktalk_bridge` WebSocket protocol grows a new binary frame type. Existing JSON control messages are untouched (backward-compatible with current extension versions during rollout).

**Dependencies**:
- No new Python deps (reuses existing `websockets`, `torch`, `gigaam`).
- No new JS deps (AudioWorklet is native).

**Operational**:
- Memory: one GigaAM instance ~1.6 GB RSS (already loaded; no change).
- CPU: benchmark shows <5% realtime at N=5 participants.
- User-visible latency: **unchanged** — chunk_duration stays at 3 s (UI-latency tuning is out of scope; see non-goals below).

**Non-goals (out of scope for this change, separate proposals if needed)**:
- Shorter `chunk_duration` / live-latency optimization.
- MPS / Metal GPU acceleration.
- Alternative ASR backends (Whisper, etc.).
- Any remote ASR worker or offload to Z270XX — benchmark proved unnecessary.
- Changes to meeting storage, frontend rendering, or AI summary features (anchor / catchup / increments) — they operate on aggregated transcript and are unaffected.
