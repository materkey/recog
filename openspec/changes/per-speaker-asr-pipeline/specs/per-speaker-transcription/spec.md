## ADDED Requirements

### Requirement: Per-participant audio capture from Jitsi SFU

When a KTalk conference is active and the Chrome extension reports protocol version 2 or higher, the system SHALL capture audio from each remote participant as an independent PCM stream sourced from that participant's Jitsi `MediaStreamTrack`.

#### Scenario: Two remote participants in same room

- **WHEN** the user joins a KTalk room with two other participants
- **THEN** the extension establishes one `AudioWorkletNode` downsampler per remote participant
- **AND** each node emits 16 kHz mono Int16 PCM derived solely from that participant's stream
- **AND** cross-talk between participant streams is absent (RMS on participant A while only participant B speaks is below 0.001)

#### Scenario: New participant joins mid-meeting

- **WHEN** a new participant joins after the meeting has started
- **THEN** the extension attaches a worklet to their stream within 500 ms of the `conference.trackAdded` event
- **AND** the first PCM frame for the new participant carries their correct SFU participant ID

#### Scenario: Participant leaves mid-meeting

- **WHEN** a participant leaves the meeting
- **THEN** the extension tears down the worklet for their stream
- **AND** no further PCM chunks with that participant ID are emitted

### Requirement: PCM transport protocol

The Chrome extension SHALL transmit per-participant PCM chunks to the local bridge over the existing WebSocket (`ws://localhost:8765`) using a binary frame format. The bridge SHALL accept these frames alongside the existing JSON control messages.

#### Scenario: Binary frame structure

- **WHEN** the extension emits a PCM chunk
- **THEN** it sends a single WebSocket binary message
- **AND** the message contains a 4-byte little-endian unsigned integer header length, followed by a UTF-8 JSON header `{type: "pcm_chunk", pid, seq, samples, ts_ms}`, followed by exactly `2 * samples` bytes of Int16 little-endian mono PCM at 16 kHz

#### Scenario: Out-of-order frames

- **WHEN** the bridge receives two `pcm_chunk` frames for the same `pid` with `seq` values out of order
- **THEN** the bridge keeps both frames but reorders them by `seq` before enqueuing for ASR

#### Scenario: Protocol version handshake

- **WHEN** the extension connects to the bridge
- **THEN** it sends a JSON control message including a `protocol_version` field
- **AND** the bridge records the version and routes PCM frames only for clients with `protocol_version >= 2`
- **AND** clients with lower protocol versions are served by the existing mixed-capture path without receiving any PCM

### Requirement: Deterministic speaker tagging

The system SHALL tag each transcribed text fragment with the participant ID (and resolved display name) from the PCM chunk that produced it. The system SHALL NOT rely on dominant-speaker timing heuristics for chunks processed via the per-speaker path.

#### Scenario: Single speaker chunk

- **WHEN** a 3-second chunk from participant A is transcribed
- **THEN** the emitted transcript entry has `speaker = A.display_name`
- **AND** the entry is not consulted against `_speaker_history` for attribution

#### Scenario: Simultaneous speech (overlap)

- **WHEN** two participants A and B both speak in overlapping 3-second windows
- **THEN** the transcript contains two separate entries, one per participant, each with correct display name
- **AND** neither entry is mis-attributed to the other participant

### Requirement: Shared-model batched ASR inference

The Python live pipeline SHALL process per-speaker PCM chunks using a single loaded `GigaAM` instance and batched `forward()` calls.

#### Scenario: Multiple pending chunks

- **WHEN** PCM chunks from 3 participants arrive within a 200 ms window
- **THEN** the pipeline groups them into one batch and invokes the model's forward pass once
- **AND** each chunk's transcription is correctly associated with its originating participant ID

#### Scenario: Single chunk

- **WHEN** a PCM chunk arrives with no other pending chunks
- **THEN** the pipeline waits up to 200 ms for additional chunks
- **AND** fires a single-element forward pass after the timeout

### Requirement: Backward-compatible mixed-capture fallback

The system SHALL preserve the existing mixed-capture path (BlackHole or ScreenCaptureKit + `SingleDeviceCapture`) for non-KTalk sessions and for KTalk sessions where the new protocol is unavailable.

#### Scenario: Non-KTalk session

- **WHEN** the user runs live transcription without the `--ktalk` flag
- **THEN** audio capture goes through BlackHole / ScreenCaptureKit as before
- **AND** no worklet-based per-speaker processing is engaged

#### Scenario: KTalk session with old extension

- **WHEN** the user is in a KTalk meeting but the extension reports a protocol version below 2
- **THEN** audio capture uses the mixed-capture path with dominant-speaker attribution as before
- **AND** the transcript format is unchanged

#### Scenario: Mid-meeting protocol failure

- **WHEN** the per-speaker WebSocket frames stop arriving while a KTalk meeting is active
- **THEN** the pipeline stops emitting per-speaker output
- **AND** the transcript continues with a gap marker instead of silently producing wrong-speaker output

### Requirement: Local microphone handling unchanged

The system SHALL continue to capture the local user's microphone through `SingleDeviceCapture` and SHALL NOT route local mic audio through the per-speaker PCM path.

#### Scenario: Local user speaks

- **WHEN** the local user speaks into their microphone
- **THEN** audio is captured via the existing `SingleDeviceCapture` instance
- **AND** the resulting transcript entry is tagged with `MIC_SPEAKER_NAME` (configured via env var, default `Me`)
- **AND** no `pcm_chunk` frame is sent for the local participant

### Requirement: Preserve chunk duration

The per-speaker path SHALL use `chunk_duration = 3.0` seconds, matching the current live-pipeline default.

#### Scenario: Default chunk duration

- **WHEN** the per-speaker path is active
- **THEN** each PCM chunk spans exactly 3.0 seconds of audio (48000 samples at 16 kHz)
- **AND** chunks overlap by 300 ms so the existing `_remove_duplicate_words` deduplication can bridge boundaries

### Requirement: Optional VAD-gate (disabled by default)

The extension SHALL expose a toggle that, when enabled, suppresses PCM transmission for participants whose audio is classified as non-speech by `isSpeech()` from `audio-analyzer.js`. The toggle SHALL default to **disabled**.

#### Scenario: VAD-gate disabled (default)

- **WHEN** a participant is silent (e.g., muted ambient noise only)
- **THEN** PCM chunks are still transmitted
- **AND** the ASR pipeline processes them normally

#### Scenario: VAD-gate enabled

- **WHEN** the toggle is enabled in the extension popup
- **AND** `isSpeech()` returns `{speech: false, reason: "noise"}` for a participant
- **THEN** the worklet suppresses PCM transmission for that participant until `isSpeech()` returns `speech: true`
