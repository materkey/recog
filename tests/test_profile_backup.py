"""Execute the backup script against an isolated installation fixture."""

import os
from pathlib import Path
import shutil
import subprocess

import pytest


@pytest.mark.parametrize("has_profile", [False, True])
@pytest.mark.parametrize("has_ai_settings", [False, True])
def test_isolated_installation_when_backed_up_then_profile_private_and_environment_unchanged(tmp_path, has_profile, has_ai_settings):
    repo = tmp_path / "checkout"
    scripts = repo / "scripts"
    scripts.mkdir(parents=True)
    shutil.copyfile(Path(__file__).resolve().parents[1] / "scripts/backup-data.sh", scripts / "backup-data.sh")
    data = tmp_path / "data"
    data.mkdir()
    (scripts / "data-dir.sh").write_text('#!/bin/bash\nprintf "%s\\n" "$TEST_DATA_DIR"\n')
    (scripts / "config-path.sh").write_text('#!/bin/bash\nprintf "%s\\n" "$TEST_CONFIG"\n')
    (data / "speakers_test.json").write_text('{"speakers": []}')
    profile = data / "user-profile.json"
    if has_profile:
        profile.write_text('{"version": 1, "name": "Александр", "aliases": []}')
        profile.chmod(0o644)  # Even an older incorrectly public source is backed up privately.
    ai_settings = data / "ai-settings.json"
    if has_ai_settings:
        ai_settings.write_text('{"version": 1, "models": {}, "tasks": {}}')
        ai_settings.chmod(0o644)
    (data / "ai-settings.lock").touch()
    (data / "privacy.json").write_text('{"enabled": true}')
    (repo / ".env").write_text("TEST_ONLY=fixture\n")
    frontend = repo / "recog/web"
    frontend.mkdir(parents=True)
    (frontend / ".env").write_text("TEST_FRONTEND_ONLY=fixture\n")
    dest = tmp_path / "backup"
    result = subprocess.run(
        ["bash", str(scripts / "backup-data.sh"), "--dest", str(dest)], cwd=repo,
        env={
            **os.environ,
            "TEST_DATA_DIR": str(data),
            "TEST_CONFIG": str(tmp_path / "absent.toml"),
        },
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    backed_profile = dest / "user-profile.json"
    assert backed_profile.exists() is has_profile
    if has_profile:
        assert backed_profile.read_bytes() == profile.read_bytes()
        assert backed_profile.stat().st_mode & 0o777 == 0o600
    backed_ai_settings = dest / "ai-settings.json"
    assert backed_ai_settings.exists() is has_ai_settings
    if has_ai_settings:
        assert backed_ai_settings.read_bytes() == ai_settings.read_bytes()
        assert backed_ai_settings.stat().st_mode & 0o777 == 0o600
    assert not (dest / "ai-settings.lock").exists()
    assert (dest / "privacy.json").read_bytes() == (data / "privacy.json").read_bytes()
    assert (dest / "privacy.json").stat().st_mode & 0o777 == 0o600
    assert (dest / "speakers_test.json").read_bytes() == (data / "speakers_test.json").read_bytes()
    for source, filename in ((repo / ".env", "env.backup"), (frontend / ".env", "frontend-env.backup")):
        assert (dest / filename).read_bytes() == source.read_bytes()
        assert (dest / filename).stat().st_mode & 0o777 == 0o600
    assert (repo / ".env").read_text() == "TEST_ONLY=fixture\n"
    assert (frontend / ".env").read_text() == "TEST_FRONTEND_ONLY=fixture\n"


# `recog config ...` as the real binary answers it: the data directory and the
# config path from the environment the test gives.
RECOG_STUB = """#!/bin/sh
case "$*" in
  "config data-dir") printf '%s\\n' "$RECOG_DATA_DIR" ;;
  "config config-path") printf '%s\\n' "$RECOG_CONFIG_FILE" ;;
  *) exit 2 ;;
esac
"""


@pytest.mark.skipif(shutil.which("rsync") is None, reason="rsync required for backup")
def test_migrated_data_when_backing_up_then_all_databases_and_logs_are_in_snapshot(tmp_path):
    """The real data-dir.sh and config-path.sh, which ask bin/recog, feed the backup."""
    root = Path(__file__).resolve().parent.parent
    repo = tmp_path / "checkout"
    (repo / "scripts").mkdir(parents=True)
    for name in ("backup-data.sh", "config-path.sh", "data-dir.sh", "deps.sh"):
        shutil.copy2(root / "scripts" / name, repo / "scripts" / name)
    (repo / "bin").mkdir()
    (repo / "bin/recog").write_text(RECOG_STUB)
    (repo / "bin/recog").chmod(0o755)
    (repo / ".env").write_bytes(b"BACKUP_TEST=value\n")
    data = tmp_path / "My data"
    (data / "meetings").mkdir(parents=True)
    (data / "meetings/transcript.jsonl").write_bytes(b"transcript\n")
    (data / "logs").mkdir()
    (data / "logs/live.log").write_bytes(b"log\n")
    (data / "speakers_custom.json").write_bytes(b"{}")
    config = tmp_path / "config home/recog/config.toml"
    config.parent.mkdir(parents=True)
    config.write_bytes(b'[asr]\nbackend = "local"\n')
    destination = tmp_path / "backup"
    result = subprocess.run(
        ["bash", str(repo / "scripts/backup-data.sh"), "--with-logs", "--dest", str(destination)],
        env={
            **os.environ,
            "RECOG_DATA_DIR": str(data),
            "RECOG_CONFIG_FILE": str(config),
            "RECOG_INSTALL_RUNTIME": "",
        },
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    for relative in ("meetings/transcript.jsonl", "logs/live.log", "speakers_custom.json"):
        assert (destination / relative).read_bytes() == (data / relative).read_bytes()
    assert (destination / "env.backup").read_bytes() == (repo / ".env").read_bytes()
    assert (destination / "config.toml").read_bytes() == config.read_bytes()
    assert not (repo / "meetings").exists()
