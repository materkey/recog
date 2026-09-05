## 1. Protocol + handshake foundation

- [ ] 1.1 Define `protocol_version` constant (value `2`) in a new `kontur-diarize/ktalk-speaker-extension/src/protocol.js`, exported and consumed from `ws.js`
- [ ] 1.2 Add `protocol_version` field to the extension's initial `connection_status` message sent on WebSocket connect
- [ ] 1.3 In `ktalk_bridge.py`, record `protocol_version` per connected client and expose it via an accessor (`client_protocol_version(websocket)`)
- [ ] 1.4 Add a unit test in `tests/test_ktalk_bridge.py` that verifies the bridge records and returns the protocol version correctly, and defaults to 1 when absent

## 2. AudioWorklet downsampler (browser side)

- [ ] 2.1 Create `kontur-diarize/ktalk-speaker-extension/src/pcm-worklet-processor.js` as an `AudioWorkletProcessor` that consumes 128-sample Float32 quanta at 48 kHz, applies a simple 3-tap low-pass + 3:1 decimation, and accumulates into a chunk buffer
- [ ] 2.2 In the worklet, emit a chunk via `this.port.postMessage({pid, seq, samples, ts_ms, pcm_int16})` every `chunk_duration=3.0s` of captured audio, with 300 ms overlap between consecutive chunks
- [ ] 2.3 Create `kontur-diarize/ktalk-speaker-extension/src/pcm-sink.js` that manages per-participant `AudioWorkletNode` instances sharing the existing `AudioContext` from `audio-analyzer.js`
- [ ] 2.4 In `pcm-sink.js`, implement `attachParticipant(pid, mediaStream)` and `detachParticipant(pid)` mirroring the existing `audio-analyzer.js` lifecycle, and wire them to the same `trackAdded` / `trackRemoved` / `userLeft` events in `platforms/ktalk.js`
- [ ] 2.5 Register the worklet module via `audioContext.audioWorklet.addModule(chrome.runtime.getURL('pcm-worklet-processor.js'))` and add the file to `web_accessible_resources` in `manifest.json`
- [ ] 2.6 Ensure `dispose()` in the KTalk adapter tears down all worklet nodes before the audio-analyzer dispose runs

## 3. PCM transport

- [ ] 3.1 In `kontur-diarize/ktalk-speaker-extension/src/ws.js`, add `sendPcmChunk({pid, seq, samples, ts_ms, pcm_int16})` that builds the binary frame (header-length u32 LE + JSON header + Int16LE PCM) and sends it as a single WebSocket binary message
- [ ] 3.2 Wire `pcm-sink.js` message handler to call `sendPcmChunk`
- [ ] 3.3 In `ktalk_bridge.py`, register a binary WebSocket message handler that parses the frame format and dispatches `pcm_chunk` events to a new callback (`set_pcm_chunk_callback`)
- [ ] 3.4 Validate frame integrity: reject messages with inconsistent header-length / payload-size and log an error without disconnecting the client
- [ ] 3.5 Add a unit test in `tests/test_ktalk_bridge.py` that feeds a known binary frame through `_process_message` and asserts the callback is invoked with the expected `(pid, seq, pcm_int16, ts_ms)` tuple

## 4. Per-PID ingest pipeline in Python

- [ ] 4.1 Add a new module `recog/per_speaker.py` containing `PerSpeakerIngest` class with per-pid ring buffers (3 × chunk_duration capacity, drop-oldest on overflow) and sequence-number reordering
- [ ] 4.2 In `PerSpeakerIngest`, implement a batch collector: wait up to 200 ms for additional chunks from other pids, then build a `(batch_wav, batch_len, pid_list)` triple and invoke a transcribe callback
- [ ] 4.3 Extend `recog/asr.py` `ASRModule` with `transcribe_batch(wavs: list[Tensor], lengths: Tensor) -> list[str]` that stacks inputs into a `[B, T_max]` tensor, calls `model.forward`, then `model.decoding.decode` and returns per-sample text. Reuse the `_remove_duplicate_words` helper at the boundary level.
- [ ] 4.4 In `recog/live.py`, add a new `_setup_per_speaker_ingest()` method that instantiates `PerSpeakerIngest` and registers its chunk callback with `self._ktalk_bridge.set_pcm_chunk_callback`
- [ ] 4.5 Gate the per-speaker path on `use_ktalk=True AND bridge.client_protocol_version() >= 2`. If either is false, the mixed-capture path runs as today.
- [ ] 4.6 When emitting transcript entries from the per-speaker path, set `speaker` directly from the pid → display-name mapping in `bridge.get_participants()`. Do NOT call `get_current_speaker` / `get_dominant_speaker_for_period` on these chunks.

## 5. Mixed-path preservation

- [ ] 5.1 Verify (by inspection + test) that when `use_ktalk=False`, zero code paths from `recog/per_speaker.py` or the new bridge handler are exercised
- [ ] 5.2 Verify that when `use_ktalk=True` but `protocol_version < 2`, the pipeline still uses `_setup_ktalk_bridge`'s existing wiring and dominant-speaker attribution
- [ ] 5.3 Add a regression test in `tests/test_live_speaker_attribution.py` asserting old attribution logic is unchanged for a `protocol_version=1` mock client

## 6. VAD-gate toggle

- [ ] 6.1 Add a `pcmVadGate` boolean setting to the extension's popup storage (`popup.html` + `popup.js`), default `false`
- [ ] 6.2 In `pcm-sink.js`, when the toggle is `true`, consult `isSpeech(pid)` from `audio-analyzer.js` before forwarding a chunk to `sendPcmChunk`; when `false`, forward unconditionally
- [ ] 6.3 Document the toggle in the extension `README.md`

## 7. Rebuild + integration smoke

- [ ] 7.1 Update webpack entry if needed and run `npm run build` in `kontur-diarize/ktalk-speaker-extension/` so that `injected.js` and the worklet file are built and included in `web_accessible_resources`
- [ ] 7.2 Bump extension `version` in `manifest.json` to `1.3.0`
- [ ] 7.3 Manual smoke test: start `recog-service.sh`, reload extension in Chrome, join a KTalk meeting with at least one other participant, verify transcript emits per-speaker text for 5 minutes without drops
- [ ] 7.4 Kill the live service mid-meeting, relaunch, verify that reconnection works and no duplicate text is emitted for chunks already processed

## 8. E2E test for overlap attribution

- [ ] 8.1 Add a new e2e scenario in `tests/e2e/` that launches host + 2 fake-audio guests with `speech_reference.wav` playing on both, overlapping for at least 5 seconds
- [ ] 8.2 Extend the existing e2e orchestrator (or add a new one) to assert: final transcript contains entries from BOTH guest names during the overlap window, AND neither guest's words appear under the other guest's tag
- [ ] 8.3 Gate the new e2e test under an existing marker (`-m e2e`) and document it in `tests/e2e/README.md`

## 9. Documentation + ADR

- [ ] 9.1 Write ADR entry `docs/adr/ADR-0014-per-speaker-asr-pipeline.md` summarizing the decision, alternatives considered (from design.md), and bench results
- [ ] 9.2 Update repo-level `CLAUDE.md` Architecture section to mention the per-speaker path and when it activates
- [ ] 9.3 Update `kontur-diarize/ktalk-speaker-extension/README.md` with the new protocol version note and the VAD-gate toggle

## 10. Rollout + cleanup

- [ ] 10.1 After one week of stable operation, open a follow-up change to remove the mixed-capture path for KTalk sessions specifically (while keeping it for non-KTalk)
- [ ] 10.2 Archive this change via `openspec archive per-speaker-asr-pipeline` once tasks 1–9 are complete and manual smoke has passed
