"""Tests for `scripts/check.sh` and the CI workflow that runs it.

check.sh is driven against a throwaway repo copy whose interpreter and
`recog-service.sh` are stubs on PATH, so the assertions are about *what it
runs, in which order, and which failure stops it* — without compiling anything
or running the real suite a second time.

The workflow tests are drift guards: CI must keep installing the tools
check.sh needs and running check.sh itself on both hosts.
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

# Answers the interpreter invocations check.sh makes, each with its own
# steerable exit code so a test can fail exactly one step.
PYTHON_STUB = r"""#!/bin/sh
printf '%s\n' "python $*" >> "$STUB_LOG"
case "$*" in
  *compileall*)    exit "${STUB_COMPILEALL_RC:-0}" ;;
  *scripts/quality.py*) exit "${STUB_QUALITY_RC:-0}" ;;
  *pytest*)        exit "${STUB_PYTEST_RC:-0}" ;;
esac
exit 0
"""

SERVICE_STUB = r"""#!/bin/sh
printf '%s\n' "recog $*" >> "$STUB_LOG"
exit "${STUB_RENDER_RC:-0}"
"""

# The linters log their arguments; gitleaks also logs, per directory scan, the
# files it was given, so a test can tell which files a scan saw.
TOOL_STUB = r"""#!/bin/sh
printf '%s\n' "$(basename "$0") $*" >> "$STUB_LOG"
if [ "$(basename "$0")" = gitleaks ] && [ "$1" = dir ]; then
  find . -type f | sed 's|^\./|gitleaks-file |' >> "$STUB_LOG"
fi
exit "$(eval echo "\${STUB_$(basename "$0" | tr a-z A-Z)_RC:-0}")"
"""


@pytest.fixture
def repo(tmp_path):
    """A throwaway repo holding check.sh, a stub service script and sources."""
    dst = tmp_path / "recog"
    (dst / "scripts").mkdir(parents=True)
    shutil.copy2(REPO_ROOT / "scripts/check.sh", dst / "scripts/check.sh")
    (dst / "scripts/check.sh").chmod(0o755)

    (dst / "target/debug").mkdir(parents=True)
    (dst / "target/debug/recog").write_text(SERVICE_STUB)
    (dst / "target/debug/recog").chmod(0o755)

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
    for tool in ("shellcheck", "actionlint", "gitleaks", "cargo"):
        (bin_dir / tool).write_text(TOOL_STUB)
        (bin_dir / tool).chmod(0o755)
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
    for step in (
        "compileall",
        "shell syntax",
        "shellcheck",
        "secrets",
        "CLI help",
        "service units render",
        "pytest",
    ):
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
    for rel in ("scripts/check.sh", "extra.sh", "nested/deep.sh"):
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

    render = next(c for c in result.calls if c.startswith("recog service render"))
    assert render.startswith("recog service render ")
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


@pytest.mark.parametrize("tool", ["SHELLCHECK", "GITLEAKS"])
def test_linter_finding_when_checking_then_run_stops_before_the_suite(repo, stub_bin, tool):
    result = run_check(repo, stub_bin, env_extra={f"STUB_{tool}_RC": "1"})

    assert result.returncode != 0
    assert "check: OK" not in result.stdout
    assert not any("pytest" in c for c in result.calls)


def add_rust_workspace(repo):
    (repo / "Cargo.toml").write_text("[workspace]\n")


def test_rust_workspace_when_checking_then_format_lint_and_data_free_tests_run_before_pytest(
    repo, stub_bin
):
    """given: a checkout with the Rust workspace and no golden data configured
    when: the checks run
    then: rustfmt, clippy with warnings as errors and the data-free tests of every
    crate run, the other crates' in one build of the workspace (a run per crate
    builds a shared dependency again for each set of features it is asked with),
    and the golden parity tests do not"""
    add_rust_workspace(repo)

    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "=== rust ===" in result.stdout
    cargo = [c for c in result.calls if c.startswith("cargo ")]
    assert cargo == [
        "cargo fmt --check",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "cargo test -p speexdsp-aec --lib --test properties --test cli --test adapted_golden",
        "cargo test -p speexdsp-aec --doc",
        "cargo test --workspace --exclude speexdsp-aec",
        "cargo run -q -p recog-live --bin recog -- --help",
    ]
    # "-m pytest": the logged interpreter paths hold this test's name, "pytest" too.
    assert result.calls.index(cargo[-1]) < next(
        i for i, c in enumerate(result.calls) if "-m pytest" in c
    )


def test_golden_data_when_configured_then_parity_tests_run_too(repo, stub_bin):
    """given: SPEEXDSP_AEC_GOLDEN pointing at the port's golden data
    when: the checks run
    then: the bit-exactness tests against pyaec run as well"""
    add_rust_workspace(repo)

    result = run_check(repo, stub_bin, env_extra={"SPEEXDSP_AEC_GOLDEN": str(repo)})

    assert "cargo test -p speexdsp-aec --test parity --test fft_parity" in result.calls


def test_rust_finding_when_checking_then_run_stops_before_the_suite(repo, stub_bin):
    """given: a clippy or rustfmt finding
    when: the checks run
    then: they fail and pytest does not start"""
    add_rust_workspace(repo)

    result = run_check(repo, stub_bin, env_extra={"STUB_CARGO_RC": "1"})

    assert result.returncode != 0
    assert not any("pytest" in c for c in result.calls)


def test_rust_workspace_when_cargo_is_missing_then_the_run_fails_with_a_hint(repo, stub_bin):
    """given: the Rust workspace but no cargo on PATH
    when: the checks run
    then: they fail and say where cargo comes from"""
    add_rust_workspace(repo)
    bin_dir, _ = stub_bin
    (bin_dir / "cargo").unlink()
    # A distribution's cargo lives in /usr/bin (Arch), so the system directories come
    # through links to everything in them but cargo.
    system = repo.parent / "system-bin"
    system.mkdir()
    for directory in (Path("/usr/bin"), Path("/bin")):
        for entry in directory.iterdir():
            if entry.name != "cargo" and not os.path.lexists(system / entry.name):
                (system / entry.name).symlink_to(entry)

    result = run_check(repo, stub_bin, {"PATH": f"{bin_dir}{os.pathsep}{system}"})

    assert result.returncode != 0
    assert "rustup" in result.stderr


def test_shell_scripts_when_linted_then_each_listed_script_follows_its_sources(repo, stub_bin):
    add_script(repo, "nested/deep.sh")
    (repo / ".gitignore").write_text("junk/\n")
    add_script(repo, "junk/ignored.sh")

    result = run_check(repo, stub_bin)

    call = next(c for c in result.calls if c.startswith("shellcheck "))
    assert call.startswith("shellcheck -x --source-path=SCRIPTDIR ")
    assert "nested/deep.sh" in call and "scripts/check.sh" in call
    assert "junk/ignored.sh" not in call


def test_workflows_when_present_then_actionlint_uses_the_same_shellcheck(repo, stub_bin):
    bin_dir, _ = stub_bin
    assert not any(c.startswith("actionlint") for c in run_check(repo, stub_bin).calls)
    (repo / ".github/workflows").mkdir(parents=True)

    result = run_check(repo, stub_bin, env_extra={"STUB_ACTIONLINT_RC": "1"})

    assert result.returncode != 0
    assert f"actionlint -shellcheck {bin_dir / 'shellcheck'}" in result.calls


def test_secret_scan_when_checking_then_history_and_only_committable_files_are_scanned(
    repo, stub_bin
):
    (repo / ".gitignore").write_text("venv/\n")
    (repo / "venv").mkdir()
    (repo / "venv/vendored.py").write_text("")
    (repo / "new_module.py").write_text("")

    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    args = "--no-banner --redact --config scripts/quality-gitleaks.toml ."
    assert f"gitleaks git {args}" in result.calls
    assert f"gitleaks dir {args}" in result.calls
    scanned = {c.split(" ", 1)[1] for c in result.calls if c.startswith("gitleaks-file ")}
    assert {"new_module.py", ".gitignore", "recog/__init__.py"} <= scanned
    assert "venv/vendored.py" not in scanned


def test_missing_tool_when_checking_then_run_names_it(repo, stub_bin):
    bin_dir, _ = stub_bin
    (bin_dir / "gitleaks").unlink()

    result = run_check(repo, stub_bin)

    assert result.returncode != 0
    assert "check: gitleaks not found (install gitleaks" in result.stderr
    assert not any("pytest" in c for c in result.calls)


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


def test_public_repo_or_pull_request_when_running_ci_then_use_hosted_machines(workflow):
    jobs = workflow["jobs"]
    hosts = jobs["check"]["strategy"]["matrix"]["include"]
    assert {h["os"]: h["hosted"] for h in hosts} == {
        "linux": "ubuntu-24.04",
        "macos": "macos-latest",
    }
    trusted = "github.event.repository.private && github.event_name != 'pull_request'"
    assert (
        jobs["check"]["runs-on"]
        == "${{ " + trusted + " && fromJSON(matrix.trusted) || matrix.hosted }}"
    )
    assert jobs["extension"]["runs-on"] == (
        "${{ "
        + trusted
        + """ && fromJSON('["self-hosted","linux","recog"]') || 'ubuntu-24.04' }}"""
    )


def test_ci_uses_python_312(workflow):
    setup = next(s for s in _steps(workflow) if str(s.get("uses", "")).startswith("actions/setup-python"))
    assert setup["with"]["python-version"] == "3.12"


def test_ci_installs_the_python_tools_the_remaining_tests_need(workflow):
    assert "pip install -r deploy/python/requirements-quality.txt" in _run_script(workflow)


def test_ci_installs_the_linters_check_script_requires(workflow):
    """check.sh fails without them, and its hint names the gitleaks CI pins."""
    assert "npm ci --prefix deploy/node" in _run_script(workflow)
    step = next(s for s in _steps(workflow) if s.get("name") == "Install gitleaks")
    version = step["env"]["GITLEAKS_VERSION"]
    assert f"install gitleaks {version}" in (REPO_ROOT / "scripts/check.sh").read_text()


def test_ci_runs_the_check_script(workflow):
    assert any(step.get("run", "").strip() == "./scripts/check.sh" for step in _steps(workflow))


def test_ci_has_the_rust_components_check_script_runs(workflow):
    assert "rustup component add clippy rustfmt" in _run_script(workflow)


def test_ci_when_on_a_hosted_machine_then_the_rust_build_comes_from_the_cache_main_saved(
    workflow,
):
    """A hosted machine starts empty; a runner that keeps its home keeps its build
    itself, and rust-cache removed rustup from such a runner's ~/.cargo/bin."""
    steps = _steps(workflow)
    cache = next(s for s in steps if str(s.get("uses", "")).startswith("Swatinem/rust-cache@"))
    assert cache["if"] == "runner.environment == 'github-hosted'"
    assert cache["with"]["save-if"] == "${{ github.ref == 'refs/heads/main' }}"
    # Its key carries rustc's version, so the cache comes after the update of stable.
    components = next(s for s in steps if s.get("name") == "Install Rust components")
    assert steps.index(components) < steps.index(cache)


def test_ci_when_gate_measures_rust_then_a_pinned_cargo_llvm_cov_and_llvm_tools_are_installed(
    workflow,
):
    assert "llvm-tools-preview" in _run_script(workflow)
    installs = [
        step["with"]["tool"]
        for step in _steps(workflow)
        if str(step.get("uses", "")).startswith("taiki-e/install-action")
    ]
    assert len(installs) == 1
    assert re.fullmatch(r"cargo-llvm-cov@\d+\.\d+\.\d+", installs[0])


@pytest.mark.parametrize(
    "forced,missing", [(True, True), (True, False), (False, True), (False, False)]
)
def test_push_baseline_when_previous_commit_missing_then_only_forced_push_uses_parent(
    workflow, repo, tmp_path, forced, missing
):
    for message in ("base", "middle", "head"):
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                message,
            ],
            cwd=repo,
            check=True,
        )
    previous = (
        "f" * 40
        if missing
        else subprocess.check_output(
            ["git", "rev-parse", "HEAD~2"],
            cwd=repo,
            text=True,
        ).strip()
    )
    expected = (
        subprocess.check_output(
            ["git", "rev-parse", "HEAD^"],
            cwd=repo,
            text=True,
        ).strip()
        if forced and missing
        else previous
    )
    output = tmp_path / "github-env"
    step = next(s for s in _steps(workflow) if s.get("name") == "Select quality baseline")
    result = subprocess.run(
        ["bash", "-e", "-o", "pipefail", "-c", step["run"]],
        cwd=repo,
        env={
            **os.environ,
            "PR_BASE": "",
            "PUSH_BEFORE": previous,
            "PUSH_FORCED": str(forced).lower(),
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_ENV": str(output),
        },
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert output.read_text() == f"RECOG_QUALITY_BASE={expected}\n"
    assert ("::warning::" in result.stdout) == (forced and missing)


@pytest.mark.parametrize("quality_exit", [0, 1])
def test_quality_enabled_when_gate_finishes_then_verdict_propagates_without_duplicate_pytest(
    repo, stub_bin, quality_exit
):
    result = run_check(
        repo,
        stub_bin,
        {"RECOG_QUALITY_BASE": "origin/main", "STUB_QUALITY_RC": str(quality_exit)},
    )
    assert result.returncode == quality_exit
    assert sum("scripts/quality.py --base origin/main" in call for call in result.calls) == 1
    assert not any("-m pytest" in call for call in result.calls)
    assert ("check: OK" in result.stdout) == (quality_exit == 0)


def _commit_with_origin_main(repo):
    config = ["-c", "user.name=Test", "-c", "user.email=test@example.com"]
    subprocess.run(
        ["git", *config, "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-qm", "base"],
        cwd=repo,
        check=True,
    )
    subprocess.run(["git", "update-ref", "refs/remotes/origin/main", "HEAD"], cwd=repo, check=True)


def test_no_quality_base_when_origin_main_exists_then_ruff_check_precedes_one_pytest(
    repo, stub_bin
):
    _commit_with_origin_main(repo)

    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    quality_calls = [c for c in result.calls if "scripts/quality.py" in c]
    assert len(quality_calls) == 1
    assert "--style-only --base origin/main" in quality_calls[0]
    pytest_calls = [c for c in result.calls if "-m pytest" in c]
    assert len(pytest_calls) == 1
    assert result.calls.index(quality_calls[0]) < result.calls.index(pytest_calls[0])


def test_ruff_check_when_it_fails_then_pytest_does_not_run(repo, stub_bin):
    _commit_with_origin_main(repo)

    result = run_check(repo, stub_bin, {"STUB_QUALITY_RC": "1"})

    assert result.returncode != 0
    assert "check: OK" not in result.stdout
    assert not any("-m pytest" in call for call in result.calls)


def test_explicit_quality_base_when_origin_main_exists_then_full_gate_replaces_pytest(
    repo, stub_bin
):
    _commit_with_origin_main(repo)

    result = run_check(repo, stub_bin, {"RECOG_QUALITY_BASE": "HEAD"})

    assert result.returncode == 0, result.stdout + result.stderr
    quality_calls = [c for c in result.calls if "scripts/quality.py" in c]
    assert len(quality_calls) == 1
    assert "--base HEAD" in quality_calls[0]
    assert "--style-only" not in quality_calls[0]
    assert not any("-m pytest" in call for call in result.calls)


def test_no_quality_base_when_origin_main_is_absent_then_plain_pytest_runs(repo, stub_bin):
    result = run_check(repo, stub_bin)

    assert result.returncode == 0, result.stdout + result.stderr
    assert not any("scripts/quality.py" in call for call in result.calls)
    assert any("-m pytest" in call for call in result.calls)


def test_ci_quality_steps_when_matrix_has_two_hosts_then_only_linux_runs_the_gate(workflow):
    steps = _steps(workflow)
    for name in ("Select quality baseline", "Publish quality reports"):
        step = next(s for s in steps if s.get("name") == name)
        assert "runner.os == 'Linux'" in str(step.get("if", "")), name
    coverage_tool = next(s for s in steps if "taiki-e/install-action" in str(s.get("uses", "")))
    assert "runner.os == 'Linux'" in str(coverage_tool.get("if", ""))
    for name in ("Run checks", "Summarize quality delta"):
        step = next(s for s in steps if s.get("name") == name)
        assert "runner.os" not in str(step.get("if", "")), name
