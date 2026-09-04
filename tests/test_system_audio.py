"""Тесты recog/system_audio.py.

После переноса модуля в пакет `recog/` путь до Swift-хелпера нельзя считать
от каталога модуля: `sck_audio_capture` и его исходник лежат в корне репо.
`_find_utility()` вызывается прямо из `__init__`, поэтому ошибка здесь роняет
live на macOS ещё до старта захвата.
"""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

from recog import system_audio
from recog.system_audio import REPO_ROOT, ScreenCaptureAudio


def _find_utility():
    """_find_utility не трогает self — зовём его на пустом экземпляре."""
    return ScreenCaptureAudio._find_utility(object.__new__(ScreenCaptureAudio))


def test_repo_root_points_at_the_repository_not_the_package():
    package_dir = Path(system_audio.__file__).resolve().parent
    assert REPO_ROOT == package_dir.parent
    assert (REPO_ROOT / "mp4_notes.py").exists()
    assert not (package_dir / "sck_audio_capture.swift").exists()


def test_existing_binary_in_repo_root_is_found(tmp_path, monkeypatch):
    binary = tmp_path / "sck_audio_capture"
    binary.write_bytes(b"")
    monkeypatch.setattr(system_audio, "REPO_ROOT", tmp_path)

    assert _find_utility() == str(binary)


def test_missing_binary_and_source_raises(tmp_path, monkeypatch):
    monkeypatch.setattr(system_audio, "REPO_ROOT", tmp_path)

    with pytest.raises(FileNotFoundError, match="sck_audio_capture"):
        _find_utility()


def test_stale_binary_is_recompiled_from_repo_root_source(tmp_path, monkeypatch):
    binary = tmp_path / "sck_audio_capture"
    source = tmp_path / "sck_audio_capture.swift"
    binary.write_bytes(b"")
    source.write_text("// newer than the binary")
    import os
    os.utime(binary, (0, 0))

    monkeypatch.setattr(system_audio, "REPO_ROOT", tmp_path)

    argv = {}

    def fake_run(cmd, **kwargs):
        argv["cmd"] = cmd
        binary.write_bytes(b"rebuilt")

        class Result:
            returncode = 0
            stderr = ""

        return Result()

    monkeypatch.setattr(system_audio.subprocess, "run", fake_run)

    assert _find_utility() == str(binary)
    assert argv["cmd"][:4] == ["swiftc", "-o", str(binary), str(source)]
