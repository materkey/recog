"""
Unit tests for recog/config.py — env-gated feature flags.
"""
import pytest

from recog import config


class TestPcmRefineEnabled:
    """Gate: pcm_refine_enabled() requires BOTH RECOG_PCM_TRANSPORT=1 AND RECOG_PCM_REFINE=1."""

    def test_both_flags_set_returns_true(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
        monkeypatch.setenv("RECOG_PCM_REFINE", "1")
        assert config.pcm_refine_enabled() is True

    def test_only_transport_set_returns_false(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
        monkeypatch.delenv("RECOG_PCM_REFINE", raising=False)
        assert config.pcm_refine_enabled() is False

    def test_only_refine_set_returns_false(self, monkeypatch):
        monkeypatch.delenv("RECOG_PCM_TRANSPORT", raising=False)
        monkeypatch.setenv("RECOG_PCM_REFINE", "1")
        assert config.pcm_refine_enabled() is False

    def test_both_unset_returns_false(self, monkeypatch):
        monkeypatch.delenv("RECOG_PCM_TRANSPORT", raising=False)
        monkeypatch.delenv("RECOG_PCM_REFINE", raising=False)
        assert config.pcm_refine_enabled() is False

    def test_transport_zero_refine_one_returns_false(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "0")
        monkeypatch.setenv("RECOG_PCM_REFINE", "1")
        assert config.pcm_refine_enabled() is False

    def test_transport_one_refine_zero_returns_false(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
        monkeypatch.setenv("RECOG_PCM_REFINE", "0")
        assert config.pcm_refine_enabled() is False

    def test_non_one_value_treated_as_off(self, monkeypatch):
        """Strict equality to "1" — "true", "yes", "on" do NOT enable the gate."""
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "true")
        monkeypatch.setenv("RECOG_PCM_REFINE", "true")
        assert config.pcm_refine_enabled() is False


class TestPcmRefineConstants:
    """The refine constants have the documented defaults."""

    def test_window_default_is_20s(self):
        assert config.PCM_REFINE_WINDOW_S == 20.0

    def test_monolog_cap_default_is_15s(self):
        assert config.PCM_REFINE_MONOLOG_CAP_S == 15.0


class TestPcmVadEnabled:
    """Gate: pcm_vad_enabled() requires BOTH RECOG_PCM_TRANSPORT=1 AND RECOG_PCM_VAD=1."""

    def test_both_flags_set_returns_true(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
        monkeypatch.setenv("RECOG_PCM_VAD", "1")
        assert config.pcm_vad_enabled() is True

    def test_only_transport_set_returns_false(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
        monkeypatch.delenv("RECOG_PCM_VAD", raising=False)
        assert config.pcm_vad_enabled() is False

    def test_only_vad_set_returns_false(self, monkeypatch):
        monkeypatch.delenv("RECOG_PCM_TRANSPORT", raising=False)
        monkeypatch.setenv("RECOG_PCM_VAD", "1")
        assert config.pcm_vad_enabled() is False

    def test_both_unset_returns_false(self, monkeypatch):
        monkeypatch.delenv("RECOG_PCM_TRANSPORT", raising=False)
        monkeypatch.delenv("RECOG_PCM_VAD", raising=False)
        assert config.pcm_vad_enabled() is False

    def test_transport_zero_vad_one_returns_false(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "0")
        monkeypatch.setenv("RECOG_PCM_VAD", "1")
        assert config.pcm_vad_enabled() is False

    def test_transport_one_vad_zero_returns_false(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "1")
        monkeypatch.setenv("RECOG_PCM_VAD", "0")
        assert config.pcm_vad_enabled() is False

    def test_non_one_value_treated_as_off(self, monkeypatch):
        monkeypatch.setenv("RECOG_PCM_TRANSPORT", "true")
        monkeypatch.setenv("RECOG_PCM_VAD", "true")
        assert config.pcm_vad_enabled() is False
