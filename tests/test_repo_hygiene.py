"""Drift guard: personal files and generated artifacts stay out of the index.

A colleague clones this repo, so personal data, the compiled Swift helper
and the machine-specific launchd plists must not be tracked — generated files
are built or rendered from `deploy/` templates by `setup.sh`. A
`.gitignore` rule alone does not untrack a file that was committed before the
rule existed (which is exactly how the four root plists survived step 8), so
the assertion is on `git ls-files`, not on the ignore rules.
"""
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent

# (glob matched against tracked paths, exceptions that are meant to be tracked)
UNTRACKED_PATTERNS = [
    ("speakers*.json", set()),
    ("ai-settings.json", set()),
    ("ai-settings.lock", set()),
    ("sck_audio_capture", set()),
    ("com.recog.*.plist", set()),
]


def _git(*args):
    return subprocess.run(
        ["git", *args],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout


@pytest.fixture(scope="module")
def tracked_root_files():
    inside = subprocess.run(
        ["git", "rev-parse", "--is-inside-work-tree"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if inside.returncode != 0:
        pytest.skip("not a git checkout")
    # Only the repo root: the ignore rules are anchored there, and the
    # `deploy/*.in` templates carry the same names one level down.
    return [p for p in _git("ls-files").splitlines() if "/" not in p]


@pytest.mark.parametrize("pattern,allowed", UNTRACKED_PATTERNS)
def test_personal_files_are_not_tracked(tracked_root_files, pattern, allowed):
    offenders = [
        p for p in tracked_root_files if Path(p).match(pattern) and p not in allowed
    ]
    assert offenders == [], (
        f"{offenders} must be untracked (git rm --cached), not merely gitignored"
    )


@pytest.mark.parametrize("pattern,_allowed", UNTRACKED_PATTERNS)
def test_gitignore_covers_the_pattern(pattern, _allowed):
    rules = (REPO_ROOT / ".gitignore").read_text().splitlines()
    assert f"/{pattern}" in rules, f"/{pattern} missing from .gitignore"
