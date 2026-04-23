## ADDED Requirements

### Requirement: Rolling-window refinement of recent PCM with word-level timestamps

When `RECOG_PCM_REFINE=1` and `RECOG_PCM_TRANSPORT=1` are both set, the system SHALL maintain a per-participant rolling PCM buffer of approximately 20 seconds and SHALL re-transcribe recently-closed replies with GigaAM's word-level timestamp API, then apply the refined text to the live transcript in place.

#### Scenario: Gap-driven refine on reply close

- **WHEN** a participant has been silent for at least `SPEAKER_TURN_GAP` seconds AND their rolling buffer contains un-refined audio
- **THEN** the refine worker transcribes the un-refined window with `word_timestamps=True`
- **AND** emits one `(segment_id, new_text)` tuple per live segment whose audio range intersects the window

#### Scenario: Monolog fallback

- **WHEN** a participant has spoken continuously for at least `PCM_REFINE_MONOLOG_CAP_S` seconds without a `SPEAKER_TURN_GAP`-sized pause
- **THEN** the refine worker transcribes the trailing 20-second window
- **AND** emits refined text for segments in that window as above

#### Scenario: No duplicate emission

- **WHEN** the refined text for a segment is byte-identical to the segment's current `text`
- **THEN** the system SHALL NOT emit a `transcript_refine` message for that segment
- **AND** the segment's `refined` flag SHALL remain `false`

#### Scenario: Empty alignment result

- **WHEN** the refined transcription for a window contains no words whose timestamps fall inside a given live segment's audio range
- **THEN** that segment SHALL remain tier-1 (no update, no `transcript_refine`)
- **AND** the system logs a counter metric for diagnostic purposes

#### Scenario: Sanity filter rejects gross refine drift

- **WHEN** a refined `new_text` differs from the live segment's `text` by more than 3× word count OR by Levenshtein distance exceeding 80% of the longer string
- **THEN** the system SHALL NOT emit `transcript_refine` for that segment
- **AND** the segment SHALL remain tier-1
- **AND** a `refine.rejected_sanity` counter SHALL increment (observable via `/api/pcm-diagnostics`)

#### Scenario: Runtime toggle pauses refine without restart

- **WHEN** an operator issues `POST /api/pcm-diagnostics {"refine_enabled": false}`
- **THEN** the refine worker SHALL pause within 1 second
- **AND** no new `transcript_refine` messages SHALL be emitted until a subsequent `POST {"refine_enabled": true}` or service restart

### Requirement: Inline text replacement preserves segment identity

The system SHALL apply refined text by rewriting only the `text` field and setting `refined = true` on the matching `segment_id`. All other fields of a segment (`id`, `ts`, `ts_display`, `speaker`, `speaker_id`, `is_local`, `cont`, `uncertain`, `uncertain_reason`, `audio_offset_ms`) SHALL remain unchanged.

Refined word timestamps SHALL be rebased into the per-pid audio timeline (`pid_audio_ms`) — NOT the mixed-wav timeline used by `audio_offset_ms` — before alignment onto stored live-segment boundaries (`pid_audio_ms_start`, `pid_audio_ms_end`). Segments record both timelines at append time; only the per-pid frame is used for refine alignment.

#### Scenario: Refined text is persisted to JSONL

- **WHEN** the refine worker completes alignment for `segment_id = N`
- **THEN** the row with `id == N` in `transcript.jsonl` is rewritten with the new `text` and `refined: true`
- **AND** every other field on that row matches its pre-refine value

#### Scenario: Speaker correction and refinement are commutative

- **WHEN** `_retroactively_correct_unknown_speakers` and `_apply_refine` both target `segment_id = N` (in either order)
- **THEN** the final row has both the corrected `speaker` AND the refined `text`
- **AND** `uncertain`, `uncertain_reason`, `refined` flags reflect the union of both operations

#### Scenario: Meeting switch between dispatch and apply

- **WHEN** a refine task is dispatched before a meeting switch but completes after
- **THEN** the result is dropped without writing to any JSONL
- **AND** no `transcript_refine` message is broadcast

### Requirement: `transcript_refine` WebSocket message

The KTalk bridge SHALL broadcast a new WebSocket message type `transcript_refine` on `ws://localhost:8765` whenever one or more segments are refined. The webapp frontend SHALL mirror the same message on `ws://127.0.0.1:9080` to its clients.

#### Scenario: Message shape

- **WHEN** the bridge sends a refine message
- **THEN** the JSON payload is `{type: "transcript_refine", updates: [{segment_id: int, text: str, refined_at: iso8601}, ...]}`
- **AND** the `updates` list contains one entry per refined segment

#### Scenario: Unknown-type tolerance

- **WHEN** a client receives `transcript_refine` but has no handler for it
- **THEN** the message is silently dropped (per existing WS client behaviour)
- **AND** the live `transcript` message flow continues unaffected

#### Scenario: Frontend delivery independent of JSONL tail

- **WHEN** `MeetingStorage.update_segments` rewrites `transcript.jsonl` via atomic `os.replace`
- **THEN** the webapp WS clients on `ws://127.0.0.1:9080` still receive `transcript_refine` within 200 ms
- **AND** the delivery does NOT depend on the JSONL tail watcher observing the rewrite
- **AND** the delivery source is a bridge-originated subscription (per design D10)

### Requirement: Tier-1 / tier-2 visual lifecycle in webapp

The webapp frontend SHALL render each transcript row with a visual tier that reflects its `refined` state. Tier-1 (un-refined) rows SHALL be visually distinguishable from tier-2 (refined) rows.

#### Scenario: New live row renders as tier-1

- **WHEN** the webapp receives a `transcript` message for a new segment
- **THEN** it creates a row with `data-refined="false"`, opacity 0.6, and a dashed left-border accent
- **AND** the row carries a tooltip explaining the provisional state

#### Scenario: Refine promotes to tier-2

- **WHEN** the webapp receives `transcript_refine` for `segment_id = N`
- **AND** a row with `data-segment-id="N"` exists in the DOM
- **THEN** the row's text content is replaced with the refined text
- **AND** `data-refined` flips to `"true"`
- **AND** opacity animates to 1.0 and the border accent becomes solid over 200 ms

#### Scenario: Refine arrives before the row is in view

- **WHEN** the webapp receives `transcript_refine` for a segment that is not currently in the DOM (e.g., scrolled far above the viewport or not yet rendered)
- **THEN** the refined text is stored so that when the row is eventually rendered (e.g., from JSONL replay on reconnect) it appears already as tier-2

### Requirement: Extension overlay text rewrite on refine

The Chrome extension overlay SHALL replace the text of a displayed bubble in place when a `transcript_refine` message arrives for the corresponding `segment_id`. The overlay does not render tier-1/tier-2 visual differentiation (space-constrained).

#### Scenario: Bubble rewrite

- **WHEN** the extension overlay receives `transcript_refine` for `segment_id = N`
- **AND** a bubble with `data-segment-id="N"` is currently displayed
- **THEN** the bubble's text content is replaced with the refined text
- **AND** a 100 ms fade-out / fade-in transition signals the change

### Requirement: Refine worker yields to live path

The refine contour SHALL share a single `GigaAM` instance with the live path under a mutex, and the live path SHALL have priority on contention.

#### Scenario: Live batch preempts refine (cooperative-abort mode)

- **WHEN** `RefineWorker` is in `cooperative` mode (task 2.3 spike confirmed a decoder step hook) AND a live PCM batch is ready AND the shared model lock is held by an in-progress refine inference
- **THEN** the refine worker aborts its current task within one decoder step (< 200 ms typical)
- **AND** the live batch acquires the lock within 500 ms
- **AND** the aborted refine task is re-enqueued

#### Scenario: Live batch waits (coarse-serialisation mode)

- **WHEN** `RefineWorker` is in `coarse` mode (task 2.3 spike showed no suitable hook) AND a live PCM batch is ready AND the shared model lock is held by an in-progress refine inference
- **THEN** the refine worker completes the current window before releasing the lock
- **AND** the live batch acquires the lock within `PCM_REFINE_WINDOW_S × max(RTF_cpu, RTF_mps)` seconds (≤ 3 s CPU, ≤ 0.7 s MPS)
- **AND** the refine task is NOT re-enqueued (its output is applied normally)

#### Scenario: No memory doubling

- **WHEN** the refine contour is active with 5 participants each accumulating a 20-second rolling buffer
- **THEN** the number of loaded GigaAM instances remains 1
- **AND** additional RAM for PCM ring buffers stays below 10 MB

### Requirement: Refine disabled in fallback_mixed mode

The system SHALL NOT emit refine tasks, apply refined text, or broadcast `transcript_refine` messages while `PerSpeakerIngest._degraded == True`.

#### Scenario: Watchdog trip pauses refine

- **WHEN** the PCM silence watchdog flips the system into fallback_mixed
- **THEN** all queued refine tasks are dropped
- **AND** no new refine tasks are accepted
- **AND** existing tier-1 rows stay tier-1 (no spurious promotions)

#### Scenario: Recovery resumes refine

- **WHEN** PCM chunks begin flowing again after a watchdog trip AND the `[PCM recovered]` marker is written
- **THEN** the refine worker resumes accepting tasks from the next gap-driven or monolog-triggered event
- **AND** replies that closed during the degraded window remain tier-1 permanently

## MODIFIED Requirements

### Requirement: Preserve chunk duration

The per-speaker **live** path SHALL use `chunk_duration = 3.0` seconds and overlap = 600 ms. The per-speaker **refine** path SHALL use a 20-second rolling window. The live chunk duration MUST NOT change due to refine being enabled or disabled.

#### Scenario: Default chunk duration (live)

- **WHEN** the per-speaker live path is active
- **THEN** each live PCM chunk spans 3.0 seconds of audio (48000 samples at 16 kHz)
- **AND** chunks overlap by 600 ms so the existing `_remove_duplicate_words` deduplication can bridge boundaries

#### Scenario: Refine window duration

- **WHEN** the refine worker triggers on a gap or monolog cap
- **THEN** it transcribes up to the trailing 20 seconds of the pid's rolling buffer
- **AND** less than 20 seconds if the pid has been active for less than that
- **AND** the live-path chunk duration is not affected
