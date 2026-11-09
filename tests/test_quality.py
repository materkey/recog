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
        (10, 11, False),
        (10, 11.001, True),
        (12, 13.001, True),
        (30, 31, False),
        (30, 31.001, True),
        (40, 10, False),
        (40, 30, False),
    ],
)
def test_existing_score_when_compared_then_absolute_delta_and_ceiling_apply(old, new, fails):
    assert bool(policy.crap_regressions([scored(value=old)], [scored(value=new)])) == fails


@pytest.mark.parametrize(("score", "fails"), [(10, False), (10.001, True)])
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


LIBRARY = """pub fn answer() -> u32 {
    42
}

#[cfg(test)]
mod tests {
    #[test]
    fn answer_when_asked_then_42() {
        assert_eq!(super::answer(), 42);
    }
}
"""


@pytest.fixture
def repository(tmp_path, monkeypatch):
    root = tmp_path / "repo"
    (root / "scripts").mkdir(parents=True)
    (root / "tests").mkdir()
    (root / "crates/demo/src").mkdir(parents=True)
    (root / "Cargo.toml").write_text('[workspace]\nresolver = "3"\nmembers = ["crates/demo"]\n')
    (root / "crates/demo/Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
    )
    (root / "crates/demo/src/lib.rs").write_text(LIBRARY)
    (root / "scripts/example.py").write_text("def answer():\n    return 42\n")
    (root / "tests/test_example.py").write_text("def test_answer():\n    assert 6 * 7 == 42\n")
    (root / ".gitignore").write_text("__pycache__/\n.pytest_cache/\ntarget/\nCargo.lock\n")
    # The crate builds into the test's own directory, not a shared one from the caller.
    monkeypatch.setenv("CARGO_TARGET_DIR", str(tmp_path / "target"))
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


def cargo_llvm_cov_missing():
    try:
        return subprocess.run(["cargo", "llvm-cov", "--version"], capture_output=True).returncode
    except OSError:
        return True


needs_rust_tools = pytest.mark.skipif(
    importlib.util.find_spec("crapkit") is None
    or importlib.util.find_spec("scb_check") is None
    or importlib.util.find_spec("tree_sitter_rust") is None
    or importlib.util.find_spec("ruff") is None
    or cargo_llvm_cov_missing(),
    reason="Install requirements-quality.txt, cargo-llvm-cov and llvm-tools",
)


@needs_rust_tools
def test_real_baseline_when_head_has_new_uncovered_function_then_gate_fails(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    arms = "".join(
        f"    if value == {index} {{\n        return {index};\n    }}\n" for index in range(5)
    )
    library = repository / "crates/demo/src/lib.rs"
    library.write_text(
        library.read_text() + f"\npub fn uncovered(value: u32) -> u32 {{\n{arms}    9\n}}\n"
    )
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--output", str(output)]) == 1
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "fail", result
    failures = [item for item in result["failures"] if item["kind"] == "crap"]
    assert [item["function"].split("::")[0] for item in failures] == ["crates/demo/src/lib.rs"]
    assert failures[0]["baseline"] is None
    # The test module's function is not production code.
    assert [row["long_name"] for row in result["baseline"]["functions"]] == ["answer"]
    assert {row["long_name"].split()[0]: row["cov"] for row in result["head"]["functions"]} == {
        "answer": 1.0,
        "uncovered": 0.0,
    }
    assert "1 passed" in next(output.glob("run-*/head-tests.log")).read_text()


@needs_rust_tools
def test_real_baseline_when_head_moves_a_file_then_its_functions_are_not_new(
    repository, tmp_path, monkeypatch
):
    """given: a baseline with an uncovered function over the ceiling in a module's file
    when: the head renames that file
    then: the function is the baseline's own, not a new one, and the gate passes"""
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    arms = "".join(
        f"    if value == {index} {{\n        return {index};\n    }}\n" for index in range(5)
    )
    library = repository / "crates/demo/src/lib.rs"
    library.write_text("pub mod heavy;\n" + library.read_text())
    (repository / "crates/demo/src/heavy.rs").write_text(
        f"pub fn uncovered(value: u32) -> u32 {{\n{arms}    9\n}}\n"
    )
    commit(repository, "an uncovered function")
    quality.git(repository, "mv", "crates/demo/src/heavy.rs", "crates/demo/src/weighty.rs")
    library.write_text(library.read_text().replace("pub mod heavy;", "pub mod weighty;"))
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--output", str(output)]) == 0
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "pass", result
    assert {row["path"] for row in result["baseline"]["functions"]} == {
        "crates/demo/src/lib.rs",
        "crates/demo/src/weighty.rs",
    }


@needs_rust_tools
def test_real_baseline_when_head_equals_it_then_gate_passes(repository, tmp_path, monkeypatch):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--output", str(output)]) == 0
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "pass", result
    assert result["head"]["scb"]["files_scanned"] == 1


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
    monkeypatch.setattr(quality, "tests", lambda *args: pytest.fail("tests must not run"))
    monkeypatch.setattr(quality, "rust_coverage", lambda *args: pytest.fail("cargo must not run"))
    with (repository / "scripts/example.py").open("a") as stream:
        stream.write(line)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--style-only", "--output", str(output)]) == code
    printed = capsys.readouterr().out
    assert ("scripts/example.py:6: differs from ruff format" in printed) == bool(code)
    assert not list(output.glob("*/report.json"))
    assert not list(output.iterdir())


@pytest.mark.skipif(importlib.util.find_spec("ruff") is None, reason="Needs ruff")
@pytest.mark.parametrize(
    ("addition", "code"),
    [("", 0), ("\n\ndef added():\n    return [1,2]\n", 1)],
)
def test_style_only_when_a_file_moved_then_its_old_findings_are_not_new(
    repository, tmp_path, monkeypatch, capsys, addition, code
):
    """given: a file with an unused import and unformatted code at the merge base
    when: the file moves, with or without a badly formatted new line
    then: only the new line fails; what the baseline had under the old name stays old"""
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda packages=None: {})
    old = repository / "scripts/example.py"
    old.write_text("import os\n\n\ndef answer():\n    return [4,2]\n")
    commit(repository, "debt")
    quality.git(repository, "mv", "scripts/example.py", "scripts/moved.py")
    with (repository / "scripts/moved.py").open("a") as stream:
        stream.write(addition)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--style-only", "--output", str(output)]) == code
    printed = capsys.readouterr().out
    assert "F401" not in printed
    assert "scripts/moved.py:5:" not in printed
    assert ("scripts/moved.py:9: differs from ruff format" in printed) == bool(code)


def commit(root, message):
    subprocess.run(["git", "add", "."], cwd=root, check=True, capture_output=True)
    subprocess.run(
        [
            "git",
            "-c",
            "user.name=Quality test",
            "-c",
            "user.email=quality@example.invalid",
            "commit",
            "-qm",
            message,
        ],
        cwd=root,
        check=True,
        capture_output=True,
    )


@pytest.mark.skipif(not quality.ESLINT.exists(), reason="Run npm ci --prefix deploy/node")
@pytest.mark.parametrize(
    ("addition", "code", "reported"),
    [
        ("function added() {\n  return 1;\n}\n", 0, None),
        ("function added() {\n  missing();\n}\n", 1, "no-undef"),
        ("function added() {\n", 1, "parse-error"),
    ],
)
def test_style_only_when_javascript_changes_then_only_new_eslint_findings_fail(
    repository, tmp_path, monkeypatch, capsys, addition, code, reported
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda packages=None: {})
    script = repository / "crates/recog-web/static/page.js"
    script.parent.mkdir(parents=True)
    old = "function inner() {\n  const unused = 1;\n}\n"
    script.write_text(old)
    commit(repository, "javascript")
    script.write_text("// The old finding moves down a line.\n" + old + addition)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--style-only", "--output", str(output)]) == code
    printed = capsys.readouterr().out
    assert "no-unused-vars" not in printed
    assert ("crates/recog-web/static/page.js:" in printed) == bool(code)
    if reported:
        assert reported in printed


@pytest.mark.skipif(
    importlib.util.find_spec("crapkit") is None
    or importlib.util.find_spec("tree_sitter_rust") is None
    or not (quality.NODE_TOOLS / "node_modules/.bin/c8").exists(),
    reason="Install requirements-quality.txt and run npm ci --prefix deploy/node",
)
def test_real_baseline_when_web_script_gets_uncovered_function_then_gate_fails(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    metrics = {"total_loc": 1, "verbosity": 0, "erosion": 0, "cog_erosion": 0}
    monkeypatch.setattr(quality, "scb", lambda sources: metrics)
    # The Rust side is measured by the tests above; here its coverage is given.
    covered = {"crates/demo/src/lib.rs": ({1: 1, 2: 1, 3: 1}, {})}
    monkeypatch.setattr(quality, "rust_coverage", lambda *args: covered)
    script = repository / "crates/recog-web/static/page.js"
    script.parent.mkdir(parents=True)
    script.write_text("function answer() {\n  return 42;\n}\n")
    (repository / "tests/page.test.cjs").write_text(
        "const { test } = require('node:test');\n"
        "const assert = require('node:assert/strict');\n"
        "const fs = require('node:fs');\n"
        "const path = require('node:path');\n"
        "const vm = require('node:vm');\n"
        "const sourcePath = path.join(__dirname, '../crates/recog-web/static/page.js');\n"
        "test('answer', () => {\n"
        "  const context = vm.createContext({});\n"
        "  vm.runInContext(fs.readFileSync(sourcePath, 'utf8'), context,"
        " { filename: sourcePath });\n"
        "  assert.equal(vm.runInContext('answer()', context), 42);\n"
        "});\n"
    )
    commit(repository, "web script")
    branches = "".join(
        f"  if (value === {index}) {{\n    return {index};\n  }}\n" for index in range(5)
    )
    # A handler that is registered but never called is never compiled, so V8 alone
    # does not report it; the coverage must still list it.
    code = f"function uncovered(value) {{\n{branches}  return -1;\n}}\n"
    code += f"globalThis.handlers = [];\nglobalThis.handlers.push((value) => {{\n{branches}}});\n"
    with script.open("a") as stream:
        stream.write(code)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--output", str(output)]) == 1
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "fail", result
    failures = [item for item in result["failures"] if item["kind"] == "crap"]
    assert [item["function"].split("::")[0] for item in failures] == [
        "crates/recog-web/static/page.js",
        "crates/recog-web/static/page.js",
    ]
    assert all(item["baseline"] is None for item in failures)
    answers = [row for row in result["head"]["functions"] if row["path"].endswith("page.js")]
    assert {row["long_name"].split("(")[0].strip(): row["cov"] for row in answers} == {
        "answer": 1.0,
        "uncovered": 0.0,
        "": 0.0,
    }


def test_node_tool_when_installed_version_differs_then_measurement_fails(tmp_path, monkeypatch):
    (tmp_path / "package.json").write_text('{"devDependencies": {"eslint": "10.12.0"}}')
    monkeypatch.setattr(quality, "NODE_TOOLS", tmp_path)
    with pytest.raises(ValueError, match="installed None; run npm ci"):
        quality.versions({"eslint"})
    installed = tmp_path / "node_modules/eslint/package.json"
    installed.parent.mkdir(parents=True)
    installed.write_text('{"version": "10.11.0"}')
    with pytest.raises(ValueError, match="expected 10.12.0, installed 10.11.0"):
        quality.versions({"eslint"})
    installed.write_text('{"version": "10.12.0"}')
    assert quality.versions({"eslint"}) == {"eslint": "10.12.0"}


def test_extension_jest_when_installed_version_differs_from_lock_then_measurement_fails(
    tmp_path, monkeypatch
):
    (tmp_path / "deploy/python").mkdir(parents=True)
    (tmp_path / "deploy/python/requirements-quality.txt").write_text("")
    (tmp_path / "package.json").write_text('{"devDependencies": {}}')
    extension = tmp_path / "browser-extension"
    extension.mkdir()
    (extension / "package-lock.json").write_text(
        '{"packages": {"node_modules/jest": {"version": "30.1.3"}}}'
    )
    monkeypatch.setattr(quality, "ROOT", tmp_path)
    monkeypatch.setattr(quality, "NODE_TOOLS", tmp_path)
    monkeypatch.setattr(quality, "rust_tools", dict)
    with pytest.raises(ValueError, match="installed None; run npm ci --prefix browser-extension"):
        quality.versions()
    installed = extension / "node_modules/jest/package.json"
    installed.parent.mkdir(parents=True)
    installed.write_text('{"version": "30.1.2"}')
    with pytest.raises(ValueError, match="expected 30.1.3, installed 30.1.2"):
        quality.versions()
    installed.write_text('{"version": "30.1.3"}')
    assert quality.versions() == {"jest": "30.1.3"}


def test_style_only_when_base_is_missing_then_it_reports_that_it_could_not_run(
    repository, tmp_path, monkeypatch, capsys
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda packages=None: {})
    output = tmp_path / "reports"
    assert quality.main(["--base", "no-such-ref", "--style-only", "--output", str(output)]) == 2
    assert "could not run" in capsys.readouterr().out


def test_missing_base_when_runner_starts_then_report_explains_failure(
    repository, tmp_path, monkeypatch, capsys
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    output = tmp_path / "reports"
    assert quality.main(["--base", "no-such-ref", "--output", str(output)]) == 1
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "measurement-error"
    assert "merge-base" in result["error"]
    assert result["error"] in capsys.readouterr().out


def test_missing_rust_tools_when_gate_starts_then_measurement_names_them(monkeypatch):
    def missing(*args, **kwargs):
        raise OSError("No such file or directory: 'cargo'")

    monkeypatch.setattr(quality, "run", missing)
    with pytest.raises(ValueError, match="install cargo-llvm-cov and the llvm-tools component"):
        quality.rust_tools()


SCB_SOURCES = {"crates/a/src/lib.rs": "pub fn a() {}\n", "crates/a/src/b.rs": "", "page.js": ""}


@pytest.mark.parametrize(
    "payload",
    [
        "not-json",
        "{}",
        '{"verbosity": NaN}',
        '{"verbosity": 0, "erosion": 0, "cog_erosion": 0, "total_loc": 1, "files_scanned": 0}',
    ],
)
def test_malformed_scb_when_parsed_then_measurement_fails(monkeypatch, payload):
    monkeypatch.setattr(quality, "run", lambda *args, **kwargs: payload)
    with pytest.raises((ValueError, KeyError)):
        quality.scb(SCB_SOURCES)


def test_scb_findings_when_report_is_valid_then_return_one_is_accepted(monkeypatch):
    payload = {
        "verbosity": 0.1,
        "erosion": 0.2,
        "cog_erosion": 0.3,
        "total_loc": 2,
        "files_scanned": 2,
    }

    def fake_run(command, cwd, **kwargs):
        assert kwargs["accepted"] == (0, 1)
        # Only the production Rust, outside any repository whose .gitignore could hide it.
        names = {path.relative_to(cwd).as_posix() for path in cwd.rglob("*") if path.is_file()}
        assert names == {"crates/a/src/lib.rs", "crates/a/src/b.rs"}
        assert not any((parent / ".git").exists() for parent in cwd.parents)
        return json.dumps(payload)

    monkeypatch.setattr(quality, "run", fake_run)
    assert quality.scb(SCB_SOURCES) == payload


def fake_cargo(lcov, calls):
    real = quality.run

    def run(command, cwd, **kwargs):
        if command[0] != "cargo":
            return real(command, cwd, **kwargs)
        calls.append(command)
        if "report" in command:
            Path(command[command.index("--output-path") + 1]).write_text(lcov)
        return ""

    return run


needs_tree_sitter_rust = pytest.mark.skipif(
    importlib.util.find_spec("tree_sitter_rust") is None, reason="Needs tree-sitter-rust"
)


@needs_tree_sitter_rust
@pytest.mark.parametrize("source", ["/elsewhere/crates/demo/src/lib.rs", None])
def test_coverage_report_when_paths_do_not_join_then_measurement_fails(
    repository, tmp_path, monkeypatch, source
):
    lcov = f"SF:{source}\nDA:1,1\nend_of_record\n" if source else ""
    monkeypatch.setattr(quality, "run", fake_cargo(lcov, []))
    with pytest.raises(ValueError, match=r"Missing Rust coverage of crates: \['demo'\]"):
        quality.rust_coverage(repository, tmp_path, "head")


@needs_tree_sitter_rust
def test_coverage_report_when_paths_join_then_production_files_are_returned(
    repository, tmp_path, monkeypatch
):
    calls = []
    lcov = f"SF:{repository}/crates/demo/src/lib.rs\nDA:1,3\nDA:2,0\nend_of_record\n"
    lcov += "SF:/registry/serde/src/lib.rs\nDA:1,1\nend_of_record\n"
    monkeypatch.setattr(quality, "run", fake_cargo(lcov, calls))
    assert quality.rust_coverage(repository, tmp_path, "head") == {
        "crates/demo/src/lib.rs": ({1: 3, 2: 0}, {})
    }
    assert calls[:2] == [
        ["cargo", "llvm-cov", "clean", "--workspace"],
        ["cargo", "llvm-cov", "--no-report", "--workspace"],
    ]


@needs_tree_sitter_rust
def test_coverage_build_when_cargo_would_skip_incremental_then_it_is_asked_for(
    repository, tmp_path, monkeypatch
):
    """given: CI, where cargo builds without incremental compilation
    when: the workspace's coverage is measured
    then: every cargo call asks for it; without it rustc inlines small functions into
    the crates that call them, and their coverage follows the order of the object files"""
    monkeypatch.setenv("CI", "true")
    monkeypatch.delenv("CARGO_INCREMENTAL", raising=False)
    lcov = f"SF:{repository}/crates/demo/src/lib.rs\nDA:1,1\nend_of_record\n"
    asked = []
    cargo = fake_cargo(lcov, [])

    def run(command, cwd, **kwargs):
        if command[0] == "cargo":
            asked.append(kwargs["env"].get("CARGO_INCREMENTAL"))
        return cargo(command, cwd, **kwargs)

    monkeypatch.setattr(quality, "run", run)
    quality.rust_coverage(repository, tmp_path, "head")
    assert asked == ["1", "1", "1"]


@needs_tree_sitter_rust
def test_listed_test_target_absent_from_a_tree_when_measured_then_left_out(
    repository, tmp_path, monkeypatch
):
    # a baseline older than a target the list names (the gate runs its own list on both trees)
    (repository / "crates/speexdsp-aec/src").mkdir(parents=True)
    (repository / "crates/speexdsp-aec/tests").mkdir()
    for target in ("properties", "cli"):
        (repository / f"crates/speexdsp-aec/tests/{target}.rs").write_text("")
    calls = []
    lcov = f"SF:{repository}/crates/demo/src/lib.rs\nDA:1,1\nend_of_record\n"
    monkeypatch.setattr(quality, "run", fake_cargo(lcov, calls))
    quality.rust_coverage(repository, tmp_path, "baseline")
    assert calls[2] == [
        "cargo",
        "llvm-cov",
        "--no-report",
        "-p",
        "speexdsp-aec",
        "--lib",
        "--test",
        "properties",
        "--test",
        "cli",
    ]


@needs_tree_sitter_rust
def test_crate_with_data_bound_tests_when_measured_then_only_its_listed_targets_run(
    repository, tmp_path, monkeypatch
):
    (repository / "crates/speexdsp-aec/src").mkdir(parents=True)
    (repository / "crates/speexdsp-aec/tests").mkdir()
    for target in ("properties", "cli", "adapted_golden"):
        (repository / f"crates/speexdsp-aec/tests/{target}.rs").write_text("")
    calls = []
    lcov = f"SF:{repository}/crates/demo/src/lib.rs\nDA:1,1\nend_of_record\n"
    monkeypatch.setattr(quality, "run", fake_cargo(lcov, calls))
    quality.rust_coverage(repository, tmp_path, "head")
    assert calls[1] == ["cargo", "llvm-cov", "--no-report", "--workspace", "--exclude=speexdsp-aec"]
    assert calls[2] == [
        "cargo",
        "llvm-cov",
        "--no-report",
        "-p",
        "speexdsp-aec",
        "--lib",
        "--test",
        "properties",
        "--test",
        "cli",
        "--test",
        "adapted_golden",
    ]


def test_wrong_installed_version_when_gate_starts_then_measurement_fails(monkeypatch):
    monkeypatch.setattr(quality.importlib.metadata, "version", lambda package: "0.0.0")
    with pytest.raises(ValueError, match="expected"):
        quality.versions()


@needs_rust_tools
def test_baseline_failure_when_head_would_pass_then_gate_does_not_substitute_head(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    monkeypatch.setattr(quality, "versions", lambda: {})
    test = repository / "crates/demo/src/lib.rs"
    correct = test.read_text()
    test.write_text(correct.replace("answer(), 42", "answer(), 43"))
    quality.git(repository, "add", "crates/demo/src/lib.rs")
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
    assert "baseline-rust-tests.log" in result["error"]
    assert not list(output.glob("run-*/head-*"))


@pytest.mark.parametrize(
    "rows", [{}, None, [{"code": "F821", "message": "error", "location": {"row": 0, "column": 1}}]]
)
def test_malformed_lint_when_compared_then_fails_closed(rows):
    with pytest.raises((TypeError, ValueError, KeyError)):
        policy.lint_regressions(rows, rows, {})


@pytest.mark.parametrize("staged", [False, True])
def test_clean_head_snapshot_when_local_config_exists_then_only_versioned_and_new_source_copied(
    repository, tmp_path, monkeypatch, staged
):
    monkeypatch.setattr(quality, "ROOT", repository)
    with (repository / ".gitignore").open("a") as stream:
        stream.write("private-installation.cfg\n")
    (repository / "private-installation.cfg").write_text("local-only")
    (repository / "scripts/new.py").write_text("VALUE = 1\n")
    (repository / "scripts/example.py").unlink()
    if staged:
        quality.git(repository, "add", "scripts/example.py")
    snapshot = tmp_path / "snapshot"
    try:
        quality.snapshot_head(snapshot)
        assert not (snapshot / "private-installation.cfg").exists()
        assert not (snapshot / "scripts/example.py").exists()
        assert (snapshot / "scripts/new.py").read_text() == "VALUE = 1\n"
        assert "scripts/new.py" in quality.git(snapshot, "ls-files")
        assert "scripts/new.py" not in quality.git(repository, "ls-files")
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
        quality.scb(SCB_SOURCES)


def test_untracked_ignored_head_file_when_snapshotted_then_old_commit_does_not_resurrect_it(
    repository, tmp_path, monkeypatch
):
    monkeypatch.setattr(quality, "ROOT", repository)
    quality.git(repository, "update-index", "--force-remove", "scripts/example.py")
    with (repository / ".gitignore").open("a") as stream:
        stream.write("scripts/example.py\n")
    snapshot = tmp_path / "snapshot"
    try:
        quality.snapshot_head(snapshot)
        assert (repository / "scripts/example.py").exists()
        assert not (snapshot / "scripts/example.py").exists()
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
        (destination / "scripts/example.py").unlink()

    monkeypatch.setattr(quality, "snapshot_head", incomplete_snapshot)
    output = tmp_path / "reports"
    assert quality.main(["--base", "HEAD", "--output", str(output)]) == 1
    result = json.loads(next(output.glob("run-*/report.json")).read_text())
    assert result["status"] == "measurement-error"
    assert "Head snapshot differs" in result["error"]
    assert not list(output.glob("run-*/*.lcov"))


rust = importlib.import_module("quality_rust")


@pytest.mark.parametrize(
    ("arguments", "needed"),
    [
        ("(test)", True),
        ("(all(test, unix))", True),
        ("(all(unix, any(test, test)))", True),
        ('(any(test, feature = "fake"))', False),
        ("(not(test))", False),
        ('(target_os = "macos")', False),
        ("(any())", False),
    ],
)
def test_cfg_predicate_when_read_then_only_one_false_without_test_needs_test(arguments, needed):
    assert rust.requires_test(arguments) is needed


@pytest.mark.parametrize("arguments", ["(all(test", "test", "(test, unix)", "(test))"])
def test_malformed_cfg_predicate_when_read_then_refused(arguments):
    with pytest.raises((ValueError, IndexError)):
        rust.requires_test(arguments)


@pytest.mark.parametrize(
    ("attribute", "test"),
    [
        ("#[test]", True),
        ('#[tokio::test(flavor = "multi_thread")]', True),
        ("#[cfg(test)]", True),
        ("#![cfg(test)]", True),
        ("#[cfg_attr(test, allow(dead_code))]", False),
        ("#[cfg(not(test))]", False),
        ("#[derive(Debug)]", False),
        ("#[test_case(1)]", False),
    ],
)
def test_attribute_when_read_then_test_code_is_recognised(attribute, test):
    assert rust.is_test_attribute(attribute) is test


CRATE = {
    "crates/c/src/lib.rs": (
        "pub mod a;\n"
        "#[cfg(test)]\n"
        "mod tests;\n"
        "#[cfg(test)]\n"
        "mod helpers {\n"
        "    pub fn helper() {}\n"
        "}\n"
        "pub fn kept() -> u8 {\n"
        "    #[cfg(test)]\n"
        "    let _ = test_only();\n"
        "    1\n"
        "}\n"
        "#[test]\n"
        "fn loose() {}\n"
        "#[cfg(all(test, unix))]\n"
        "fn unix_only() {}\n"
        "#[cfg(not(test))]\n"
        "fn production_only() {}\n"
    ),
    "crates/c/src/a.rs": (
        "pub fn a() -> char {\n    'é'\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n"
    ),
    "crates/c/src/tests.rs": "mod nested;\nfn shared() {}\n",
    "crates/c/src/tests/nested.rs": "fn deep() {}\n",
    "crates/c/src/whole.rs": "#![cfg(test)]\nfn inside() {}\n",
    "crates/c/src/bin/tool.rs": "#[cfg(test)]\nmod tool_tests;\nfn main() {}\n",
    "crates/c/src/bin/tool_tests.rs": "fn checked() {}\n",
}


@needs_tree_sitter_rust
def test_crate_sources_when_read_then_test_code_is_blanked_and_test_files_left_out():
    production = rust.production_sources(CRATE)
    assert set(production) == {
        "crates/c/src/lib.rs",
        "crates/c/src/a.rs",
        "crates/c/src/bin/tool.rs",
    }
    for name, code in production.items():
        assert code.count("\n") == CRATE[name].count("\n")
        assert len(code.encode()) == len(CRATE[name].encode())
    library = production["crates/c/src/lib.rs"]
    for kept in ("pub mod a;", "pub fn kept() -> u8 {", "fn production_only() {}"):
        assert kept in library
    for gone in ("mod tests;", "helper", "test_only", "loose", "unix_only", "#[test]", "cfg(test)"):
        assert gone not in library
    assert library.splitlines()[7] == "pub fn kept() -> u8 {"
    assert "fn t()" not in production["crates/c/src/a.rs"]
    assert "'é'" in production["crates/c/src/a.rs"]


@needs_tree_sitter_rust
@pytest.mark.parametrize(
    "files",
    [
        {"crates/c/src/lib.rs": "#[cfg(test)]\nmod missing;\n"},
        {"crates/c/src/lib.rs": "#[cfg(test)]\nmod a;\n", "crates/c/src/a.rs": "mod b;\n"},
    ],
)
def test_test_module_when_its_file_cannot_be_found_then_refused(files):
    with pytest.raises(ValueError, match="Cannot resolve module"):
        rust.production_sources(files)


@needs_tree_sitter_rust
def test_unparsable_source_when_read_then_refused():
    with pytest.raises(ValueError, match="does not parse"):
        rust.production_sources({"crates/c/src/lib.rs": "fn broken( {\n"})


LIZARD_HAZARDS = """pub fn word(byte: u8) -> bool {
    byte == b'_' || byte == b'a'
}

pub fn raw() -> &'static str {
    r#"{
"#
}

pub trait Shape {
    fn area(&self) -> u32;
    fn doubled(&self) -> u32 {
        if self.area() > 1 { self.area() * 2 } else { 0 }
    }
}
"""
NEUTRALISED = [("word", 1, 3), ("raw", 5, 8), ("doubled", 12, 14)]


@pytest.mark.skipif(
    importlib.util.find_spec("crapkit") is None
    or importlib.util.find_spec("tree_sitter_rust") is None,
    reason="Install requirements-quality.txt",
)
def test_lizard_hazards_when_neutralised_then_each_function_keeps_its_own_span():
    from crapkit.analyze import analyze_source

    production = rust.production_sources({"crates/c/src/lib.rs": LIZARD_HAZARDS})
    production = production["crates/c/src/lib.rs"]

    def functions(code):
        rows = analyze_source("a.rs", code)
        return [(row.long_name.split()[0], row.start, row.end) for row in rows]

    # lizard 1.24.0 alone: `b'_'` opens a quote that swallows the next function, and the
    # bodiless `area` takes the body of `doubled`.
    assert functions(LIZARD_HAZARDS) != NEUTRALISED
    with pytest.raises(ValueError, match="lizard misread"):
        rust.check_functions("a.rs", LIZARD_HAZARDS, functions(LIZARD_HAZARDS))
    assert functions(rust.lizard_text(production)) == NEUTRALISED
    rust.check_functions("a.rs", production, NEUTRALISED)
    assert rust.lizard_text(production).count("\n") == production.count("\n")


@pytest.mark.parametrize(
    "text",
    [
        "DA:1,1\n",
        "end_of_record\n",
        "SF:a.rs\nDA:x,1\nend_of_record\n",
        "SF:a.rs\nDA:1\nend_of_record\n",
        "SF:a.rs\nDA:1,-1\nend_of_record\n",
        "SF:a.rs\nBRDA:1,0,0\nend_of_record\n",
        "SF:a.rs\nSF:b.rs\nend_of_record\n",
        "SF:a.rs\nDA:1,1\n",
    ],
)
def test_malformed_lcov_when_parsed_then_refused(text):
    with pytest.raises(ValueError):
        rust.parse_lcov(text)


def test_lcov_when_parsed_then_lines_and_branches_per_file():
    text = "TN:\nSF:a.rs\nFN:1,f\nDA:1,2\nDA:2,0,abc\nBRDA:2,0,0,1\nBRDA:2,0,1,-\nend_of_record\n"
    assert rust.parse_lcov(text) == {"a.rs": ({1: 2, 2: 0}, {2: [1, 0]})}


def test_nested_function_when_attributed_then_its_lines_are_not_the_enclosers():
    lines = {1: 1, 2: 0, 3: 1, 4: 1, 6: 0, 20: 1}
    counted = rust.function_coverage([(1, 10), (3, 5)], lines, {4: [1, 0]})
    assert counted == [((1, 10), 3, 1, 0, 0), ((3, 5), 2, 2, 2, 1)]


@pytest.mark.skipif(importlib.util.find_spec("crapkit") is None, reason="Needs crapkit")
def test_function_without_instrumented_lines_when_scored_then_left_out_as_not_built():
    from crapkit.snapshot import InventoryRow

    def row(path, name, start, end):
        return InventoryRow("production", path, name, start, end, 1, 1, 1, 3, 0, 0)

    rows = [
        row("crates/c/src/lib.rs", "built", 1, 3),
        row("crates/c/src/lib.rs", "macos_only", 5, 7),
        row("page.js", "answer", 1, 3),
    ]
    rust_lines = {"crates/c/src/lib.rs": ({1: 1, 2: 0}, {})}
    measured, unbuilt = quality.rust_function_coverage(rows, rust_lines)
    assert unbuilt == [rows[1]]
    assert [(fn.name, fn.coverage) for fn in measured["crates/c/src/lib.rs"]] == [("built", 0.5)]
    assert "page.js" not in measured
