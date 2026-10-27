"""
Unit tests for recog/config.py — module-level constants.
"""
from recog import config
from recog.install_config import INSTALL_CONFIG


class TestPcmRefineConstants:
    """The refine constants have the documented defaults."""

    def test_window_default_is_20s(self):
        assert config.PCM_REFINE_WINDOW_S == 20.0

    def test_monolog_cap_default_is_15s(self):
        assert config.PCM_REFINE_MONOLOG_CAP_S == 15.0


class TestAsrFallbackBufferingConfig:
    """Константы из docs/plans/2026-06-02-asr-fallback-buffering-design.md."""

    def test_live_timeout_default_is_5s(self):
        assert type(INSTALL_CONFIG.asr)().live_timeout_s == 5.0

    def test_dedup_max_gap_default_is_2s(self):
        assert type(INSTALL_CONFIG.live)().dedup_max_gap_s == 2.0

    def test_pending_buffer_cap_default_is_600s(self):
        assert type(INSTALL_CONFIG.asr)().pending_buffer_cap_s == 600.0
