"""Quality policy boundaries and real tool contracts on a tiny git repository."""

import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))
policy = importlib.import_module("quality_policy")

spec = importlib.util.spec_from_file_location("recog_quality", SCRIPTS / "quality.py")
quality = importlib.util.module_from_spec(spec)
spec.loader.exec_module(quality)


def scored(name="f()", value=15):
    return {
        "path": "recog/a.py",
        "key_name": name,
        "crap": value,
        "cov": 0.5,
        "flag": "measured",
    }


@pytest.mark.parametrize(
    ("old", "new", "fails"),
    [
        (15, 16, False),
        (15, 16.001, True),
        (30, 31, False),
        (30, 31.001, True),
        (40, 15, False),
        (40, 30, False),
    ],
)
def test_existing_score_when_compared_then_absolute_delta_and_ceiling_apply(old, new, fails):
    assert bool(policy.crap_regressions([scored(value=old)], [scored(value=new)])) == fails


@pytest.mark.parametrize(("score", "fails"), [(15, False), (15.001, True)])
def test_new_function_when_scored_then_ceiling_applies(score, fails):
    assert bool(policy.crap_regressions([scored()], [scored("new()", score)])) == fails


@pytest.mark.parametrize(
    "rows",
    [
        [],
        None,
        [scored(value=float("nan"))],
        [scored(value=True)],
        [scored(), scored()],
        [{**scored(), "flag": "untested"}],
        [{**scored(), "cov": 1.1}],
    ],
)
def test_missing_or_malformed_scores_when_compared_then_fail_closed(rows):
    with pytest.raises((ValueError, TypeError, KeyError)):
        policy.crap_regressions(rows, [scored()])


def diagnostic(line, code="F821", message="Undefined name x"):
    return {"code": code, "message": message, "location": {"row": line, "column": 1}}


def test_old_lint_when_lines_move_then_preserved_but_new_findings_blocked():
    mapping = policy.unchanged_lines("bad\nok", "# heading\nbad\nnew_bad\nok")
    findings = policy.lint_regressions([diagnostic(1)], [diagnostic(2), diagnostic(3)], mapping)
    assert findings == [diagnostic(3)]


def test_old_format_debt_when_new_line_is_bad_then_only_new_line_fails():
    before, source = "a=1\n", "a=1\nb=2\n"
    assert policy.format_regressions(
        source, "a = 1\nb = 2\n", policy.unchanged_lines(before, source)
    )
    assert not policy.format_regressions(
        "a=1\n# hello\n",
        "a = 1\n# hello\n",
        policy.unchanged_lines(before, "a=1\n# hello\n"),
    )


def pair(first="a()", second="b()"):
    return {
        "functions": [{"path": "recog/a.py", "key_name": name} for name in (first, second)],
        "similarity": 0.9,
    }


def test_duplicate_candidate_when_new_then_requires_exact_reviewed_exception():
    candidate = pair()
    assert policy.duplicate_regressions([], [candidate], {"duplicates": []})
    assert not policy.duplicate_regressions([candidate], [candidate], {"duplicates": []})
    exception = {
        "functions": ["recog/a.py::a()", "recog/a.py::b()"],
        "reason": "Protocol adapters intentionally preserve separate lifecycle ownership.",
    }
    assert not policy.duplicate_regressions([], [candidate], {"duplicates": [exception]})
    assert policy.duplicate_regressions([], [pair("a()", "c()")], {"duplicates": [exception]})
    with pytest.raises(ValueError):
        policy.duplicate_regressions([], [candidate], {"duplicates": [{**exception, "reason": ""}]})


def test_failed_command_when_measurement_runs_then_rejects_and_retains_log(tmp_path):
    log = tmp_path / "command.log"
    with pytest.raises(RuntimeError, match="Command failed"):
        quality.run(
            [sys.executable, "-c", 'print("failure evidence");raise SystemExit(2)'],
            tmp_path,
            log=log,
        )
    assert "failure evidence" in log.read_text()


@pytest.fixture
def repository(tmp_path):
    root = tmp_path / "repo"
    (root / "recog").mkdir(parents=True)
    (root / "tests").mkdir()
    (root / "recog/__init__.py").write_text("")
    (root / "recog/example.py").write_text("def answer():\n    return 42\n")
    (root / "tests/test_example.py").write_text(
        "from recog.example import answer\n\n\ndef test_answer():\n    assert answer() == 42\n"
    )
    (root / ".gitignore").write_text("__pycache__/\n.pytest_cache/\n")
    for args in [
        ("init", "-q"),
        ("add", "."),
        (
            "-c",
            "user.name=Quality test",
            "-c",
            "user.email=quality@example.invalid",
            "commit",
            "-qm",
            "baseline",
        ),
    ]:
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)
    return root


@pytest.mark.skipif(
    importlib.util.find_spec("crapkit") is None
    or importlib.util.find_spec("scb_check") is None
    or importlib.util.find_spec("pytest_cov") is None
    or importlib.util.find_spec("ruff") is None,
    reason="Install requirements-quality.txt for tool integration tests",
)
def test_real_baseline_when_head_has_new_uncovered_function_then_gate_fails(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    code = "\n\ndef uncovered(value):\n"
    for index in range(5):
        code += f"    if value == {index}:\n        return {index}\n"
    code += "    return -1\n"
    with (repository / "recog/example.py").open("a") as stream:
        stream.write(code)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--output", str(output)]) == 1
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "fail", result
    assert any(item["kind"] == "crap" and item["baseline"] is None for item in result["failures"])
    assert len(result["baseline"]["functions"]) == 1
    assert len(result["head"]["functions"]) == 2


@pytest.mark.skipif(importlib.util.find_spec("ruff") is None, reason="Needs ruff")
@pytest.mark.parametrize(
    ("line", "code"),
    [
        ("\n\ndef added():\n    return 1\n", 0),
        ("\n\ndef added():\n    return [1,2]\n", 1),
    ],
)
def test_style_only_when_changed_line_is_checked_then_only_ruff_decides(
    repository, tmp_path, monkeypatch, capsys, line, code
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda packages=None: {})
    monkeypatch.setattr(quality, "coverage", lambda *args: pytest.fail("tests must not run"))
    with (repository / "recog/example.py").open("a") as stream:
        stream.write(line)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--style-only", "--output", str(output)]) == code
    printed = capsys.readouterr().out
    assert ("recog/example.py:6: differs from ruff format" in printed) == bool(code)
    assert not list(output.glob("*/report.json"))
    assert not list(output.iterdir())


def test_style_only_when_base_is_missing_then_it_reports_that_it_could_not_run(
    repository, tmp_path, monkeypatch, capsys
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda packages=None: {})
    output = tmp_path / "reports"
    assert quality.main(["--base", "no-such-ref", "--style-only", "--output", str(output)]) == 2
    assert "could not run" in capsys.readouterr().out


def test_missing_base_when_runner_starts_then_report_explains_failure(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    output = tmp_path / "reports"
    assert quality.main(["--base", "no-such-ref", "--output", str(output)]) == 1
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "measurement-error"
    assert "merge-base" in result["error"]


@pytest.mark.parametrize(
    "payload",
    [
        "not-json",
        "{}",
        '{"verbosity": NaN}',
        '{"verbosity": 0, "erosion": 0, "cog_erosion": 0, "total_loc": 1, "files_scanned": 0}',
    ],
)
def test_malformed_scb_when_parsed_then_measurement_fails(repository, monkeypatch, payload):
    monkeypatch.setattr(quality, "run", lambda *args, **kwargs: payload)
    with pytest.raises((ValueError, KeyError)):
        quality.scb(repository)


def test_scb_findings_when_report_is_valid_then_return_one_is_accepted(repository, monkeypatch):
    payload = {
        "verbosity": 0.1,
        "erosion": 0.2,
        "cog_erosion": 0.3,
        "total_loc": 2,
        "files_scanned": 2,
    }

    def fake_run(*args, **kwargs):
        assert kwargs["accepted"] == (0, 1)
        return json.dumps(payload)

    monkeypatch.setattr(quality, "run", fake_run)
    monkeypatch.setattr(quality, "source_files", lambda root: {"a": "", "b": ""})
    assert quality.scb(repository) == payload


def test_coverage_hole_when_suite_returns_success_then_measurement_fails(
    repository, tmp_path, monkeypatch
):
    (tmp_path / "head-coverage.json").write_text(
        json.dumps(
            {
                "meta": {"branch_coverage": True},
                "files": {"recog/__init__.py": {"functions": {}}},
            }
        )
    )
    monkeypatch.setattr(quality, "run", lambda *args, **kwargs: "")
    monkeypatch.setattr(quality, "source_files", lambda root: {"recog/example.py": ""})
    with pytest.raises(ValueError, match="Missing coverage function regions"):
        quality.coverage(repository, tmp_path, "head")


def test_wrong_installed_version_when_gate_starts_then_measurement_fails(monkeypatch):
    monkeypatch.setattr(quality.importlib.metadata, "version", lambda package: "0.0.0")
    with pytest.raises(ValueError, match="expected"):
        quality.versions()


@pytest.mark.skipif(
    importlib.util.find_spec("pytest_cov") is None, reason="Optional coverage plugin"
)
def test_baseline_failure_when_head_would_pass_then_gate_does_not_substitute_head(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    test = repository / "tests/test_example.py"
    correct = test.read_text()
    test.write_text(correct.replace("== 42", "== 43"))
    quality.git(repository, "add", "tests/test_example.py")
    quality.git(
        repository,
        "-c",
        "user.name=Quality test",
        "-c",
        "user.email=quality@example.invalid",
        "commit",
        "-qm",
        "broken baseline",
    )
    test.write_text(correct)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--output", str(output)]) == 1
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "measurement-error"
    assert "baseline-tests.log" in result["error"]
    assert not list(output.glob("run-*/head-coverage.json"))


@pytest.mark.parametrize(
    "rows", [{}, None, [{"code": "F821", "message": "error", "location": {"row": 0, "column": 1}}]]
)
def test_malformed_lint_when_compared_then_fails_closed(rows):
    with pytest.raises((TypeError, ValueError, KeyError)):
        policy.lint_regressions(rows, rows, {})


def region():
    return {
        "summary": {
            "num_statements": 1,
            "covered_lines": 1,
            "missing_lines": 0,
            "num_branches": 2,
            "covered_branches": 1,
            "missing_branches": 1,
        },
        "executed_lines": [2],
        "missing_lines": [],
        "executed_branches": [[2, 3]],
        "missing_branches": [[2, 4]],
    }


@pytest.mark.parametrize(
    "field", ["num_branches", "covered_branches", "num_statements", "covered_lines"]
)
def test_missing_coverage_counts_when_parsed_then_no_statement_fallback(field):
    value = region()
    del value["summary"][field]
    with pytest.raises(KeyError):
        policy.validate_region(value)


@pytest.mark.parametrize(
    "update", [{"executed_lines": []}, {"missing_branches": None}, {"executed_branches": [[2]]}]
)
def test_inconsistent_coverage_locations_when_parsed_then_rejected(update):
    with pytest.raises(ValueError):
        policy.validate_region({**region(), **update})


@pytest.mark.parametrize("staged", [False, True])
def test_clean_head_snapshot_when_local_config_exists_then_only_versioned_and_new_source_copied(
    repository, tmp_path, monkeypatch, staged
):
    monkeypatch.setattr(quality, "ROOT", repository)
    with (repository / ".gitignore").open("a") as stream:
        stream.write("private-installation.cfg\n")
    (repository / "private-installation.cfg").write_text("local-only")
    (repository / "recog/new.py").write_text("VALUE = 1\n")
    (repository / "recog/example.py").unlink()
    if staged:
        quality.git(repository, "add", "recog/example.py")
    snapshot = tmp_path / "snapshot"
    try:
        quality.snapshot_head(snapshot)
        assert not (snapshot / "private-installation.cfg").exists()
        assert not (snapshot / "recog/example.py").exists()
        assert (snapshot / "recog/new.py").read_text() == "VALUE = 1\n"
        assert "recog/new.py" in quality.git(snapshot, "ls-files")
        assert "recog/new.py" not in quality.git(repository, "ls-files")
    finally:
        quality.git(repository, "worktree", "remove", "--force", str(snapshot))


@pytest.mark.parametrize(
    "update", [{"verbosity": 1.1}, {"total_loc": 2.0}, {"files_scanned": True}]
)
def test_scb_out_of_range_metrics_when_parsed_then_fail_closed(repository, monkeypatch, update):
    payload = {
        "verbosity": 0.1,
        "erosion": 0.2,
        "cog_erosion": 0.3,
        "total_loc": 2,
        "files_scanned": 2,
        **update,
    }
    monkeypatch.setattr(quality, "run", lambda *args, **kwargs: json.dumps(payload))
    with pytest.raises(ValueError):
        quality.scb(repository)


def test_untracked_ignored_head_file_when_snapshotted_then_old_commit_does_not_resurrect_it(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    quality.git(repository, "update-index", "--force-remove", "recog/example.py")
    with (repository / ".gitignore").open("a") as stream:
        stream.write("recog/example.py\n")
    snapshot = tmp_path / "snapshot"
    try:
        quality.snapshot_head(snapshot)
        assert (repository / "recog/example.py").exists()
        assert not (snapshot / "recog/example.py").exists()
    finally:
        quality.git(repository, "worktree", "remove", "--force", str(snapshot))


def test_incomplete_snapshot_when_gate_starts_then_refused_before_measurement(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    original_snapshot = quality.snapshot_head

    def incomplete_snapshot(destination):
        original_snapshot(destination)
        (destination / "recog/example.py").unlink()

    monkeypatch.setattr(quality, "snapshot_head", incomplete_snapshot)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--output", str(output)]) == 1
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "measurement-error"
    assert "Head snapshot differs" in result["error"]
    assert not list(output.glob("run-*/*-coverage.json"))
