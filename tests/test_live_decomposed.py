"""Tests for extracted LiveTranscriber methods (Phase 1 & 2 decomposition)."""
import time
import threading
import sys
from pathlib import Path
from collections import deque
from unittest.mock import Mock, patch

import numpy as np
import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from recog.live import (
    LiveTranscriber,
    SourceState,
    normalize_word,
    deduplicate,
    RETROACTIVE_MAX_SEGMENTS,
)
from recog.config import SILENCE_SPLIT_TIMEOUT, MIC_SPEAKER_NAME, SPEAKER_TURN_GAP, MAX_SEGMENT_DURATION


# ---------------------------------------------------------------------------
# normalize_word
# ---------------------------------------------------------------------------

class TestNormalizeWord:

    def test_strips_punctuation(self):
        assert normalize_word("hello!") == "hello"
        assert normalize_word("world,") == "world"
        assert normalize_word("...test...") == "test"

    def test_lowercases(self):
        assert normalize_word("Hello") == "hello"
        assert normalize_word("WORLD") == "world"

    def test_strips_and_lowercases_combined(self):
        assert normalize_word("Hello!") == "hello"
        assert normalize_word("'World',") == "world"

    def test_empty_string(self):
        assert normalize_word("") == ""

    def test_only_punctuation(self):
        assert normalize_word("...") == ""

    def test_cyrillic(self):
        assert normalize_word("Привет!") == "привет"
        assert normalize_word("МИРА,") == "мира"


# ---------------------------------------------------------------------------
# deduplicate
# ---------------------------------------------------------------------------

class TestDeduplicate:

    def test_removes_leading_overlap(self):
        result = deduplicate("world hello there", ["hello", "world"])
        assert result == "hello there"

    def test_returns_none_for_full_duplicate(self):
        result = deduplicate("hello world", ["hello", "world"])
        assert result is None

    def test_no_overlap_returns_text(self):
        result = deduplicate("completely new text", ["other", "words"])
        assert result == "completely new text"

    def test_empty_prev_words_returns_text(self):
        result = deduplicate("hello world", [])
        assert result == "hello world"

    def test_empty_text_returns_text(self):
        result = deduplicate("", ["some", "words"])
        assert result == ""

    def test_none_prev_words_returns_text(self):
        # prev_words is falsy
        result = deduplicate("hello", [])
        assert result == "hello"

    def test_case_insensitive_overlap(self):
        result = deduplicate("World new stuff", ["hello", "world"])
        assert result == "new stuff"

    def test_punctuation_insensitive_overlap(self):
        result = deduplicate("world, new stuff", ["hello", "world"])
        assert result == "new stuff"

    def test_max_overlap_capped_at_6(self):
        prev = ["a", "b", "c", "d", "e", "f", "g"]
        text = "a b c d e f g extra"
        # Only checks up to 6 words overlap, so "a b c d e f g" won't fully match
        # because max_overlap = min(8, 7, 6) = 6
        result = deduplicate(text, prev)
        # The 6-word overlap check: ["a","b","c","d","e","f"] vs prev[-6:] = ["b","c","d","e","f","g"]
        # That doesn't match. 5-word: ["a","b","c","d","e"] vs ["c","d","e","f","g"] - no.
        # No overlap found -> returns original
        assert result == text


# ---------------------------------------------------------------------------
# _handle_mic_chunk
# ---------------------------------------------------------------------------

class TestHandleMicChunk:

    def _make_transcriber_mock(self):
        transcriber = Mock(spec=LiveTranscriber)
        transcriber.mic_speaker_name = MIC_SPEAKER_NAME
        transcriber._ktalk_bridge = None
        transcriber._meeting_storage = None
        transcriber._meeting_generation = 0
        transcriber.transcript_lines = []
        transcriber._state_lock = threading.Lock()
        return transcriber

    def test_always_uses_mic_speaker_name(self):
        """given: mic chunk - when: _handle_mic_chunk - then: speaker = MIC_SPEAKER_NAME"""
        transcriber = self._make_transcriber_mock()
        state = SourceState()
        now = time.time()

        LiveTranscriber._handle_mic_chunk(
            transcriber, "test text", "12:00:00", now, 0, state)

        assert f"**{MIC_SPEAKER_NAME}**" in transcriber.transcript_lines[0]

    def test_is_local_true_in_meeting_storage(self):
        """given: mic chunk with meeting storage - when: _handle_mic_chunk - then: is_local=True"""
        transcriber = self._make_transcriber_mock()
        transcriber._meeting_storage = Mock()
        state = SourceState()
        now = time.time()

        LiveTranscriber._handle_mic_chunk(
            transcriber, "hello", "12:00:00", now, 0, state)

        call_kwargs = transcriber._meeting_storage.append_segment.call_args[1]
        assert call_kwargs['is_local'] is True
        assert call_kwargs['speaker'] == MIC_SPEAKER_NAME
        assert call_kwargs['speaker_id'] == "local"

    def test_continuation_detection(self):
        """given: same speaker, small gap - when: _handle_mic_chunk - then: is_continuation=True"""
        transcriber = self._make_transcriber_mock()
        transcriber._meeting_storage = Mock()
        state = SourceState()
        now = time.time()
        state.last_speaker = MIC_SPEAKER_NAME
        state.last_speech_time = now - 1  # 1 second ago (< SPEAKER_TURN_GAP)
        state.segment_start_time = now - 5  # 5 seconds ago (< MAX_SEGMENT_DURATION)

        LiveTranscriber._handle_mic_chunk(
            transcriber, "hello", "12:00:00", now, 0, state)

        call_kwargs = transcriber._meeting_storage.append_segment.call_args[1]
        assert call_kwargs['is_continuation'] is True

    def test_new_segment_on_large_gap(self):
        """given: same speaker, gap > SPEAKER_TURN_GAP - when: _handle_mic_chunk - then: is_continuation=False"""
        transcriber = self._make_transcriber_mock()
        transcriber._meeting_storage = Mock()
        state = SourceState()
        now = time.time()
        state.last_speaker = MIC_SPEAKER_NAME
        state.last_speech_time = now - (SPEAKER_TURN_GAP + 1)
        state.segment_start_time = now - (SPEAKER_TURN_GAP + 1)

        LiveTranscriber._handle_mic_chunk(
            transcriber, "hello", "12:00:00", now, 0, state)

        call_kwargs = transcriber._meeting_storage.append_segment.call_args[1]
        assert call_kwargs['is_continuation'] is False

    def test_sends_to_ktalk_bridge_when_present(self):
        """given: ktalk bridge connected - when: _handle_mic_chunk - then: send_transcript called with is_local=True"""
        transcriber = self._make_transcriber_mock()
        transcriber._ktalk_bridge = Mock()
        state = SourceState()
        now = time.time()

        LiveTranscriber._handle_mic_chunk(
            transcriber, "hello", "12:00:00", now, 0, state)

        call_kwargs = transcriber._ktalk_bridge.send_transcript.call_args
        assert call_kwargs[1]['is_local'] is True


# ---------------------------------------------------------------------------
# _check_silence_split
# ---------------------------------------------------------------------------

class TestCheckSilenceSplit:

    def test_splits_on_long_silence(self):
        """given: silence > SILENCE_SPLIT_TIMEOUT - when: _check_silence_split - then: split_on_silence_timeout called"""
        transcriber = Mock(spec=LiveTranscriber)
        transcriber._meeting_storage = Mock()

        state = SourceState()
        state.last_text_time = time.time() - SILENCE_SPLIT_TIMEOUT - 1
        state.last_speaker = "Alice"
        state.last_words = ["some", "words"]
        current_time = time.time()

        LiveTranscriber._check_silence_split(transcriber, state, current_time)

        transcriber._meeting_storage.split_on_silence_timeout.assert_called_once()
        assert state.last_speaker is None
        assert state.last_words == []

    def test_no_split_on_short_silence(self):
        """given: silence < SILENCE_SPLIT_TIMEOUT - when: _check_silence_split - then: no split"""
        transcriber = Mock(spec=LiveTranscriber)
        transcriber._meeting_storage = Mock()

        state = SourceState()
        state.last_text_time = time.time() - 10  # 10 seconds ago
        current_time = time.time()

        LiveTranscriber._check_silence_split(transcriber, state, current_time)

        transcriber._meeting_storage.split_on_silence_timeout.assert_not_called()

    def test_updates_last_text_time(self):
        """given: any silence - when: _check_silence_split - then: last_text_time updated"""
        transcriber = Mock(spec=LiveTranscriber)
        transcriber._meeting_storage = Mock()

        state = SourceState()
        state.last_text_time = time.time() - 5
        current_time = time.time()

        LiveTranscriber._check_silence_split(transcriber, state, current_time)

        assert state.last_text_time == current_time

    def test_no_storage_does_nothing(self):
        """given: no meeting storage - when: _check_silence_split - then: no error"""
        transcriber = Mock(spec=LiveTranscriber)
        transcriber._meeting_storage = None

        state = SourceState()
        state.last_text_time = time.time() - SILENCE_SPLIT_TIMEOUT - 100
        current_time = time.time()

        LiveTranscriber._check_silence_split(transcriber, state, current_time)
        # No exception raised


# ---------------------------------------------------------------------------
# _resolve_speaker_embedding
# ---------------------------------------------------------------------------

class TestResolveSpeakerEmbedding:

    def test_delegates_to_get_embedding_and_identify(self):
        """given: audio chunk - when: _resolve_speaker_embedding - then: calls _get_embedding then _identify_speaker"""
        transcriber = Mock(spec=LiveTranscriber)
        fake_embedding = np.random.randn(192).astype(np.float32)
        transcriber._get_embedding.return_value = fake_embedding
        transcriber._identify_speaker.return_value = "Alice"

        result = LiveTranscriber._resolve_speaker_embedding(
            transcriber, np.zeros(16000, dtype=np.float32))

        transcriber._get_embedding.assert_called_once()
        transcriber._identify_speaker.assert_called_once_with(fake_embedding)
        assert result == "Alice"

    def test_returns_speaker_when_embedding_is_none(self):
        """given: _get_embedding returns None - when: _resolve_speaker_embedding - then: _identify_speaker("speaker")"""
        transcriber = Mock(spec=LiveTranscriber)
        transcriber._get_embedding.return_value = None
        transcriber._identify_speaker.return_value = "speaker"

        result = LiveTranscriber._resolve_speaker_embedding(
            transcriber, np.zeros(16000, dtype=np.float32))

        transcriber._identify_speaker.assert_called_once_with(None)
        assert result == "speaker"
