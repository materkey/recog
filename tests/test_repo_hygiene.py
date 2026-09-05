"""Drift guard: personal files and generated artifacts stay out of the index.

A colleague clones this repo, so voice embeddings, the compiled Swift helper
and the machine-specific launchd plists must not be tracked — they are seeded
from `.example` files or rendered from `deploy/` templates by `setup.sh`. A
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
    ("speakers*.json", {"speakers_ecapa.json.example"}),
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


def test_the_seed_example_stays_tracked(tracked_root_files):
    assert "speakers_ecapa.json.example" in tracked_root_files


# ---------------------------------------------------------------------------
# `scripts/deps.sh` reads the ASR config with the *host* interpreter, before
# the venv exists (setup.sh step 3 runs before step 4 creates it). That only
# works while `import recog.config` needs nothing but the stdlib — one
# third-party import at the top of either file would abort every clean-machine
# install with a ModuleNotFoundError, while CI (which always has the venv)
# stayed green.
# ---------------------------------------------------------------------------

BARE_IMPORT_FILES = ("recog/__init__.py", "recog/config.py")


def _module_level_imports(path):
    import ast

    tree = ast.parse(path.read_text())
    for node in tree.body:
        if isinstance(node, ast.Import):
            for alias in node.names:
                yield alias.name.split(".")[0]
        elif isinstance(node, ast.ImportFrom):
            if node.level:  # relative import inside the package
                continue
            if node.module:
                yield node.module.split(".")[0]


@pytest.mark.parametrize("relpath", BARE_IMPORT_FILES)
def test_asr_config_probe_stays_importable_without_the_venv(relpath):
    import sys

    third_party = sorted(
        name
        for name in _module_level_imports(REPO_ROOT / relpath)
        if name not in sys.stdlib_module_names and name != "recog"
    )
    assert third_party == [], (
        f"{relpath} imports {third_party} at module level — scripts/deps.sh "
        "runs `import recog.config` with the host python before the venv "
        "exists; import it lazily inside the function that needs it"
    )
