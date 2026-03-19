# Fix Unknown Speaker in KTalk Meetings

## Overview
- Fix 0.7-2.3% "Unknown" speaker runs in KTalk-connected meetings
- Root cause: Jitsi `_activeSpeaker$` lags 1-5 seconds behind actual speech; extension's mute filter suppresses stale speaker, leaving bridge with no speaker signal
- Self-reinforcing: "Unknown" doesn't update `last_speaker` (live.py:887), so TTL fallback can't help subsequent chunks
- Two-pronged fix: (A) unmuted participant fallback for real-time, (C) retroactive correction for remaining edge cases
- Constraint: no false attribution — better Unknown than wrong name

## Context (from investigation)

### Validated root cause chain
1. Speaker A talks → extension sees `_activeSpeaker$ = A`
2. Speaker A stops/mutes → Jitsi keeps `_activeSpeaker$ = A`
3. Speaker B starts talking
4. Extension: `_activeSpeaker$ = A`, but `isMicMuted(A) = true` → sends `null`
5. Jitsi hasn't switched dominant speaker to B yet → no positive signal
6. Bridge: `ktalk_speaker = None`, `is_speaker_muted(last_speaker) = true` → fallback blocked
7. live.py:878 → `speaker = "Unknown"`, doesn't update `last_speaker` → chain continues

### Files involved
- `ktalk_bridge.py` — `get_dominant_speaker_for_period()`, `get_current_speaker()`, `_get_current_speaker_unlocked()`, participants tracking
- `recog/live.py:850-890` — speaker assignment logic for system audio chunks
- `recog/live.py:685-740` — `process_pending()` where retroactive correction would apply

### Data validation
- KTalk meetings: 0.7-2.3% Unknown (runs of 5-15 consecutive chunks = 15-45 seconds)
- Unknown always `is_local=False` (system audio only)
- Pattern: Named speaker → Unknown run → Named speaker
- Orphan meetings not in scope (expected behavior)

## Development Approach
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests**
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- Constraint: no false attribution — when 2+ participants unmuted, don't guess

## Implementation Steps

### Task 1: Add "sole unmuted participant" fallback to ktalk_bridge.py
- [x] Add method `get_sole_unmuted_remote_participant() -> Optional[str]` to `KTalkBridge` that returns speaker name when exactly 1 remote participant has mic unmuted, else `None`
- [x] Use existing `self._participants` dict and `mic_muted` field from `participants_update` messages
- [x] Add unit test: 1 unmuted remote → returns their name
- [x] Add unit test: 0 unmuted → returns None
- [x] Add unit test: 2+ unmuted → returns None
- [x] Add unit test: 1 unmuted but is_local=True → returns None (local speaker handled separately)
- [x] Run `python -m compileall recog ktalk_bridge.py` — must pass

### Task 2: Wire unmuted fallback into live.py speaker assignment
- [ ] In `recog/live.py:854-878`, after `get_dominant_speaker_for_period` and `get_current_speaker` both return None, add new fallback tier: call `self._ktalk_bridge.get_sole_unmuted_remote_participant()`
- [ ] Only use this fallback when `self.use_ktalk` is True and bridge is connected
- [ ] Mark segments from this fallback with `uncertain=True, uncertain_reason="sole_unmuted_fallback"` for transparency
- [ ] Update speaker assignment to: ktalk_speaker → last_speaker TTL → sole_unmuted → "Unknown"
- [ ] Add test: sole unmuted fallback is used when dominant/current/TTL all fail
- [ ] Add test: sole unmuted fallback is NOT used when 2+ unmuted (still Unknown)
- [ ] Run `python -m compileall recog` — must pass

### Task 3: Retroactive correction of Unknown runs
- [ ] In `recog/live.py`, when a system-audio chunk gets a positive speaker identification after an Unknown run, scan backwards through recent transcript/meeting segments
- [ ] Re-attribute preceding consecutive "Unknown" segments (same `is_local=False`) to the newly identified speaker
- [ ] Limit retroactive window: max 20 segments or 60 seconds, whichever is smaller
- [ ] Update meeting storage (JSONL) for corrected segments via `MeetingStorage` update method
- [ ] Update in-memory `transcript_lines` for corrected segments
- [ ] Send correction to Chrome extension via `_ktalk_bridge.send_transcript()` with a correction flag
- [ ] Add test: Unknown run followed by identified speaker → all Unknown segments corrected
- [ ] Add test: retroactive correction respects max window limit
- [ ] Add test: correction doesn't cross meeting boundaries
- [ ] Run `python -m compileall recog` — must pass

### Task 4: Verify acceptance criteria
- [ ] Verify KTalk meeting scenario: speaker switch with mute → no Unknown run (sole unmuted fallback)
- [ ] Verify multi-speaker scenario: 2+ unmuted → Unknown preserved (no false attribution)
- [ ] Verify retroactive correction works end-to-end
- [ ] Verify orphan meetings (no extension) behavior unchanged
- [ ] Verify mic/local speaker ("Me"/configured name) unaffected
- [ ] Run `python -m compileall recog frontend ktalk_bridge.py`
- [ ] Run `python mp4_notes.py --help` — CLI smoke test

### Task 5: [Final] Update documentation
- [ ] Update CLAUDE.md if new patterns discovered
- [ ] Add inline code comments explaining the fallback chain

*Note: ralphex automatically moves completed plans to `docs/plans/completed/`*

## Technical Details

### Speaker assignment fallback chain (after fix)
```
1. ktalk_speaker = bridge.get_dominant_speaker_for_period(start, end)
2. if not: ktalk_speaker = bridge.get_current_speaker()
3. if ktalk_speaker → use it
4. elif last_speaker valid (TTL < 10s, not muted) → use it
5. elif bridge.get_sole_unmuted_remote_participant() → use it (uncertain=True)  ← NEW
6. else → "Unknown"
```

### Retroactive correction flow
```
When speaker identified after Unknown run:
  scan backwards through segments where speaker == "Unknown" and is_local == False
  stop at: named speaker, is_local=True, >20 segments, >60s gap
  re-attribute all found Unknown segments to current speaker
  update JSONL, transcript_lines, notify extension
```

### `get_sole_unmuted_remote_participant()` logic
```python
def get_sole_unmuted_remote_participant(self) -> Optional[str]:
    with self._lock:
        unmuted_remote = [
            p for p in self._participants.values()
            if not p.is_local and not p.mic_muted
        ]
        if len(unmuted_remote) == 1:
            return unmuted_remote[0].name
        return None
```

## Post-Completion

**Manual verification**:
- Join a KTalk meeting with 2-3 people
- Have speakers take turns with mute/unmute between turns
- Verify Unknown segments are minimal or retroactively corrected
- Check meeting JSONL for correct attribution after retroactive fix
