# Golden sessions: recording and replaying the live process

Status: the recorder is built (`recog/golden.py`, `[live] golden_record`, off by
default); the clock seam and the player are not. Step 0 of
[the move to Rust](2026-11-07-rust-migration.md).

## Purpose

Steps 4 to 7 of the move replace the live process piece by piece. Each replacement must
produce what the Python one produces for the same meeting. A golden session is one
meeting's inputs, recorded at the boundary of the live process, plus what the
process wrote. A player feeds the inputs to either implementation and compares the
outputs.

## What makes a replay differ from the original

Mapped from the code on 2026-11-07:

- **The ASR model.** Different backends and devices give different text. A refine can
  also be aborted part-way when the live path takes the lock.
- **Wall-clock time** (`time.time()`, `datetime.now()`).
  - It sets segment `ts` when a row has no `captured_at`.
  - It decides speaker attribution: the bridge keeps a speaker history stamped on
    arrival.
  - It sets `_clock_offset` from `clock_sync`, PCM continuation (`live.py`, around
    `_on_pcm_batch`), `audio_offset_ms`, meeting ids (time plus `uuid4`),
    `created_at`/`ended_at`, and the segment timeout in the bridge's 100 ms loop.
- **Thread interleaving.** The ingest worker, the refine worker, the mic/system loop
  and the bridge's event loop run in parallel, so the order of segment ids across
  sources depends on scheduling.
- **The tick rate.** Refine triggers advance per ingest tick
  (`_silence_ms += PCM_BATCH_INTERVAL_MS`), not per millisecond of audio.

## Design

1. **Record at four taps**, each line stamped with monotonic time since the start:
   - **Bridge frames:** every frame `_process_message` receives, plus connect and
     disconnect of each socket from `_handle_client`. The socket is identified by a
     per-session number, not by its address.
   - **Per-speaker PCM:** the `PcmChunk` handed to `PerSpeakerIngest.enqueue`,
     recorded after the clock offset has been applied.
   - **Microphone and system audio:** what `BufferedAudioCapture.get_chunk` returns,
     the samples together with `capture_start_ts`.
   - **ASR answers:** the input hash and the output of every `transcribe_batch`,
     `transcribe_chunk` and `transcribe_with_timestamps` call.

   All of this goes to `<meeting>/golden/` as JSONL plus raw PCM. It is off by
   default, switched on by a setting, and never sent anywhere. A session holds
   personal audio and text, so it stays under the data directory. Committed fixtures
   are only synthetic ones built by tests.

   As built (2026-11-07): one directory per live process, `<data dir>/golden/<start>/`,
   not per meeting, since state carries across meeting switches and a replay starts
   where the process started. The per-speaker PCM tap is not separate: `pcm_chunk`
   frames are recorded as the bridge receives them, PCM included, and a replay sends
   them through the bridge again, which applies the clock offset itself. Events
   carry monotonic `t` and `time.time()` as `wall`; capture samples go to
   `audio.bin` as `get_chunk` returned them (float32), so the replay hands ASR the
   same bytes and finds its answer by key. Audio and ASR answers are recorded only
   while a conference is on: the service listens all day, and a day of idle
   microphone would be about 11 GB. So orphan recordings are not covered yet.
   Sessions older than 14 days are deleted when the next one starts; a directory
   renamed to end with `-keep` is kept.
2. **Replay with a fake clock and recorded ASR.**
   - The player owns a clock that the bridge, live, ingest and storage read. That
     means adding a clock parameter where `time.time()` is called today.
   - It feeds frames and audio at their recorded times.
   - It answers ASR calls from the recording, looked up by input hash. The pipeline
     is then compared without the model; the model is compared separately, as in the
     [ASR experiment](../experiments/2026-11-07-rust-asr-runtimes.md).
   - Ticks run per recorded tick, not by sleeping.
3. **Compare normalized outputs:**
   - transcript rows, without ids and with timestamps relative to the session start;
   - `meta.json` without times;
   - broadcast frames, in the order received per client;
   - WAV sample counts.

   Ordering across sources is compared as a set within each tick, not as one
   sequence.
4. **Acceptance for a replacement:** the same normalized outputs on every golden
   session, or a reviewed list of differences.

## Pieces of work, in order

1. A clock seam: one `Clock` object passed to `KTalkBridge`, `LiveTranscriber`,
   `PerSpeakerIngest` and `MeetingStorage`; the default is the system clock, so
   behaviour does not change.
2. The recorder behind a setting, tested with a synthetic session. **Done.**
3. The player for the Python implementation: a recorded session replays to the same
   normalized outputs. This is the player's own acceptance test.
4. Recording a few real sessions on the user's machine: a KTalk meeting, Google Meet,
   a Zoom web call, and an orphan microphone recording.

`cargo test` and clippy join `scripts/check.sh` and CI with the first Rust code in the
repository (step 2), not here.
