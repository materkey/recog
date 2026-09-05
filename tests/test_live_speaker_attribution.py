"""Tests for speaker attribution in live transcription."""
import pytest
import time
import threading
from pathlib import Path
import sys
from collections import deque

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from unittest.mock import Mock
from recog.ktalk_bridge import KTalkBridge, Participant
from recog.live import LiveTranscriber, SourceState, RETROACTIVE_MAX_SEGMENTS


def test_system_audio_uses_is_local_false():
    """
    Verify the fix: system audio processing should use is_local=False hardcoded.

    This test reads the source code and verifies:
    1. The buggy pattern is NOT present
    2. The fix is in place
    """
    live_py_path = Path(__file__).resolve().parent.parent / 'recog' / 'live.py'
    source = live_py_path.read_text()

    # The bug was: is_local = self._ktalk_bridge.is_current_speaker_local()
    # The fix is: is_local = False
    buggy_pattern = "is_local = self._ktalk_bridge.is_current_speaker_local()"

    assert buggy_pattern not in source, \
        f"Bug still present: {buggy_pattern}"


def test_no_uncertain_local_reason_for_system_audio():
    """
    Verify: system audio should not have 'uncertain_reason: local' logic.

    The old code marked system audio as uncertain when is_local was True.
    Since system audio is always remote, this logic is no longer needed.
    """
    live_py_path = Path(__file__).resolve().parent.parent / 'recog' / 'live.py'
    source = live_py_path.read_text()

    # The old buggy pattern that marked system audio as uncertain
    buggy_pattern = 'uncertain_reason = "local"'

    assert buggy_pattern not in source, \
        f"Old uncertain logic still present: {buggy_pattern}"


def test_live_startup_loads_asr_before_pcm_ingress():
    """
    Verify that PCM worker and WS bridge cannot accept chunks before the
    explicit ASR model load in LiveTranscriber.run().
    """
    live_py_path = Path(__file__).resolve().parent.parent / 'recog' / 'live.py'
    source = live_py_path.read_text()
    run_start = source.index("    def run(")
    run_end = source.index("\n    def ", run_start + 1)
    run_body = source[run_start:run_end]

    load_pos = run_body.index("self.asr.load_model(quiet=True)")
    ingest_pos = run_body.index("self._setup_per_speaker_ingest()")
    bridge_pos = run_body.index("self._start_ktalk_bridge()")

    assert load_pos < ingest_pos
    assert load_pos < bridge_pos


# --- Sole unmuted fallback wiring tests ---


def test_sole_unmuted_fallback_in_speaker_assignment_chain():
    """
    given: KTalk mode, dominant/current speaker both None, TTL expired, one unmuted remote
    when: _handle_ktalk_chunk runs
    then: sole unmuted participant is attributed with uncertain_reason="sole_unmuted_fallback"
    """
    transcriber = Mock()
    transcriber._ktalk_bridge = Mock()
    transcriber._ktalk_bridge.get_dominant_speaker_for_period.return_value = None
    transcriber._ktalk_bridge.get_current_speaker.return_value = None
    transcriber._ktalk_bridge.get_sole_unmuted_remote_participant.return_value = "Bob"
    transcriber._ktalk_bridge.are_all_mics_muted.return_value = False
    transcriber._meeting_storage = None
    transcriber._state_lock = threading.Lock()
    transcriber._meeting_generation = 0
    transcriber.transcript_lines = []
    transcriber._recent_system_segments = deque(maxlen=RETROACTIVE_MAX_SEGMENTS)
    transcriber._ktalk_current_speaker = None

    state = SourceState()
    state.last_speaker = None  # No last speaker -> TTL path skipped
    now = time.time()

    LiveTranscriber._handle_ktalk_chunk(
        transcriber, "hello", "12:00:00", now - 3, now, now, 0, state)

    # Speaker attributed to sole unmuted
    assert "**Bob**" in transcriber.transcript_lines[0]

    # send_transcript called with sole_unmuted_fallback uncertainty
    call_kwargs = transcriber._ktalk_bridge.send_transcript.call_args
    assert call_kwargs[1]['uncertain'] is True
    assert call_kwargs[1]['uncertain_reason'] == "sole_unmuted_fallback"

    # Dominant and current were checked first (chain order preserved)
    transcriber._ktalk_bridge.get_dominant_speaker_for_period.assert_called_once()
    transcriber._ktalk_bridge.get_current_speaker.assert_called_once()
    transcriber._ktalk_bridge.get_sole_unmuted_remote_participant.assert_called_once()


def test_sole_unmuted_fallback_used_when_all_other_signals_fail():
    """
    given: KTalkBridge where dominant speaker and current speaker return None,
           last_speaker TTL expired, but exactly 1 remote participant unmuted
    when: speaker assignment logic runs
    then: sole unmuted participant is returned as speaker
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=True),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=True),
    }

    # Simulate the fallback chain as in live.py:
    # 1. ktalk_speaker = None (dominant/current both failed)
    ktalk_speaker = None
    # 2. last_speaker TTL expired (simulated by not entering that branch)
    last_speaker = None
    # 3. sole unmuted fallback
    sole_unmuted = bridge.get_sole_unmuted_remote_participant()

    # Chain result:
    if ktalk_speaker:
        speaker = ktalk_speaker
    elif last_speaker:
        speaker = last_speaker
    elif sole_unmuted:
        speaker = sole_unmuted
    else:
        speaker = "Unknown"

    assert speaker == "Bob"
    assert sole_unmuted == "Bob"


def test_sole_unmuted_fallback_not_used_when_multiple_unmuted():
    """
    given: KTalkBridge with 2+ remote participants unmuted,
           dominant speaker and current speaker return None
    when: speaker assignment logic runs
    then: speaker is "Unknown" (no false attribution)
    """
    bridge = KTalkBridge()
    bridge._participants = {
        "p1": Participant(id="p1", name="Alice", is_local=False, mic_muted=False),
        "p2": Participant(id="p2", name="Bob", is_local=False, mic_muted=False),
        "p3": Participant(id="p3", name="Carol", is_local=False, mic_muted=True),
    }

    # Simulate the fallback chain:
    ktalk_speaker = None
    last_speaker = None
    sole_unmuted = bridge.get_sole_unmuted_remote_participant()

    if ktalk_speaker:
        speaker = ktalk_speaker
    elif last_speaker:
        speaker = last_speaker
    elif sole_unmuted:
        speaker = sole_unmuted
    else:
        speaker = "Unknown"

    assert speaker == "Unknown"
    assert sole_unmuted is None


# --- Per-speaker PCM transport wiring tests (plan §4.4) ---


def _fresh_transcriber_mock():
    """Shared baseline for Task 4 wiring tests: bare-minimum Mock with the
    attributes the instance methods under test read from `self`."""
    t = Mock(spec=LiveTranscriber)
    t._ktalk_bridge = Mock()
    t._meeting_storage = None
    t._state_lock = threading.Lock()
    t._wav_lock = threading.Lock()
    t._wav_writer_system = None
    t._meeting_generation = 0
    t.transcript_lines = []
    t._recent_system_segments = deque(maxlen=RETROACTIVE_MAX_SEGMENTS)
    t._ktalk_current_speaker = None
    t._per_speaker_ingest = None
    t._pcm_prev_words = {}
    t._pcm_pid_state = {}
    t._pcm_live_segments = {}
    t._pcm_segment_texts = {}
    t._last_pcm_degraded = False
    t._refine_worker = None
    t._system_wav_anchor = None
    t.sample_rate = 16000
    t.use_pcm_transport = False
    t.use_ktalk = True
    t.asr = Mock()
    return t


def test_pcm_off_mixed_path_queries_dominant_speaker():
    """
    given: PCM transport off, KTalk mode, system audio chunk in flight
    when:  _handle_ktalk_chunk runs
    then:  get_dominant_speaker_for_period IS called (old attribution path)
    """
    t = _fresh_transcriber_mock()
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = "Alice"
    t._ktalk_bridge.are_all_mics_muted.return_value = False
    state = SourceState()
    now = time.time()

    LiveTranscriber._handle_ktalk_chunk(
        t, "hi", "12:00:00", now - 3, now, now, 0, state)

    t._ktalk_bridge.get_dominant_speaker_for_period.assert_called_once()
    # Confident attribution → uncertain_reason is empty in the persisted
    # segment (no fallback_mixed prefix when PCM is off).
    call_kwargs = t._ktalk_bridge.send_transcript.call_args
    assert call_kwargs[1]["uncertain"] is False


def test_pcm_on_batch_callback_does_not_query_bridge_speakers():
    """
    given: PCM transport active; batch arrives from PerSpeakerIngest
    when:  _on_pcm_batch runs
    then:  the speaker_name in the batch is used verbatim — neither
           get_current_speaker nor get_dominant_speaker_for_period is called
    """
    t = _fresh_transcriber_mock()
    t.asr.transcribe_batch.return_value = ["hello world"]
    import numpy as np

    batch = [(
        "pid-1",           # pid
        "Alice",           # speaker_name
        False,             # is_local
        np.zeros(48000, dtype=np.int16),  # pcm
        1000.0,            # chunk_start_ms
        4300.0,            # chunk_end_ms
        0.0,               # pid_audio_ms_start
        3000.0,            # pid_audio_ms_end
    )]

    LiveTranscriber._on_pcm_batch(t, batch)

    t._ktalk_bridge.get_dominant_speaker_for_period.assert_not_called()
    t._ktalk_bridge.get_current_speaker.assert_not_called()
    # Speaker label taken directly from batch tuple.
    call_kwargs = t._ktalk_bridge.send_transcript.call_args
    assert call_kwargs[0][0] == "Alice"
    assert call_kwargs[0][1] == "hello world"
    # No uncertainty for PCM-native entries.
    assert call_kwargs[1]["uncertain"] is False


def test_pcm_on_batch_rejects_batch_when_meeting_switched_post_drain():
    """
    given: PerSpeakerIngest snapshotted gen=5 at drain; meeting_generation
           bumps to 6 before the callback runs (simulates
           _invalidate_pending_chunks firing between drain and callback)
    when:  _on_pcm_batch dispatches the batch
    then:  no transcript is emitted or persisted — the drain-time gen
           (from PerSpeakerIngest.last_drain_generation) is used as the
           authoritative reference so the stale batch is discarded rather
           than attributed to the new meeting
    """
    t = _fresh_transcriber_mock()
    t.asr.transcribe_batch.return_value = ["hello world"]
    t._meeting_generation = 6  # bumped after drain
    t._per_speaker_ingest = Mock()
    t._per_speaker_ingest.last_drain_generation = 5  # drain-time snapshot
    import numpy as np

    batch = [(
        "pid-1", "Alice", False, np.zeros(48000, dtype=np.int16),
        0.0, 3300.0, 0.0, 3000.0,
    )]

    LiveTranscriber._on_pcm_batch(t, batch)

    # Batch dropped — no extension send, no transcript line appended.
    t._ktalk_bridge.send_transcript.assert_not_called()
    assert t.transcript_lines == []


def test_pcm_on_batch_sanitizes_newlines_in_speaker_name():
    """
    given: PCM batch carries a speaker_name with embedded newline/CR chars
    when:  _on_pcm_batch dispatches the batch
    then:  send_transcript receives the label stripped of \\n/\\r so the
           markdown transcript line stays single-line and anchor/AI parsers
           cannot be confused by extension-supplied names
    """
    t = _fresh_transcriber_mock()
    t.asr.transcribe_batch.return_value = ["hi"]
    import numpy as np

    batch = [(
        "pid-xyz",
        "Evil\nName\r**injected**",
        False,
        np.zeros(48000, dtype=np.int16),
        0.0,
        3300.0,
        0.0,
        3000.0,
    )]

    LiveTranscriber._on_pcm_batch(t, batch)

    call_kwargs = t._ktalk_bridge.send_transcript.call_args
    label = call_kwargs[0][0]
    assert "\n" not in label
    assert "\r" not in label
    # Non-newline bytes (including leftover ** from the injection attempt)
    # are kept; only CR/LF are stripped — keeps the fix minimal.
    assert "Evil" in label and "Name" in label


def test_pcm_on_batch_falls_back_to_wav_writer_without_anchor():
    """
    given: no wall↔WAV anchor yet (before the first system write), PCM batch
           with chunk_start_ms ≈ 2s ago and wav writer at offset 10000 ms
    when:  _on_pcm_batch persists the segment
    then:  falls back to `wav_offset_now - chunk_age` ≈ 10000 - 2000 = 8000
           (points into audio_system.wav where the chunk's audio lives),
           not 0 — segment playback / "enroll from segments" relies on it.
    """
    import numpy as np

    t = _fresh_transcriber_mock()
    t._system_wav_anchor = None
    t.asr.transcribe_batch.return_value = ["hello"]
    captured_storage = Mock()
    captured_storage.append_segment.return_value = 1
    t._meeting_storage = captured_storage

    wav_writer = Mock()
    wav_writer.get_offset_ms.return_value = 10_000
    t._wav_writer_system = wav_writer

    # chunk_start_ms is a wall-clock ms value (extension's clock_offset
    # already applied).  Stage it 2s in the past.
    now_ms = time.time() * 1000
    batch = [(
        "pid-1", "Alice", False,
        np.zeros(48000, dtype=np.int16),
        now_ms - 2000, now_ms + 1300,
        0.0, 3300.0,
    )]

    LiveTranscriber._on_pcm_batch(t, batch)

    append_kwargs = captured_storage.append_segment.call_args.kwargs
    # Tolerate a few ms of drift between time.time() in the test and
    # inside _on_pcm_batch (test ran on real wall clock).
    assert abs(append_kwargs["audio_offset_ms"] - 8000) < 50


def test_pcm_on_batch_audio_offset_clamped_to_zero_when_no_wav_writer():
    """
    given: PCM batch arrives but no system wav writer exists (e.g.
           save_audio disabled or pre-meeting path)
    when:  _on_pcm_batch persists the segment
    then:  audio_offset_ms falls back to 0 rather than raising — the
           segment is recorded even if playback isn't possible later.
    """
    import numpy as np

    t = _fresh_transcriber_mock()
    t.asr.transcribe_batch.return_value = ["hi"]
    captured_storage = Mock()
    captured_storage.append_segment.return_value = 1
    t._meeting_storage = captured_storage
    t._wav_writer_system = None  # no recording wired

    batch = [(
        "pid-1", "Alice", False,
        np.zeros(48000, dtype=np.int16),
        time.time() * 1000 - 500, time.time() * 1000 + 2800,
        0.0, 3300.0,
    )]

    LiveTranscriber._on_pcm_batch(t, batch)

    append_kwargs = captured_storage.append_segment.call_args.kwargs
    assert append_kwargs["audio_offset_ms"] == 0


def _anchor_batch(chunk_start_ms, end_ms=None):
    import numpy as np
    return (
        "pid-1", "Alice", False,
        np.zeros(48000, dtype=np.int16),
        chunk_start_ms, (end_ms if end_ms is not None else chunk_start_ms + 3300),
        0.0, 3300.0,
    )


def test_pcm_on_batch_offset_from_anchor_is_exact():
    """
    given: fresh anchor (wall T0, WAV offset 7000) and a chunk captured
           1500 ms after the anchored sample
    when:  _on_pcm_batch persists the segment
    then:  audio_offset_ms == 7000 + 1500 == 8500 exactly — the offset is
           extrapolated in one clock domain, no wall-clock drift.
    """
    t = _fresh_transcriber_mock()
    t.asr.transcribe_batch.return_value = ["hello"]
    captured_storage = Mock()
    captured_storage.append_segment.return_value = 1
    t._meeting_storage = captured_storage

    now_ms = time.time() * 1000
    # Anchor recent enough to be fresh; chunk 1500 ms past the anchor sample.
    t._system_wav_anchor = (now_ms - 3000, 7000)
    batch = [_anchor_batch(now_ms - 1500)]

    LiveTranscriber._on_pcm_batch(t, batch)

    append_kwargs = captured_storage.append_segment.call_args.kwargs
    assert append_kwargs["audio_offset_ms"] == 8500


def test_pcm_on_batch_consecutive_chunks_offsets_track_chunk_starts():
    """
    given: two chunks in one batch whose chunk_start_ms differ by 1000 ms
    when:  _on_pcm_batch persists both segments
    then:  their offsets differ by exactly 1000 — regression for the
           sawtooth bug where consecutive PCM segments collapsed onto the
           same offset (seg id=9 played seg id=6's audio).
    """
    t = _fresh_transcriber_mock()
    t.asr.transcribe_batch.return_value = ["first", "second"]
    captured_storage = Mock()
    captured_storage.append_segment.return_value = 1
    t._meeting_storage = captured_storage
    # Distinct pids so dedup can't merge them.
    now_ms = time.time() * 1000
    t._system_wav_anchor = (now_ms - 5000, 0)
    batch = [
        ("pid-1", "Alice", False, __import__("numpy").zeros(48000, dtype="int16"),
         now_ms - 2000, now_ms + 1300, 0.0, 3300.0),
        ("pid-2", "Bob", False, __import__("numpy").zeros(48000, dtype="int16"),
         now_ms - 1000, now_ms + 2300, 0.0, 3300.0),
    ]

    LiveTranscriber._on_pcm_batch(t, batch)

    offsets = [c.kwargs["audio_offset_ms"]
               for c in captured_storage.append_segment.call_args_list]
    assert len(offsets) == 2
    assert offsets[1] - offsets[0] == 1000


def test_pcm_on_batch_falls_back_when_anchor_stale():
    """
    given: an anchor older than PCM_WAV_ANCHOR_MAX_AGE_MS (degraded-mode gap
           broke the WAV timeline ahead of it) and wav writer at 10000 ms
    when:  _on_pcm_batch persists the segment
    then:  the stale anchor is ignored — offset comes from the fallback
           `wav_offset_now - chunk_age` (≈ 8000), not the anchor.
    """
    t = _fresh_transcriber_mock()
    t.asr.transcribe_batch.return_value = ["hello"]
    captured_storage = Mock()
    captured_storage.append_segment.return_value = 1
    t._meeting_storage = captured_storage
    wav_writer = Mock()
    wav_writer.get_offset_ms.return_value = 10_000
    t._wav_writer_system = wav_writer

    now_ms = time.time() * 1000
    # Anchor 60s old → stale; would extrapolate to a wildly wrong offset.
    t._system_wav_anchor = (now_ms - 60_000, 5000)
    batch = [_anchor_batch(now_ms - 2000)]

    LiveTranscriber._on_pcm_batch(t, batch)

    append_kwargs = captured_storage.append_segment.call_args.kwargs
    assert abs(append_kwargs["audio_offset_ms"] - 8000) < 50


def test_pcm_on_batch_anchor_offset_clamped_to_zero():
    """
    given: fresh anchor but chunk_start_ms far earlier than the anchor's
           wall time (extrapolation would be negative)
    when:  _on_pcm_batch persists the segment
    then:  offset is clamped to 0 rather than going negative.
    """
    t = _fresh_transcriber_mock()
    t.asr.transcribe_batch.return_value = ["hello"]
    captured_storage = Mock()
    captured_storage.append_segment.return_value = 1
    t._meeting_storage = captured_storage

    now_ms = time.time() * 1000
    t._system_wav_anchor = (now_ms - 1000, 500)
    # chunk captured 5s before the anchor sample → 500 + (-4000) < 0
    batch = [_anchor_batch(now_ms - 5000)]

    LiveTranscriber._on_pcm_batch(t, batch)

    append_kwargs = captured_storage.append_segment.call_args.kwargs
    assert append_kwargs["audio_offset_ms"] == 0


def test_pcm_healthy_skips_system_audio_ktalk_dispatch(monkeypatch):
    """
    given: PCM transport active + ingest healthy (degraded=False)
    when:  _process_chunk runs for the system-audio source
    then:  early-return short-circuits ASR — transcribe_direct is NOT
           called and _handle_ktalk_chunk is NOT invoked
    """
    import numpy as np
    t = _fresh_transcriber_mock()
    t.use_pcm_transport = True
    t._per_speaker_ingest = Mock()
    t._per_speaker_ingest.degraded = False
    t._per_speaker_ingest.pcm_ever_seen = True
    t._wav_lock = threading.Lock()
    t._wav_writer_system = None
    t._wav_writer_mic = None
    t.chunk_duration = 3.0

    capture = Mock()
    capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)

    # Guard: these must not be called in healthy-PCM mode.
    t._transcribe_direct = Mock()
    t._handle_ktalk_chunk = Mock()
    t._emit_pcm_transition_marker = Mock()

    LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=False)

    t._transcribe_direct.assert_not_called()
    t._handle_ktalk_chunk.assert_not_called()
    # No transition marker because degraded state didn't flip.
    t._emit_pcm_transition_marker.assert_not_called()


def test_pcm_pre_first_chunk_falls_through_to_mixed_path():
    """
    given: PCM transport active but no chunk has arrived yet
           (pcm_ever_seen=False — pre-join window or broken extension)
    when:  _process_chunk runs for the system-audio source
    then:  the PCM-primary gate is skipped entirely, mixed-path ASR runs,
           and no transition marker is emitted.  Without this the session
           start swallows up to PCM_SILENCE_WATCHDOG_S of system audio
           before the watchdog flips the ingest into degraded mode.
    """
    import numpy as np
    t = _fresh_transcriber_mock()
    t.use_pcm_transport = True
    t._per_speaker_ingest = Mock()
    t._per_speaker_ingest.degraded = False
    t._per_speaker_ingest.pcm_ever_seen = False
    t._wav_lock = threading.Lock()
    t._wav_writer_system = None
    t._wav_writer_mic = None
    t.chunk_duration = 3.0
    t._ktalk_bridge.is_local_mic_muted.return_value = False

    capture = Mock()
    capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)

    t._transcribe_direct = lambda _c: "hi"
    t._check_silence_split = Mock()
    t._handle_ktalk_chunk = Mock()
    t._emit_pcm_transition_marker = Mock()

    LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=False)

    # Mixed-path dispatched — not PCM-gated out.
    t._handle_ktalk_chunk.assert_called_once()
    # No spurious marker at session start / before first PCM.
    t._emit_pcm_transition_marker.assert_not_called()


def test_pcm_degraded_dispatches_mixed_path_with_fallback_flag():
    """
    given: PCM ingest in degraded mode (watchdog tripped)
    when:  system-audio chunk arrives via _process_chunk
    then:  _handle_ktalk_chunk is invoked with fallback_mixed=True
           (so the mixed-path attribution runs and tags the segment)
    """
    import numpy as np
    t = _fresh_transcriber_mock()
    t.use_pcm_transport = True
    t._per_speaker_ingest = Mock()
    t._per_speaker_ingest.degraded = True
    t._per_speaker_ingest.pcm_ever_seen = True
    t._last_pcm_degraded = True  # No transition this call
    t._wav_lock = threading.Lock()
    t._wav_writer_system = None
    t._wav_writer_mic = None
    t.chunk_duration = 3.0
    t._ktalk_bridge.is_local_mic_muted.return_value = False

    capture = Mock()
    capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)

    t._transcribe_direct = lambda _c: "hello"
    t._check_silence_split = Mock()
    t._handle_ktalk_chunk = Mock()
    t._handle_mic_chunk = Mock()
    t._emit_pcm_transition_marker = Mock()

    LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=False)

    t._handle_ktalk_chunk.assert_called_once()
    call_kwargs = t._handle_ktalk_chunk.call_args.kwargs
    assert call_kwargs.get("fallback_mixed") is True
    t._emit_pcm_transition_marker.assert_not_called()


def test_handle_ktalk_chunk_fallback_mixed_tags_persisted_reason():
    """
    given: confident KTalk attribution in fallback-mixed mode
    when:  _handle_ktalk_chunk runs
    then:  send_transcript gets uncertain=False, uncertain_reason=""
           BUT the persisted meeting_storage segment's uncertain_reason is
           'fallback_mixed' (diagnostic for offline analysis)
    """
    t = _fresh_transcriber_mock()
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = "Alice"
    t._ktalk_bridge.are_all_mics_muted.return_value = False
    captured_storage = Mock()
    captured_storage.append_segment.return_value = 42
    t._meeting_storage = captured_storage

    state = SourceState()
    now = time.time()

    LiveTranscriber._handle_ktalk_chunk(
        t, "hi", "12:00:00", now - 3, now, now, 0, state,
        fallback_mixed=True)

    # Extension still sees a confident attribution (no uncertainty UI).
    send_kwargs = t._ktalk_bridge.send_transcript.call_args.kwargs
    assert send_kwargs["uncertain"] is False
    assert send_kwargs["uncertain_reason"] == ""
    # JSONL has the diagnostic marker for the degraded window.
    append_kwargs = captured_storage.append_segment.call_args.kwargs
    assert append_kwargs["uncertain_reason"] == "fallback_mixed"


def test_handle_ktalk_chunk_fallback_mixed_composes_with_existing_reason():
    """
    given: uncertain attribution (no_speaker) and fallback_mixed=True
    when:  _handle_ktalk_chunk runs
    then:  persisted uncertain_reason becomes 'fallback_mixed/no_speaker';
           extension sees the original reason unchanged
    """
    t = _fresh_transcriber_mock()
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = None
    t._ktalk_bridge.get_current_speaker.return_value = None
    t._ktalk_bridge.get_sole_unmuted_remote_participant.return_value = None
    t._ktalk_bridge.are_all_mics_muted.return_value = False
    captured_storage = Mock()
    captured_storage.append_segment.return_value = 1
    t._meeting_storage = captured_storage

    state = SourceState()
    state.last_speaker = None
    now = time.time()

    LiveTranscriber._handle_ktalk_chunk(
        t, "hello", "12:00:00", now - 3, now, now, 0, state,
        fallback_mixed=True)

    send_kwargs = t._ktalk_bridge.send_transcript.call_args.kwargs
    assert send_kwargs["uncertain"] is True
    assert send_kwargs["uncertain_reason"] == "no_speaker"
    append_kwargs = captured_storage.append_segment.call_args.kwargs
    assert append_kwargs["uncertain_reason"] == "fallback_mixed/no_speaker"


def test_pcm_transition_marker_skips_append_on_stale_generation():
    """
    given: _emit_pcm_transition_marker is called with marker_gen captured
           before a meeting switch bumped self._meeting_generation
    when:  the method runs after the switch
    then:  the stdout/list write still happens (session-level ledger) but
           meeting_storage.append_segment is NOT called — otherwise a
           watchdog marker from the old meeting would land in the new
           meeting's transcript.jsonl (see code-review-pass-2 Bug).
    """
    t = _fresh_transcriber_mock()
    storage = Mock()
    t._meeting_storage = storage
    t._meeting_generation = 10  # current
    t.transcript_lines = []

    # Caller captured gen=9 before switch.
    LiveTranscriber._emit_pcm_transition_marker(t, True, 9)

    assert t.transcript_lines, "in-memory ledger should still get the marker"
    storage.append_segment.assert_not_called()


def test_pcm_transition_marker_skips_pause_on_stale_generation():
    """
    given: _emit_pcm_transition_marker called with stale marker_gen after
           _invalidate_pending_chunks bumped the generation and reset
           _last_pcm_degraded=False (AND PerSpeakerIngest.drain reset
           ingest._degraded=False)
    when:  the stale transition tries to pause the refine worker
    then:  pause/resume is NOT invoked — if it were, the new meeting would
           never observe a healthy→degraded→healthy cycle (both flip flags
           start at False) and refine would stay paused indefinitely.
    """
    t = _fresh_transcriber_mock()
    worker = Mock()
    t._refine_worker = worker
    t._meeting_generation = 10  # current, after meeting switch
    t.transcript_lines = []

    # Stale call captured before the switch at gen=9.
    LiveTranscriber._emit_pcm_transition_marker(t, True, 9)

    worker.pause.assert_not_called()
    worker.resume.assert_not_called()


def test_pcm_transition_marker_pauses_on_matching_generation():
    """
    given: marker_gen matches current meeting_generation, degraded=True
    when:  _emit_pcm_transition_marker runs
    then:  refine worker is paused
    """
    t = _fresh_transcriber_mock()
    worker = Mock()
    t._refine_worker = worker
    t._meeting_generation = 4
    t.transcript_lines = []

    LiveTranscriber._emit_pcm_transition_marker(t, True, 4)

    worker.pause.assert_called_once()
    worker.resume.assert_not_called()


def test_pcm_transition_marker_resumes_on_matching_generation():
    """
    given: marker_gen matches current meeting_generation, degraded=False
    when:  _emit_pcm_transition_marker runs
    then:  refine worker is resumed
    """
    t = _fresh_transcriber_mock()
    worker = Mock()
    t._refine_worker = worker
    t._meeting_generation = 4
    t.transcript_lines = []

    LiveTranscriber._emit_pcm_transition_marker(t, False, 4)

    worker.resume.assert_called_once()
    worker.pause.assert_not_called()


def test_pcm_transition_marker_writes_on_matching_generation():
    """
    given: marker_gen matches current meeting_generation
    when:  _emit_pcm_transition_marker runs
    then:  meeting_storage.append_segment is invoked with the Система label
    """
    t = _fresh_transcriber_mock()
    storage = Mock()
    t._meeting_storage = storage
    t._meeting_generation = 4
    t.transcript_lines = []

    LiveTranscriber._emit_pcm_transition_marker(t, True, 4)

    storage.append_segment.assert_called_once()
    call_kwargs = storage.append_segment.call_args.kwargs
    assert call_kwargs["speaker"] == "Система"
    assert call_kwargs["uncertain_reason"] == "pcm_watchdog_transition"


def test_pcm_transition_emits_marker():
    """
    given: PCM state flips from healthy -> degraded between ticks
    when:  _process_chunk runs for the system source
    then:  _emit_pcm_transition_marker is called exactly once with
           degraded=True and the current meeting_generation (so a concurrent
           _invalidate_pending_chunks can be detected before the marker lands
           in the wrong transcript)
    """
    import numpy as np
    t = _fresh_transcriber_mock()
    t.use_pcm_transport = True
    t._per_speaker_ingest = Mock()
    t._per_speaker_ingest.degraded = True
    t._per_speaker_ingest.pcm_ever_seen = True
    t._last_pcm_degraded = False  # Fresh flip
    t._meeting_generation = 3
    t._wav_lock = threading.Lock()
    t._wav_writer_system = None
    t._wav_writer_mic = None
    t.chunk_duration = 3.0

    # Mixed path should proceed (degraded=True) — wire a minimal ASR.
    t._ktalk_bridge.get_dominant_speaker_for_period.return_value = "Alice"
    t._ktalk_bridge.are_all_mics_muted.return_value = False
    t._ktalk_bridge.is_local_mic_muted.return_value = False
    t._transcribe_direct = lambda _c: "hi"
    t._check_silence_split = Mock()
    t._emit_pcm_transition_marker = Mock()

    capture = Mock()
    capture.get_chunk.return_value = (np.ones(48000, dtype=np.float32) * 0.1, 1000.0)

    LiveTranscriber._process_chunk(t, capture, SourceState(), Mock(), is_mic=False)

    t._emit_pcm_transition_marker.assert_called_once_with(True, 3)


def test_invalidate_pending_chunks_drains_per_speaker_ingest():
    """
    given: LiveTranscriber with an attached _per_speaker_ingest
    when:  _invalidate_pending_chunks is invoked (meeting boundary)
    then:  meeting_generation bumps AND ingest.drain() is called AND
           per-pid pcm state buckets are reset
    """
    t = Mock(spec=LiveTranscriber)
    t._state_lock = threading.Lock()
    t._meeting_generation = 7
    t._recent_system_segments = deque()
    t._mic_state = None
    t._sys_state = None
    t._pcm_prev_words = {"pid-1": ["foo", "bar"]}
    t._pcm_pid_state = {"pid-1": {"last_speech_time": 1.0, "segment_start_time": 0.0}}
    t._pcm_live_segments = {"pid-1": [(5, 0.0, 3000.0)]}
    t._pcm_segment_texts = {5: "prior"}
    t._last_pcm_degraded = True

    drained = {"count": 0}
    ingest_mock = Mock()
    ingest_mock.drain.side_effect = lambda: drained.__setitem__("count", drained["count"] + 1)
    t._per_speaker_ingest = ingest_mock
    t.mic_capture = None
    t.system_capture = None
    t._refine_worker = None

    LiveTranscriber._invalidate_pending_chunks(t)

    assert t._meeting_generation == 8
    assert drained["count"] == 1
    # Per-pid state wiped (next chunk starts fresh continuation / dedup).
    assert t._pcm_prev_words == {}
    assert t._pcm_pid_state == {}
    assert t._pcm_live_segments == {}
    assert t._pcm_segment_texts == {}
    assert t._last_pcm_degraded is False


def test_invalidate_pending_chunks_resumes_paused_refine_worker():
    """
    given: refine worker paused in meeting A by a PCM-watchdog degraded trip
           (ingest._degraded=True), meeting ends before PCM recovers
    when:  _invalidate_pending_chunks fires on the meeting-switch edge
    then:  refine worker resume() is called so meeting B starts with refine
           enabled; without this, the healthy→degraded→healthy compare-and-
           set in _emit_pcm_transition_marker never re-fires in meeting B
           (both _last_pcm_degraded and ingest._degraded reset to False by
           _invalidate_pending_chunks) and the worker would stay paused for
           the rest of the process lifetime.
    """
    t = Mock(spec=LiveTranscriber)
    t._state_lock = threading.Lock()
    t._meeting_generation = 3
    t._recent_system_segments = deque()
    t._mic_state = None
    t._sys_state = None
    t._pcm_prev_words = {}
    t._pcm_pid_state = {}
    t._pcm_live_segments = {}
    t._pcm_segment_texts = {}
    t._last_pcm_degraded = True
    t._per_speaker_ingest = Mock()
    t.mic_capture = None
    t.system_capture = None

    refine_worker = Mock()
    t._refine_worker = refine_worker

    LiveTranscriber._invalidate_pending_chunks(t)

    refine_worker.resume.assert_called_once()


def test_invalidate_pending_chunks_safe_when_refine_worker_none():
    """
    given: _refine_worker is None (worker not yet built)
    when:  _invalidate_pending_chunks fires on meeting-switch edge
    then:  no AttributeError; resume is guarded by a None check
    """
    t = Mock(spec=LiveTranscriber)
    t._state_lock = threading.Lock()
    t._meeting_generation = 0
    t._recent_system_segments = deque()
    t._mic_state = None
    t._sys_state = None
    t._pcm_prev_words = {}
    t._pcm_pid_state = {}
    t._pcm_live_segments = {}
    t._pcm_segment_texts = {}
    t._last_pcm_degraded = False
    t._per_speaker_ingest = None
    t.mic_capture = None
    t.system_capture = None
    t._refine_worker = None

    LiveTranscriber._invalidate_pending_chunks(t)

    assert t._meeting_generation == 1


def test_use_pcm_transport_off_without_ktalk():
    """
    given: use_ktalk=False
    when:  LiveTranscriber.__init__ runs
    then:  self.use_pcm_transport is False (no extension to pull PCM from)
    """
    # None device selects default sounddevice input without touching hardware.
    transcriber = LiveTranscriber(
        mic_device=None,
        system_device=None,
        no_diarization=True,
        use_ktalk=False,
    )
    assert transcriber.use_pcm_transport is False


def test_use_pcm_transport_on_with_ktalk():
    """
    given: use_ktalk=True
    when:  LiveTranscriber.__init__ runs
    then:  self.use_pcm_transport is True (per-speaker is the only
           ingest path for KTalk meetings)
    """
    transcriber = LiveTranscriber(
        mic_device=None,
        system_device=None,
        no_diarization=True,
        use_ktalk=True,
    )
    assert transcriber.use_pcm_transport is True


def test_transcribe_direct_delegates_to_asr_transcribe_chunk():
    """
    given: the forward+decode body moved to ASRModule.transcribe_chunk (so the
          remote-offload FallbackASR can route the mic/mixed path too)
    when:  _transcribe_direct runs
    then:  it delegates to self.asr.transcribe_chunk and returns its result

    The decode-tuple unpacking itself is covered at the ASR layer by
    tests/test_asr.py (test_transcribe_direct_on_* via _make_mocked_asr); this
    is the live-layer delegation guard for recog/live.py:457.
    """
    import numpy as np

    asr = Mock()
    asr.transcribe_chunk = Mock(return_value="hello world")

    transcriber = LiveTranscriber.__new__(LiveTranscriber)
    transcriber.asr = asr
    transcriber._refine_worker = None

    audio = np.zeros(16000, dtype=np.float32)
    text = transcriber._transcribe_direct(audio)

    assert text == "hello world"
    assert asr.transcribe_chunk.call_count == 1
    # avoid ndarray == in Mock arg matching: assert identity of the passed clip
    assert asr.transcribe_chunk.call_args[0][0] is audio


def test_transcribe_direct_signals_refine_live_pending_around_transcribe_chunk():
    """
    given: direct live ASR runs while a RefineWorker exists
    when:  _transcribe_direct delegates to asr.transcribe_chunk
    then:  set_live_pending fires BEFORE transcribe_chunk and clear_live_pending
           AFTER it returns

    The shared model lock now lives inside ASRModule.transcribe_chunk, so the
    live-priority signal must bracket the whole delegated call (recog/live.py
    _transcribe_direct set/clear try/finally).
    """
    import numpy as np

    refine_worker = Mock()
    observed: dict = {}

    def fake_transcribe_chunk(audio):
        observed["set_called_before"] = refine_worker.set_live_pending.called
        observed["clear_called_during"] = refine_worker.clear_live_pending.called
        return "prioritized live"

    asr = Mock()
    asr.transcribe_chunk = Mock(side_effect=fake_transcribe_chunk)

    transcriber = LiveTranscriber.__new__(LiveTranscriber)
    transcriber.asr = asr
    transcriber._refine_worker = refine_worker

    text = transcriber._transcribe_direct(np.zeros(16000, dtype=np.float32))

    assert text == "prioritized live"
    refine_worker.set_live_pending.assert_called_once_with()
    refine_worker.clear_live_pending.assert_called_once_with()
    assert observed["set_called_before"] is True, "set_live_pending must fire before transcribe_chunk"
    assert observed["clear_called_during"] is False, "clear must not fire until transcribe_chunk returns"
