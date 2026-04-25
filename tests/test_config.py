"""
Unit tests for recog/config.py — module-level constants.
"""
from recog import config


class TestPcmRefineConstants:
    """The refine constants have the documented defaults."""

    def test_window_default_is_20s(self):
        assert config.PCM_REFINE_WINDOW_S == 20.0

    def test_monolog_cap_default_is_15s(self):
        assert config.PCM_REFINE_MONOLOG_CAP_S == 15.0
