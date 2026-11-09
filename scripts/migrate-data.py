#!/usr/bin/env python3
"""Move legacy Recog data outside the checkout (dry run unless --apply).

Stop all services and manual recording/enrollment commands before --apply.
After migration, run recog-service.sh install and recog-service.sh restart.
"""

import argparse
import os
from pathlib import Path
import sys

REPO_DIR = Path(__file__).resolve().parent.parent


# What recog/config.py held before the Python modules were deleted; the recog
# binary resolves the same (rust/recog-core, data_dir.rs). Run with the services'
# RECOG_DATA_DIR in the environment: .env is not read.
def data_entries(root: Path) -> list[Path]:
    """Переносимые данные, включая старые базы спикеров, без .env."""
    candidates = [
        root / "meetings",
        root / "logs",
        root / "user-profile.json",
        root / "ai-settings.json",
        root / "privacy.json",
        *sorted(root.glob("speakers*.json")),
    ]
    return [path for path in candidates if path.exists() or path.is_symlink()]


def data_dir_target() -> Path:
    """Новый каталог: одинаковый для CLI, setup и сервисов, независимо от cwd."""
    value = os.environ.get("RECOG_DATA_DIR", "").strip()
    path = Path(value).expanduser() if value else Path.home() / ".recog"
    if not path.is_absolute():
        raise ValueError("RECOG_DATA_DIR must be an absolute path (or start with ~/)")
    return path


def migrate(repo: Path, target: Path, *, apply: bool = False) -> list[tuple[Path, Path]]:
    """Preflight the whole batch; interrupted batches resume without merging data."""
    repo, target = repo.resolve(), target.expanduser().resolve()
    if target == repo or repo in target.parents or target in repo.parents:
        raise ValueError("Data directory must be outside and separate from the checkout")
    ancestor = target
    while not ancestor.exists():
        ancestor = ancestor.parent
    if not ancestor.is_dir():
        raise ValueError(f"Not a directory: {ancestor}")
    device = ancestor.stat().st_dev
    moves = [(source, target / source.name) for source in data_entries(repo)]
    for source, destination in moves:
        if source.is_symlink():
            raise ValueError(f"Resolve the legacy symlink manually before migration: {source}")
        if destination.exists() or destination.is_symlink():
            raise FileExistsError(f"Destination already exists (no merge or overwrite): {destination}")
        if source.stat().st_dev != device:
            raise ValueError(f"Migration requires the same filesystem: {source} -> {target}")
    if apply and moves:
        target.mkdir(parents=True, exist_ok=True, mode=0o700)
        for source, destination in moves:
            # A failed/interrupted rename leaves that source in place. Re-running
            # preflights the remaining entries and never recopies completed ones.
            if destination.exists() or destination.is_symlink():
                raise FileExistsError(f"Destination appeared during migration: {destination}")
            source.rename(destination)
    return moves


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="rename data; stop all writers first")
    args = parser.parse_args()
    try:
        moves = migrate(REPO_DIR, data_dir_target(), apply=args.apply)
    except (OSError, ValueError) as exc:
        print(f"Migration refused: {exc}", file=sys.stderr)
        return 1
    for source, destination in moves:
        print(f"{'Moved' if args.apply else 'Would move'}: {source} -> {destination}")
    if not moves:
        print("No legacy data to move.")
    elif not args.apply:
        print("Dry run only. Stop services and manual writers, then repeat with --apply.")
    else:
        print("Run ./recog-service.sh install and ./recog-service.sh restart to use the new paths.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
