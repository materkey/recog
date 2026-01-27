"""Tests for recog/alignment.py."""
import pytest
from pathlib import Path

import sys
sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.alignment import align_segments_with_speakers
from recog.models import Segment, DiarizationSegment


class TestAlignSegmentsWithSpeakers:
    """Tests for align_segments_with_speakers function."""

    def test_segment_fully_in_first_speaker_interval(self):
        """
        given: ASR segment [0.0, 1.0] and diarization [0.0, 2.0] SPEAKER_00
        when: aligning
        then: segment should be assigned to SPEAKER_00
        """
        asr = [Segment(text="Hello", start=0.0, end=1.0)]
        diar = [DiarizationSegment(start=0.0, end=2.0, speaker_id="SPEAKER_00")]

        result = align_segments_with_speakers(asr, diar)

        assert len(result) == 1
        assert result[0].speaker_id == "SPEAKER_00"

    def test_segment_spanning_two_speakers_uses_max_overlap(self):
        """
        given: ASR segment [1.5, 2.5] spanning SPEAKER_00 [0, 2] and SPEAKER_01 [2, 4]
        when: aligning
        then: segment should be assigned to speaker with more overlap
        """
        asr = [Segment(text="Test", start=1.5, end=2.5)]
        diar = [
            DiarizationSegment(start=0.0, end=2.0, speaker_id="SPEAKER_00"),
            DiarizationSegment(start=2.0, end=4.0, speaker_id="SPEAKER_01"),
        ]

        result = align_segments_with_speakers(asr, diar)

        assert len(result) == 1
        # Overlap with SPEAKER_00: 0.5s, with SPEAKER_01: 0.5s
        # Equal overlap - should pick first one found with max
        assert result[0].speaker_id in ["SPEAKER_00", "SPEAKER_01"]

    def test_multiple_segments_assigned_correctly(self, asr_segments, diarization_segments):
        """
        given: 3 ASR segments and 2 diarization segments
        when: aligning
        then: each segment should have appropriate speaker assigned
        """
        result = align_segments_with_speakers(asr_segments, diarization_segments)

        assert len(result) == 3
        # Segment [0, 1] -> SPEAKER_00 (fully in [0, 2])
        assert result[0].speaker_id == "SPEAKER_00"
        # Segment [1.5, 2.5] -> overlaps both, picks max
        assert result[1].speaker_id in ["SPEAKER_00", "SPEAKER_01"]
        # Segment [3, 4] -> SPEAKER_01 (fully in [2, 5])
        assert result[2].speaker_id == "SPEAKER_01"

    def test_preserves_original_text_and_times(self, asr_segments, diarization_segments):
        """
        given: ASR segments with specific text and timestamps
        when: aligning
        then: text and timestamps should be preserved
        """
        result = align_segments_with_speakers(asr_segments, diarization_segments)

        assert result[0].text == "Привет"
        assert result[0].start == 0.0
        assert result[0].end == 1.0

    def test_empty_asr_returns_empty_list(self, diarization_segments):
        """
        given: empty ASR segments
        when: aligning
        then: should return empty list
        """
        result = align_segments_with_speakers([], diarization_segments)

        assert result == []

    def test_no_diarization_uses_default_speaker(self):
        """
        given: ASR segments but no diarization
        when: aligning
        then: should use default speaker_0
        """
        asr = [Segment(text="Hello", start=0.0, end=1.0)]

        result = align_segments_with_speakers(asr, [])

        assert len(result) == 1
        assert result[0].speaker_id == "speaker_0"
