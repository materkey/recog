"""Данные не должны раздваиваться или оставаться незаметно внутри чекаута."""

import os
import json
from pathlib import Path
import shutil
import subprocess
import sys

import pytest

from recog import config


@pytest.fixture
def roots(tmp_path, monkeypatch):
    repo = tmp_path / "checkout"
    repo.mkdir()
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.delenv("RECOG_DATA_DIR", raising=False)
    return repo, home / ".recog"


def test_fresh_checkout_when_resolving_then_data_is_outside_repo(roots):
    repo, target = roots
    assert config.get_data_dir(repo) == target
    assert not target.exists()


def test_explicit_override_when_resolving_then_home_expands(roots, monkeypatch):
    repo, target = roots
    monkeypatch.setenv("RECOG_DATA_DIR", "~/My data")
    assert config.get_data_dir(repo) == target.parent / "My data"


def test_relative_override_when_resolving_then_rejected(roots, monkeypatch):
    monkeypatch.setenv("RECOG_DATA_DIR", "data")
    with pytest.raises(ValueError, match="absolute"):
        config.get_data_dir(roots[0])


@pytest.mark.parametrize("entry", ["meetings", "logs", "speakers_custom.json", "user-profile.json", "ai-settings.json"])
def test_legacy_data_when_resolving_then_whole_root_stays_with_warning(roots, capsys, entry):
    repo, target = roots
    (repo / entry).touch()
    target.mkdir()  # An empty directory is not an already migrated installation.
    assert config.get_data_dir(repo) == repo
    assert "migrate-data" in capsys.readouterr().err


def test_two_populated_roots_when_resolving_then_conflict_is_visible(roots):
    repo, target = roots
    (repo / "meetings").mkdir()
    target.mkdir()
    (target / "logs").mkdir()
    with pytest.raises(RuntimeError, match="both"):
        config.get_data_dir(repo)


def test_ai_settings_in_both_roots_when_resolving_then_conflict_is_visible(roots):
    repo, target = roots
    (repo / "ai-settings.json").write_text("{}")
    target.mkdir()
    (target / "ai-settings.json").write_text("{}")
    with pytest.raises(RuntimeError, match="both"):
        config.get_data_dir(repo)


def test_leftover_settings_lock_when_resolving_then_not_mistaken_for_legacy_data(roots):
    repo, target = roots
    (repo / "ai-settings.lock").touch()
    assert config.get_data_dir(repo) == target


def test_explicit_storage_when_constructing_then_override_is_preserved(roots):
    from recog.meeting_storage import MeetingStorage

    path = roots[1] / "isolated" / "meetings"
    assert MeetingStorage(base_path=path).base_path == path


def test_configured_root_when_constructing_storage_then_nested_directory_created(roots, monkeypatch):
    from recog.meeting_storage import MeetingStorage

    target = roots[1]
    monkeypatch.setenv("RECOG_DATA_DIR", str(target))
    storage = MeetingStorage()
    assert storage.base_path == target / "meetings"
    assert storage.base_path.is_dir()


@pytest.mark.parametrize("old_path_exists", [False, True])
def test_moved_audio_when_loading_meeting_then_current_files_win_without_rewriting_meta(tmp_path, old_path_exists):
    """Enroll и playback должны читать перенесённый WAV, даже если старый путь снова занят."""
    from recog.meeting_storage import MeetingStorage

    storage = MeetingStorage(tmp_path / "new/meetings")
    meeting = storage.base_path / "recording"
    meeting.mkdir()
    old = tmp_path / "old.wav"
    if old_path_exists:
        old.write_bytes(b"different recording")
    raw = json.dumps({"audio_mic_path": str(old), "audio_system_path": str(old)})
    (meeting / "meta.json").write_text(raw)
    for track in ("mic", "system"):
        (meeting / f"audio_{track}.wav").write_bytes(b"recording")

    loaded = storage.get_meeting("recording")

    for track in ("mic", "system"):
        assert loaded["meta"][f"audio_{track}_path"] == str(meeting / f"audio_{track}.wav")
    assert storage.get_audio_paths("recording") == (meeting / "audio_mic.wav", meeting / "audio_system.wav")
    assert (meeting / "meta.json").read_text() == raw


def test_configured_root_when_frontend_imports_then_storage_and_speakers_share_it(tmp_path):
    root = Path(__file__).resolve().parent.parent
    data = tmp_path / "data"
    result = subprocess.run(
        [sys.executable, "-c", """
import sys
from pathlib import Path
from recog.web import server
expected = Path(sys.argv[1])
assert server.MEETINGS_DIR == expected / 'meetings'
assert server.get_storage().base_path == expected / 'meetings'
""", str(data)],
        cwd=root, env={**os.environ, "RECOG_DATA_DIR": str(data)},
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr


@pytest.mark.skipif(shutil.which("rsync") is None, reason="rsync required for backup")
def test_migrated_data_when_backing_up_then_all_databases_and_logs_are_in_snapshot(tmp_path):
    root = Path(__file__).resolve().parent.parent
    repo = tmp_path / "checkout"
    (repo / "scripts").mkdir(parents=True)
    for name in ("backup-data.sh", "data-dir.sh", "deps.sh"):
        shutil.copy2(root / "scripts" / name, repo / "scripts" / name)
    (repo / "recog").mkdir()
    for name in ("__init__.py", "config.py", "install_config.py"):
        shutil.copy2(root / "recog" / name, repo / "recog" / name)
    (repo / ".env").write_bytes(b"BACKUP_TEST=value\n")
    data = tmp_path / "My data"
    (data / "meetings").mkdir(parents=True)
    (data / "meetings/transcript.jsonl").write_bytes(b"transcript\n")
    (data / "logs").mkdir()
    (data / "logs/live.log").write_bytes(b"log\n")
    (data / "speakers_custom.json").write_bytes(b"{}")
    destination = tmp_path / "backup"
    result = subprocess.run(
        ["bash", str(repo / "scripts/backup-data.sh"), "--with-logs", "--dest", str(destination)],
        env={**os.environ, "DEPS_PYTHON": sys.executable, "RECOG_DATA_DIR": str(data)},
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr
    for relative in ("meetings/transcript.jsonl", "logs/live.log", "speakers_custom.json"):
        assert (destination / relative).read_bytes() == (data / relative).read_bytes()
    assert (destination / "env.backup").read_bytes() == (repo / ".env").read_bytes()
    assert not (repo / "meetings").exists()
