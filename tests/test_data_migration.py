"""Миграция только переименовывает данные и проверяет весь набор до первого шага."""

import importlib.util
import os
from pathlib import Path

import pytest


@pytest.fixture
def migration():
    script = Path(__file__).resolve().parent.parent / "scripts/migrate-data.py"
    assert script.is_file(), "migration script is missing"
    spec = importlib.util.spec_from_file_location("migrate_data", script)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def data(tmp_path):
    repo = tmp_path / "checkout"
    repo.mkdir()
    (repo / "meetings").mkdir()
    (repo / "meetings/transcript.jsonl").write_bytes(b"transcript\n")
    (repo / "logs").mkdir()
    (repo / "logs/live.log").write_bytes(b"log\n")
    (repo / "speakers_custom.json").write_bytes(b'{"private": []}')
    (repo / ".env").write_bytes(b"PRIVATE=keep\n")
    (repo / "deploy/examples").mkdir(parents=True)
    (repo / "deploy/examples/speakers_ecapa.json.example").write_bytes(b"{}")
    return repo, tmp_path / "home/.recog"


def test_dry_run_when_planning_then_no_files_change(migration, data):
    repo, target = data
    moves = migration.migrate(repo, target)
    assert len(moves) == 3
    assert not target.exists()
    assert (repo / "meetings/transcript.jsonl").is_file()


def test_data_when_migrating_then_bytes_and_inodes_survive_and_config_stays(migration, data):
    repo, target = data
    before = {p.relative_to(repo): (p.read_bytes(), p.stat().st_ino)
              for p in repo.rglob("*") if p.is_file() and p.name != ".env"
              and not p.name.endswith(".example")}
    migration.migrate(repo, target, apply=True)
    assert (repo / ".env").read_bytes() == b"PRIVATE=keep\n"
    assert (repo / "deploy/examples/speakers_ecapa.json.example").read_bytes() == b"{}"
    for relative, expected in before.items():
        moved = target / relative
        assert (moved.read_bytes(), moved.stat().st_ino) == expected
        assert not (repo / relative).exists()
    assert migration.migrate(repo, target, apply=True) == []


def test_late_collision_when_migrating_then_no_earlier_item_moves(migration, data):
    repo, target = data
    target.mkdir(parents=True)
    (target / "speakers_custom.json").write_bytes(b"other")
    with pytest.raises(FileExistsError):
        migration.migrate(repo, target, apply=True)
    assert (repo / "meetings/transcript.jsonl").exists()
    assert (target / "speakers_custom.json").read_bytes() == b"other"


def test_target_inside_checkout_when_migrating_then_rejected(migration, data):
    repo, _ = data
    with pytest.raises(ValueError, match="outside"):
        migration.migrate(repo, repo / "data", apply=True)


def test_different_device_when_migrating_then_rejected_before_rename(migration, data, monkeypatch):
    repo, target = data
    real_stat = Path.stat

    def other_device(path, *args, **kwargs):
        result = real_stat(path, *args, **kwargs)
        if path == repo / "logs":
            values = list(result)
            values[2] += 1
            return os.stat_result(values)
        return result

    monkeypatch.setattr(Path, "stat", other_device)
    with pytest.raises(ValueError, match="filesystem"):
        migration.migrate(repo, target, apply=True)
    assert (repo / "meetings/transcript.jsonl").exists()
    assert not target.exists()


def test_partial_move_when_rerunning_then_remaining_items_complete(migration, data):
    repo, target = data
    target.mkdir(parents=True)
    (repo / "meetings").rename(target / "meetings")
    migration.migrate(repo, target, apply=True)
    assert (target / "meetings/transcript.jsonl").read_bytes() == b"transcript\n"
    assert (target / "logs/live.log").read_bytes() == b"log\n"
