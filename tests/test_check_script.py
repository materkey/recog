"""Tests for `scripts/check.sh` and the CI workflow that runs it.

check.sh is driven against a throwaway repo copy whose interpreter and
`recog-service.sh` are stubs on PATH, so the assertions are about *what it
runs, in which order, and which failure stops it* — without compiling anything
or running the real suite a second time.

The workflow tests are drift guards: CI must keep installing what setup.sh
installs (same GigaAM pin, same requirements, macOS under
`deploy/python/constraints/macos.txt`), otherwise a green CI stops saying anything about a
fresh install.
"""
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = REPO_ROOT / ".github/workflows/ci.yml"

# Answers the three interpreter invocations check.sh makes, each with its own
# steerable exit code so a test can fail exactly one step.
PYTHON_STUB = r"""#!/bin/sh
printf '%s\n' "python $*" >> "$STUB_LOG"
case "$*" in
  *compileall*)    exit "${STUB_COMPILEALL_RC:-0}" ;;
  *"-m recog --help"*)  exit "${STUB_HELP_RC:-0}" ;;
  *pytest*)        exit "${STUB_PYTEST_RC:-0}" ;;
esac
exit 0
"""

SERVICE_STUB = r"""#!/bin/sh
printf '%s\n' "recog-service.sh $*" >> "$STUB_LOG"
exit "${STUB_RENDER_RC:-0}"
"""


@pytest.fixture
def repo(tmp_path):
    """A throwaway repo holding check.sh, a stub service script and sources."""
    dst = tmp_path / "recog"
    (dst / "scripts").mkdir(parents=True)
    shutil.copy2(REPO_ROOT / "scripts/check.sh", dst / "scripts/check.sh")
    (dst / "scripts/check.sh").chmod(0o755)

    (dst / "recog-service.sh").write_text(SERVICE_STUB)
    (dst / "recog-service.sh").chmod(0o755)

    for pkg in ("recog",):
        (dst / pkg).mkdir()
        (dst / pkg / "__init__.py").write_text("")

    subprocess.run(["git", "init", "-q"], cwd=dst, check=True)
    return dst


@pytest.fixture
def stub_bin(tmp_path):
    """PATH dir with the stub interpreter, plus the log the stubs append to."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "python3").write_text(PYTHON_STUB)
    (bin_dir / "python3").chmod(0o755)
    return bin_dir, tmp_path / "stub.log"


def run_check(repo, stub_bin, env_extra=None):
    bin_dir, log = stub_bin
    env = {
        "HOME": str(repo.parent / "home"),
        "PATH": f"{bin_dir}{os.pathsep}/usr/bin{os.pathsep}/bin",
        "STUB_LOG": str(log),
        "TERM": "dumb",
    }
    Path(env["HOME"]).mkdir(exist_ok=True)
    env.update(env_extra or {})
    result = subprocess.run(
        ["bash", str(repo / "scripts/check.sh")],
        capture_output=True, text=True, env=env,
    )
    result.calls = log.read_text().splitlines() if log.exists() else []
    return result


def add_script(repo, rel, body="echo hi\n"):
    path = repo / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"#!/bin/bash\n{body}")
    return path


# ---------------------------------------------------------------------------
# The steps
# ---------------------------------------------------------------------------


def test_runs_all_five_steps_and_reports_ok(repo, stub_bin):
    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    for step in ("compileall", "shell syntax", "CLI help",
                 "service units render", "pytest"):
        assert f"=== {step} ===" in result.stdout
    assert result.stdout.strip().endswith("check: OK")


def test_source_packages_when_checking_then_application_and_scripts_are_compiled(repo, stub_bin):
    result = run_check(repo, stub_bin)

    compile_calls = [c for c in result.calls if "compileall" in c]
    assert len(compile_calls) == 1
    assert "recog" in compile_calls[0]
    assert "scripts" in compile_calls[0]


def test_checks_the_syntax_of_every_shell_script(repo, stub_bin):
    add_script(repo, "extra.sh")
    add_script(repo, "nested/deep.sh")

    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    for rel in ("scripts/check.sh", "recog-service.sh", "extra.sh",
                "nested/deep.sh"):
        assert f"bash -n {rel}" in result.stdout


def test_a_broken_shell_script_fails_the_run(repo, stub_bin):
    add_script(repo, "broken.sh", body="if true; then\n")

    result = run_check(repo, stub_bin)

    assert result.returncode != 0
    assert "check: OK" not in result.stdout
    # Stopped before the suite: a syntax error is not worth 90 s of pytest.
    assert not any("pytest" in c for c in result.calls)


def test_ignored_scripts_are_not_checked(repo, stub_bin):
    """Ignored worktrees and vendored trees are none of check.sh's business."""
    (repo / ".gitignore").write_text("junk/\n")
    add_script(repo, "junk/broken.sh", body="if true; then\n")

    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "junk/broken.sh" not in result.stdout


def test_renders_the_units_into_a_throwaway_dir_and_removes_it(repo, stub_bin):
    result = run_check(repo, stub_bin)

    render = next(c for c in result.calls if c.startswith("recog-service.sh"))
    assert render.startswith("recog-service.sh render --dry-run ")
    render_dir = Path(render.split()[-1])
    assert repo not in render_dir.parents
    assert not render_dir.exists()


def test_deselects_the_e2e_marker(repo, stub_bin):
    result = run_check(repo, stub_bin)

    pytest_call = next(c for c in result.calls if "pytest" in c)
    assert "-m not e2e" in pytest_call


# ---------------------------------------------------------------------------
# Failures
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "rc_var,later_step",
    [
        ("STUB_COMPILEALL_RC", "pytest"),
        ("STUB_HELP_RC", "pytest"),
        ("STUB_RENDER_RC", "pytest"),
    ],
)
def test_a_failing_step_stops_the_run(repo, stub_bin, rc_var, later_step):
    result = run_check(repo, stub_bin, env_extra={rc_var: "1"})

    assert result.returncode != 0
    assert "check: OK" not in result.stdout
    assert not any(later_step in c for c in result.calls)


def test_a_failing_suite_fails_the_run(repo, stub_bin):
    result = run_check(repo, stub_bin, env_extra={"STUB_PYTEST_RC": "1"})

    assert result.returncode != 0
    assert "check: OK" not in result.stdout


# ---------------------------------------------------------------------------
# Which interpreter
# ---------------------------------------------------------------------------


def test_recog_check_python_wins(repo, stub_bin, tmp_path):
    bin_dir, log = stub_bin
    chosen = tmp_path / "chosen-python"
    shutil.copy2(bin_dir / "python3", chosen)

    result = run_check(repo, stub_bin, env_extra={"RECOG_CHECK_PYTHON": str(chosen)})

    assert result.returncode == 0, result.stdout + result.stderr
    assert f"check: python is {chosen}" in result.stdout


def test_the_repo_venv_wins_over_path(repo, stub_bin):
    bin_dir, _ = stub_bin
    venv_python = repo / "venv-gigaam/bin/python"
    venv_python.parent.mkdir(parents=True)
    shutil.copy2(bin_dir / "python3", venv_python)

    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    assert f"check: python is {venv_python}" in result.stdout


def test_falls_back_to_path_python3(repo, stub_bin):
    bin_dir, _ = stub_bin

    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    assert f"check: python is {bin_dir / 'python3'}" in result.stdout


# ---------------------------------------------------------------------------
# The playwright guard `pytest -m "not e2e"` depends on
# ---------------------------------------------------------------------------


def test_e2e_collection_survives_a_missing_playwright(tmp_path):
    """`-m "not e2e"` deselects after import, so the e2e module must not
    import playwright at module level on a checkout without it."""
    shadow = tmp_path / "shadow"
    (shadow / "playwright").mkdir(parents=True)
    # A missing package raises ModuleNotFoundError, and since pytest 9.1 that
    # is the only exception `importorskip` turns into a skip by default.
    (shadow / "playwright/__init__.py").write_text(
        'raise ModuleNotFoundError("simulated: playwright is not installed")\n'
    )
    env = dict(os.environ, PYTHONPATH=str(shadow))

    result = subprocess.run(
        [sys.executable, "-m", "pytest", "--collect-only", "-q",
         "-m", "not e2e", "-p", "no:playwright", "-p", "no:cacheprovider",
         "tests/e2e"],
        cwd=REPO_ROOT, capture_output=True, text=True, env=env,
    )

    # 0 = collected something, 5 = nothing left after the module skipped.
    assert result.returncode in (0, 5), result.stdout + result.stderr
    assert "skipped" in result.stdout
    assert "ImportError" not in result.stdout


# ---------------------------------------------------------------------------
# CI workflow
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def workflow():
    return yaml.safe_load(WORKFLOW.read_text())


def _steps(workflow):
    return workflow["jobs"]["check"]["steps"]


def _run_script(workflow):
    return "\n".join(step.get("run", "") for step in _steps(workflow))


def test_ci_runs_on_both_supported_hosts(workflow):
    matrix = workflow["jobs"]["check"]["strategy"]["matrix"]["os"]
    assert set(matrix) == {"ubuntu-latest", "macos-latest"}


def test_ci_uses_python_312(workflow):
    setup = next(s for s in _steps(workflow) if str(s.get("uses", "")).startswith("actions/setup-python"))
    assert setup["with"]["python-version"] == "3.12"


def test_ci_installs_cpu_torch(workflow):
    assert "https://download.pytorch.org/whl/cpu" in _run_script(workflow)


def test_ci_pins_the_same_gigaam_ref_as_setup(workflow):
    setup_sh = (REPO_ROOT / "setup.sh").read_text()
    url = re.search(r'RECOG_GIGAAM_URL:-(\S+?)}', setup_sh).group(1)
    ref = re.search(r'RECOG_GIGAAM_REF:-(\S+?)}', setup_sh).group(1)

    env = workflow["jobs"]["check"]["env"]
    assert env["GIGAAM_URL"] == url
    assert str(env["GIGAAM_REF"]) == ref
    assert "gigaam @ git+${GIGAAM_URL}@${GIGAAM_REF}" in _run_script(workflow)


def test_ci_installs_the_same_requirements_as_setup(workflow):
    script = _run_script(workflow)
    for req in ("deploy/python/requirements.txt", "deploy/python/requirements-web.txt",
                "deploy/python/requirements-macos.txt"):
        assert req in script


def test_ci_constrains_macos_installs(workflow):
    """Without the constraints file CI would resolve the ML stack to versions
    the Mac's MPS/PyObjC behaviour was never verified against."""
    script = _run_script(workflow)
    assert "-c deploy/python/constraints/macos.txt" in script
    # Every pip stage goes through the same variable, so none can skip it.
    assert script.count("pip install $PIP_ARGS") == 3


def test_ci_runs_the_check_script(workflow):
    assert _steps(workflow)[-1]["run"].strip() == "./scripts/check.sh"
