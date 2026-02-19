# KTalk Speaker Attribution Optimization

**Date**: 2026-02-19
**Status**: Approved for implementation

## Problem

KTalk chunks are sometimes assigned to the wrong speaker. The root cause is a fundamental timing mismatch: audio is captured in 3-second chunks, but speaker identity is queried as a single point-in-time check (`get_current_speaker()`) at the moment the chunk finishes processing. During those 3 seconds, the speaker may have changed multiple times.

## Architecture Context

```
Chrome Extension (speaker.js)
    ↓ WebSocket (speaker_update with Date.now() timestamp)
KTalk Bridge (ktalk_bridge.py)
    ↓ get_current_speaker() — point-in-time query
Live Transcriber (live.py)
    → Assigns speaker to entire 3-second audio chunk
```

## Identified Bugs (9 total)

### Critical (cause wrong speaker)

1. **Point-in-time speaker query** — `live.py:738` calls `_get_ktalk_speaker()` which returns whoever is speaking *now*, not who spoke during the chunk's capture window. If Speaker A spoke for 2.5s and Speaker B started in the last 0.5s, the entire chunk is attributed to Speaker B.

2. **Speaker Memory 3s** — `ktalk_bridge.py:257` `SPEAKER_MEMORY_SECONDS = 3.0`. When a speaker stops, the bridge returns their name for 3 more seconds. If Speaker A stops and Speaker B starts within 3s, queries during the gap still return Speaker A.

3. **Clock skew** — Extension sends `Date.now()` (JS clock), bridge uses `time.time()` (Python clock). These can differ by 100-500ms. Segment timestamps (`event_time` from JS) are compared with `time.time()` for speaker memory, mixing two unsynchronized clocks.

### Medium (cause stale/wrong data)

4. **Out-of-order WebSocket events** — No sequence numbers on `speaker_update` messages. If network delays reorder events, segment boundaries are calculated incorrectly.

5. **Mute check after transcription** — `live.py:698-701`: `is_local_mic_muted()` check happens *after* `_transcribe_direct()` at line 674. The expensive ASR inference runs even when mic is muted, wasting CPU.

6. **Stale participant mic state** — Mic mute status polled every 2 seconds (init.js:287 `startParticipantsRefresh`). Quick mute toggles are missed, causing `are_all_mics_muted()` to return stale data.

7. **Race condition in callbacks** — `ktalk_bridge.py:1117-1127`: `_on_speaker_change` callback invoked inside `self._lock`, but `_on_segment_complete` callbacks invoked outside. If callback reads bridge state, it may see intermediate values.

### Minor

8. **No segment auto-flush** — Segments only complete when a new `speaker_update` arrives. If no events come for 20+ seconds, the segment stays open.

9. **"Unknown" at startup** — Before the first `speaker_update`, all chunks are labeled "Unknown" with no retroactive correction.

## Solution: Speaker History Window

### Core Idea

Instead of asking "who is speaking NOW?", ask "who spoke MOST during the chunk's capture period?"

```
Before:  chunk[0s...3s] → get_current_speaker() at t=3s → "Speaker B"
After:   chunk[0s...3s] → get_dominant_speaker(t=0s, t=3s) → "Speaker A" (spoke 2.5s)
```

### Implementation Plan

#### Stream 1: ktalk_bridge.py (core fixes)

**Fix 1: `get_dominant_speaker_for_period(start_time, end_time)`**
- Use existing `_speaker_history: List[SpeakerEvent]` which already records every speaker change with timestamps
- Walk history to calculate which speaker has the most speaking time in the given window
- Return the dominant speaker name (or None if no data)
- Fallback to `get_current_speaker()` if history is empty

```python
def get_dominant_speaker_for_period(self, start_time: float, end_time: float) -> Optional[str]:
    """Returns the speaker who spoke the most during [start_time, end_time]."""
    with self._lock:
        # Build timeline of who was speaking when
        speaker_durations: Dict[str, float] = {}

        # Find relevant events in the window
        relevant_events = [e for e in self._speaker_history
                          if e.timestamp <= end_time]

        if not relevant_events:
            return self._get_current_speaker_unlocked()

        # Calculate speaking duration per speaker in the window
        for i, event in enumerate(relevant_events):
            if event.speaker_name is None:
                continue

            event_start = max(event.timestamp, start_time)
            # Event ends at next event's timestamp or end_time
            if i + 1 < len(relevant_events):
                event_end = min(relevant_events[i + 1].timestamp, end_time)
            else:
                event_end = end_time

            if event_end > event_start:
                duration = event_end - event_start
                speaker_durations[event.speaker_name] = (
                    speaker_durations.get(event.speaker_name, 0) + duration
                )

        if not speaker_durations:
            return self._get_current_speaker_unlocked()

        # Return speaker with most speaking time
        return max(speaker_durations, key=speaker_durations.get)
```

**Fix 2: Reduce SPEAKER_MEMORY_SECONDS**
- Change from 3.0 to 0.8 seconds
- 0.8s is enough to cover WebSocket round-trip latency without causing false attribution

**Fix 3: Segment auto-flush**
- Add periodic check in the WebSocket server loop (every 1s)
- If active segment exceeds MAX_SEGMENT_DURATION, force-complete it

**Fix 4: Fix callback race condition**
- Collect completed segments into a list inside the lock
- Call all callbacks outside the lock after releasing it
- (Already partially done for `_on_segment_complete`, need to also do for `_on_speaker_change`)

#### Stream 2: live.py (consumer fixes)

**Fix 5: Timestamp-aware chunk processing**
- Record `chunk_start_time = time.time()` at the beginning of `process_chunk()`
- After chunk is captured, compute `chunk_end_time = time.time()`
- Call `bridge.get_dominant_speaker_for_period(chunk_start_time, chunk_end_time)` instead of `get_current_speaker()`

```python
# Before (live.py:738):
ktalk_speaker = self._get_ktalk_speaker()

# After:
chunk_start_time = time.time() - self.chunk_duration
chunk_end_time = time.time()
ktalk_speaker = self._ktalk_bridge.get_dominant_speaker_for_period(
    chunk_start_time, chunk_end_time
) if self._ktalk_bridge else self._ktalk_current_speaker
```

**Fix 6: Move mute check before ASR**
- Move `is_local_mic_muted()` check before `_transcribe_direct()`
- Save ~200ms CPU per muted chunk

```python
# In process_chunk(), before transcription:
if is_mic and self._ktalk_bridge and self._ktalk_bridge.is_local_mic_muted():
    return  # Skip before expensive ASR
```

**Fix 7: Handle "Unknown" speaker at startup**
- If no speaker data yet, buffer the chunk text
- When first speaker event arrives, retroactively assign buffered chunks
- Alternative (simpler): use `state.last_speaker` as fallback instead of "Unknown"

#### Stream 3: Chrome Extension (ws_api.js + speaker.js)

**Fix 8: Sequence numbers**
- Add monotonically increasing `seq` to each `speaker_update`
- Bridge checks sequence and drops out-of-order messages

```javascript
// ws_api.js
let messageSeq = 0;
export function sendWsSpeakerUpdate(speaker) {
    sendWsMessage({
        type: 'speaker_update',
        speaker: speaker,
        timestamp: Date.now(),
        seq: ++messageSeq
    });
}
```

Bridge side:
```python
# In _process_message:
seq = data.get("seq")
if seq is not None and seq <= self._last_seq:
    return  # Drop out-of-order
self._last_seq = seq
```

**Fix 9: Clock sync handshake**
- On WebSocket connect, extension sends `{ type: "clock_sync", browser_time: Date.now() }`
- Bridge records `offset = time.time() * 1000 - browser_time`
- All subsequent `event_time` calculations apply this offset
- Re-sync every 30 seconds

## Parallel Execution Plan

Three work streams that can be implemented concurrently:

| Stream | Agent | Files | Fixes |
|--------|-------|-------|-------|
| 1 (Bridge) | worker-bridge | `ktalk_bridge.py` | #1, #2, #3, #4 |
| 2 (Live) | worker-live | `recog/live.py` | #5, #6, #7 |
| 3 (Extension) | worker-live | `kontur-diarize/ktalk-speaker-extension/src/ws_api.js`, `speaker.js` | #8, #9 |

**Dependencies:**
- Stream 2 depends on Stream 1 (Fix 5 uses new `get_dominant_speaker_for_period()` method)
- Stream 3 depends on Stream 1 (Fix 8-9 require bridge-side handling)
- Solution: Stream 1 goes first, Streams 2+3 start after bridge API is ready

## Testing

No automated test suite (per CLAUDE.md). Manual verification:
```bash
python -m compileall recog ktalk_bridge.py  # Syntax check
python mp4_notes.py --help                   # CLI smoke test
```

For live testing:
1. Start live transcription with `--ktalk`
2. Join KTalk meeting
3. Have multiple speakers alternate rapidly
4. Verify speaker attribution accuracy in transcript

## Risk Assessment

- **Low risk**: Fixes 2, 5, 6, 8 are localized changes with clear behavior
- **Medium risk**: Fix 1 (new method) — needs careful handling of edge cases (empty history, single speaker)
- **Low risk**: Fix 9 (clock sync) — additive, doesn't break existing flow
- **No breaking changes**: All fixes are backward-compatible; extension protocol changes are additive (new fields ignored by old bridge)
