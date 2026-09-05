## Context

**Current pipeline** (mixed capture + post-hoc attribution):

```
Browser                     macOS audio              Python (live.py)
───────                     ───────────              ────────────────
Jitsi SFU → <audio>  ─mix─▶ BlackHole/ScreenCap  ─▶ SingleDeviceCapture
KTalk extension ─WS JSON──────────────────────────▶ KTalkBridge
                            (dominant-speaker events only)          │
                                                                    ▼
                                                           ASR chunk (3s, mixed)
                                                                    │
                                                                    ▼
                                                     guess_speaker(chunk_time_window)
                                                                    │
                                                                    ▼
                                                            transcript.jsonl
```

Dominant-speaker attribution is a guess: the ASR sees one stream containing overlapping speakers, and we tag the output with whoever KTalk said was "dominant" during that 3-second window. When overlap occurs, ASR output is garbled *and* assigned wrong.

**What investigation revealed** (2026-04-22, full details in `tests/e2e/research/README.md`):

- Jitsi exposes `_participantsMap.get(pid)._audioTrack$.value.jitsiTrack.stream` as a live `MediaStream` per participant. Already touched by the extension's `audio-analyzer.js:32-47` (used only for VAD flatness today).
- These streams are **demultiplexed by SFU** — each carries only its speaker's audio. Verified via RMS + spectral flatness tests; Alice=speech and Bob=pink-noise simultaneously showed 29× flatness separation. KTalk keeps SFU even for 2-participant rooms (no P2P fallback).
- Track metadata: `{channelCount: 1, sampleRate: 48000, sampleSize: 16, latency: 0.01, readyState: 'live'}`.
- GigaAM benchmark (`scripts/benchmarks/bench_gigaam_rtf.py`): **RTF=0.032** single-stream on CPU, 30× realtime headroom, **60× when batched** at B=8. No remote worker needed.

**Constraints**:
- Must not regress current mixed-capture path (non-KTalk users: YouTube, Zoom, offline files).
- Must not add new network egress (PCM must stay on localhost WebSocket).
- Must not increase memory footprint significantly (GigaAM already ~1.6 GB).
- `chunk_duration = 3s` is an invariant for this change (latency tuning is a separate future change).

## Goals / Non-Goals

**Goals:**
- Eliminate speaker misattribution during overlapping speech.
- Make attribution deterministic — tag comes from SFU participant ID, not timing heuristics.
- Keep MacBook-local execution; leverage bench-confirmed headroom.
- Preserve existing mixed-capture fallback for non-KTalk sessions.
- Keep implementation surface small: one new worklet, one new WS frame type, one new Python ingest path. No new dependencies.

**Non-Goals:**
- Shorter chunk / UI-latency optimization.
- MPS (Metal) acceleration.
- Alternate ASR backends.
- Remote / Z270XX worker.
- Changing meeting storage format, frontend rendering, or AI summary features.
- Encoding (Opus / Ogg) — PCM is simpler and bandwidth is trivial on localhost.

## Decisions

### D1. `AudioWorkletNode` for downsampling, not `ScriptProcessorNode`
**Choice**: `AudioWorkletNode` running a custom `AudioWorkletProcessor` that consumes 48 kHz Float32 and emits 16 kHz Int16 in fixed-size chunks.

**Why**: `ScriptProcessorNode` is deprecated, runs on the main thread, and exhibits glitches under load. `AudioWorkletNode` runs in a dedicated Web Audio rendering thread, independent of React/KTalk main-thread work. Downsample is a 3:1 decimation; apply a cheap low-pass FIR (or 3-point averaging as MVP) before picking every third sample to avoid aliasing.

**Alternative considered**: Have Python receive 48 kHz PCM and downsample server-side. Rejected: wastes 3× WebSocket bandwidth for no benefit.

### D2. Int16 PCM over WebSocket binary frames, JSON envelope for metadata
**Choice**: Each PCM chunk is sent as a WebSocket binary message with a short JSON prefix (pid, seq, sample_count, ts_ms) followed by raw Int16 little-endian samples. Alternative: one binary-only frame with a fixed header struct.

**Why binary + JSON prefix**: Reuses the existing JSON-parse path in `recog/ktalk_bridge.py` for the header; only the PCM payload is binary. Parseable without a schema library.

**Concrete frame**: a single WS message with two parts: first 4 bytes = header-length u32 LE, then UTF-8 JSON header `{type:"pcm_chunk", pid, seq, samples, ts_ms}`, then `2 * samples` bytes of Int16LE PCM.

**Alternative considered**: Base64-encode PCM inside JSON. Rejected: 33% overhead + parse cost. At 16 kHz×16 bit = 32 KB/s per participant × 5 participants = 160 KB/s — trivial on loopback, but JSON parse for every chunk would dominate CPU unnecessarily.

### D3. One shared `GigaAM` instance, batched forward
**Choice**: The live-pipeline Python process loads one `GigaAM` model (existing `ASRModule`). A new batch collector groups pending chunks from multiple pids and calls `model.forward(batch_wav, batch_len)` with `B ≥ 1`, then `model.decoding.decode(...)` to get per-sample text.

**Why**: Bench showed batch=8 sustains 60× realtime vs 30× at B=1 — per-sample latency drops from 325 ms to 188 ms. Memory: 1 instance = 1.6 GB; N instances = N × 1.6 GB (quickly infeasible on a laptop).

**Alternative considered**: N model instances, one per pid. Rejected on memory and because the benchmark showed batched inference always beats thread-parallel.

### D4. Batch collection window: 200 ms
**Choice**: Collector waits up to 200 ms (or until batch of 8 chunks accumulates) before firing a forward pass.

**Why**: Balances added latency vs batch efficiency. With 3-second chunks per participant, a 200 ms collector window captures near-simultaneous chunks from 2–5 pids without adding user-visible lag. Configurable, but default chosen from bench.

### D5. Feature selection: KTalk extension presence, not config flag
**Choice**: If the Chrome extension connects to the bridge AND reports `protocol_version >= 2` (new extension version), the pipeline switches to per-speaker mode automatically. Otherwise old mixed path is used.

**Why**: Zero-config for users. The extension is already the unique "trigger" of KTalk-mode today (via `use_ktalk` flag). Adding a protocol-version handshake lets old extensions keep working during rollout.

**Alternative considered**: CLI flag `--per-speaker`. Rejected: users would have to remember; also breaks once extension auto-updates.

### D6. VAD-gate toggle, off by default
**Choice**: Extension's `isSpeech()` (audio-analyzer.js:66) can be wired to the worklet's output gate. If enabled, participants with flatness below the speech threshold skip PCM transmission. Default off.

**Why off**: Simplifies MVP, avoids VAD-triggered missed words at utterance onset. Bench showed we can handle all participants always-on with 10× headroom. Turn on later if someone profiles battery or sees thermal throttle.

### D7. Local microphone: unchanged
**Choice**: The user's mic continues through `SingleDeviceCapture` and the existing mic-handling pipeline in `recog/live.py`. Not routed through the new per-pid path.

**Why**: The host browser tab does not expose the local mic as a remote track; `_participantsMap` contains the local participant but its `audioTrack` is a capture (not a receive) track, handled differently by Jitsi. Routing local mic through the worklet would require a parallel capture pipe and no benefit: there's no ambiguity about who the local audio belongs to.

### D8. AudioContext sharing with existing VAD analyser
**Choice**: Reuse the single `AudioContext` created in `audio-analyzer.js:8-13`. The worklet for per-speaker PCM is added as a downstream node of the existing `MediaStreamSource` per participant (not a new source).

**Why**: Avoid double `createMediaStreamSource` — the browser is fine with it but it doubles the AudioContext graph and the VAD analyser + the worklet should see the same samples. Also cleaner teardown.

### D9. Backwards compatibility with stored transcripts
**Choice**: Transcript JSONL format is unchanged. Only the `speaker` field content becomes more reliable. Meetings recorded before this change are unaffected.

## Risks / Trade-offs

- **[Worklet processor CPU on Chrome]** → Worklet runs on a dedicated audio render thread; decimation is cheap (~0.02 ms per 128-sample quantum). Budget is fine.
- **[WebSocket back-pressure during long ASR stalls]** → Bridge will drop oldest PCM chunks from a pid's ring buffer rather than unbounded-queue. Budget: ring of ~3×chunk_duration worth of audio per pid.
- **[Pid reuse / churn — participant rejoins with new pid mid-meeting]** → Transcript groups by display-name within a meeting; pid changes are invisible to the user. ECAPA-TDNN fallback (existing) could later correlate identities across sessions but is not in scope.
- **[Extension/bridge version skew during rollout]** → Handshake with `protocol_version` negotiation. Old extension → old mixed path; new extension + old bridge → new protocol ignored, falls back.
- **[Worklet load timing — participants joining before worklet initialised]** → `trackAdded` event already fires in `audio-analyzer.js` for newcomers; attach worklet there. For initial scan, `attachAllFromConf(conf)` already iterates current participants.
- **[PCM bandwidth on loopback]** → 16 kHz × 16 bit × mono × 5 pids ≈ 160 KB/s. Localhost WS trivially handles this.
- **[Chunk boundaries split a word]** → `asr.py:138 (_remove_duplicate_words)` already de-duplicates on overlap. Extend worklet to emit with 300 ms overlap between chunks so the deduper has context.
- **[GigaAM batched forward on variable-length inputs]** → `model.forward(features, feature_lengths)` accepts a length tensor; internal padding handles unequal lengths. Verified conceptually; explicit test in tasks.

## Migration Plan

1. **Phase 1 (this change)**: Ship worklet + new WS frame + new Python path, all gated on `protocol_version >= 2` handshake. Old path remains default. No user action.
2. **Phase 2 (this change)**: Bump extension version and rebuild `injected.js`. User reloads Chrome extension once. From next meeting, per-speaker path activates automatically.
3. **Phase 3 (follow-up, separate change)**: Once stable for a week on real meetings, mark mixed-path for KTalk sessions as legacy. Keep mixed path for non-KTalk sessions indefinitely.

**Rollback**: Revert the extension version (or the `protocol_version` bump). Bridge keeps handling both protocols. No data migration needed (transcripts unchanged).

## Open Questions

1. **Guest identity across sessions**: Anonymous guest "Имя Фамилия" gets fresh pid every meeting. Should we link past sessions via ECAPA-TDNN embedding on the new per-pid streams (much cleaner signal than the current mixed-stream enrollment)? Proposed: out of scope for this change, but worth documenting as a natural follow-up.
2. **Partial-chunk at join/leave**: If a participant joins mid-chunk, the first chunk for their pid is <3 s. Should we discard it, pad with silence, or transcribe short? Proposed default: transcribe as-is (GigaAM tolerates <3 s inputs).
3. **Opus decoding quality**: Browser decodes Opus transparently to the worklet. No known quality issues, but worth verifying that decoded PCM ≈ original input within Opus codec loss.
4. **Mid-meeting WS disconnect**: If extension disconnects, should live.py fall through to mixed-capture path for the rest of the meeting, or stop until reconnect? Proposed default: halt per-speaker path, let mixed path resume if available; log the gap in transcript.
