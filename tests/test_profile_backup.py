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
    (repo / ".env").write_text("TEST_ONLY=fixture\n")
    frontend = repo / "recog/web"
    frontend.mkdir(parents=True)
    (frontend / ".env").write_text("TEST_FRONTEND_ONLY=fixture\n")
    dest = tmp_path / "backup"
    result = subprocess.run(
        ["bash", str(scripts / "backup-data.sh"), "--dest", str(dest)], cwd=repo,
        env={**os.environ, "TEST_DATA_DIR": str(data)}, capture_output=True, text=True,
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
    assert (dest / "speakers_test.json").read_bytes() == (data / "speakers_test.json").read_bytes()
    for source, filename in ((repo / ".env", "env.backup"), (frontend / ".env", "frontend-env.backup")):
        assert (dest / filename).read_bytes() == source.read_bytes()
        assert (dest / filename).stat().st_mode & 0o777 == 0o600
    assert (repo / ".env").read_text() == "TEST_ONLY=fixture\n"
    assert (frontend / ".env").read_text() == "TEST_FRONTEND_ONLY=fixture\n"
