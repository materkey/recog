"""Tests for recog/speaker_db.py."""
import pytest
import numpy as np
from pathlib import Path

import sys
sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.speaker_db import SpeakerDatabase


class TestSpeakerDatabaseMatch:
    """Tests for match_speaker functionality."""

    def test_match_speaker_exact_embedding_returns_match(
        self, speaker_db_with_data, sample_embedding
    ):
        """
        given: database with one speaker "vlad"
        when: matching with exact same embedding
        then: should return ("vlad", "Vladimir", similarity ~1.0)
        """
        result = speaker_db_with_data.match_speaker(sample_embedding)

        assert result is not None
        speaker_id, display_name, similarity = result
        assert speaker_id == "vlad"
        assert display_name == "Vladimir"
        assert similarity > 0.99

    def test_match_speaker_similar_embedding_returns_match(
        self, speaker_db_with_data, sample_embedding_similar
    ):
        """
        given: database with one speaker "vlad"
        when: matching with similar embedding (above threshold)
        then: should return match with positive similarity
        """
        result = speaker_db_with_data.match_speaker(sample_embedding_similar)

        assert result is not None
        speaker_id, display_name, similarity = result
        assert speaker_id == "vlad"
        # Fixture adds noise but should still be above 0.30 threshold
        assert similarity > 0.30

    def test_match_speaker_different_embedding_returns_none(
        self, speaker_db_with_data, sample_embedding_different
    ):
        """
        given: database with one speaker "vlad"
        when: matching with very different embedding
        then: should return None (below threshold)
        """
        result = speaker_db_with_data.match_speaker(sample_embedding_different)

        assert result is None

    def test_match_speaker_empty_database_returns_none(
        self, speaker_db_path, sample_embedding
    ):
        """
        given: empty speaker database
        when: matching any embedding
        then: should return None
        """
        db = SpeakerDatabase(str(speaker_db_path))
        result = db.match_speaker(sample_embedding)

        assert result is None


class TestSpeakerDatabaseCRUD:
    """Tests for add/delete/rename operations."""

    def test_add_speaker_creates_new_entry(self, speaker_db_path, sample_embedding):
        """
        given: empty speaker database
        when: adding a new speaker
        then: speaker should be retrievable
        """
        db = SpeakerDatabase(str(speaker_db_path))
        db.add_speaker(
            speaker_id="test_id",
            display_name="Test User",
            embedding=sample_embedding
        )

        speaker = db.get_speaker("test_id")
        assert speaker is not None
        assert speaker.display_name == "Test User"
        assert len(speaker.embeddings) == 1

    def test_add_speaker_appends_to_existing(
        self, speaker_db_with_data, sample_embedding_similar
    ):
        """
        given: database with speaker "vlad" having 1 embedding
        when: adding another embedding to "vlad"
        then: "vlad" should have 2 embeddings
        """
        speaker_db_with_data.add_speaker(
            speaker_id="vlad",
            display_name="Vladimir",
            embedding=sample_embedding_similar,
            source_file="second_sample.wav"
        )

        speaker = speaker_db_with_data.get_speaker("vlad")
        assert len(speaker.embeddings) == 2

    def test_delete_speaker_removes_entry(self, speaker_db_with_data):
        """
        given: database with speaker "vlad"
        when: deleting "vlad"
        then: speaker should not be retrievable
        """
        result = speaker_db_with_data.delete_speaker("vlad")

        assert result is True
        assert speaker_db_with_data.get_speaker("vlad") is None

    def test_rename_speaker_updates_display_name(self, speaker_db_with_data):
        """
        given: database with speaker "vlad" named "Vladimir"
        when: renaming to "Vlad K"
        then: display_name should be updated
        """
        result = speaker_db_with_data.rename_speaker("vlad", "Vlad K")

        assert result is True
        speaker = speaker_db_with_data.get_speaker("vlad")
        assert speaker.display_name == "Vlad K"


class TestSpeakerDatabasePersistence:
    """Tests for save/load functionality."""

    def test_save_and_load_preserves_data(
        self, speaker_db_path, sample_embedding
    ):
        """
        given: database with one speaker
        when: save then load in new instance
        then: data should be preserved
        """
        db1 = SpeakerDatabase(str(speaker_db_path))
        db1.add_speaker(
            speaker_id="persist_test",
            display_name="Persistent User",
            embedding=sample_embedding,
            source_file="persist.wav",
            transcript="hello world"
        )
        db1.save()

        db2 = SpeakerDatabase(str(speaker_db_path))

        speaker = db2.get_speaker("persist_test")
        assert speaker is not None
        assert speaker.display_name == "Persistent User"
        assert speaker.embeddings[0].source_file == "persist.wav"
        assert speaker.embeddings[0].transcript == "hello world"
