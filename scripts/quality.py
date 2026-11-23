#!/usr/bin/env python3
"""Measure the merge base and working tree with identical tools, then apply policy."""

import argparse
import hashlib
import importlib.metadata
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import quality_rust
from quality_policy import (
    CRAP_CEILING,
    crap_regressions,
    duplicate_regressions,
    erosion_regressions,
    format_regressions,
    lint_regressions,
    number,
    score_index,
    unchanged_lines,
    validate_lint,
)

# At import, so that crapkit's spawned analysis workers, which import this
# module again, read JavaScript with the same tokenizer.
try:
    import lizard_template_literals
except ImportError:  # Quality tools absent: only the policy tests import this module.
    pass
else:
    lizard_template_literals.install()

ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / "scripts"
PYTHON = sys.executable
NODE_TOOLS = ROOT / "deploy/node"
ESLINT = NODE_TOOLS / "node_modules/.bin/eslint"
C8 = NODE_TOOLS / "node_modules/.bin/c8"
EXTENSION = "browser-extension/"
# The crates lie in `crates/`, beside the workspace manifest at the checkout's root.
CRATES = "crates"
# What check.sh tests, minus the doctests (stable rustc does not instrument them): a
# crate listed here runs only these targets, the echo canceller's parity tests
# needing golden data CI does not have.
RUST_RESTRICTED_TESTS = {
    "speexdsp-aec": ["--lib", "--test", "properties", "--test", "cli", "--test", "adapted_golden"]
}
# The web UI and what the extension loads; its tests and build tooling are not production.
JAVASCRIPT_SOURCES = (
    "crates/recog-web/static/",
    "crates/recog-desktop/bootstrap.js",
    f"{EXTENSION}src/",
    f"{EXTENSION}content.js",
    f"{EXTENSION}popup.js",
)


def run(command, cwd, *, log=None, accepted=(0,), **kwargs):
    result = subprocess.run(command, cwd=cwd, text=True, capture_output=True, **kwargs)
    if log:
        log.write_text(result.stdout + result.stderr)
    if result.returncode not in accepted:
        raise RuntimeError(
            f"Command failed ({result.returncode}): {command}; see {log or result.stderr[-2000:]}"
        )
    return result.stdout


def git(root, *arguments):
    return run(["git", *arguments], root)


def listed_files(root, *patterns):
    names = git(
        root,
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        *patterns,
    )
    return sorted({name for name in names.split("\0") if name and (root / name).is_file()})


def moved_files(tree, base, *, staged=False):
    """{a file's name at `base`: its name in `tree`} for the files git reads as moved.

    The index of `tree` is compared when `staged`, else its working files that git tracks.
    """
    arguments = ["diff", "--name-status", "-z", "-M", "-l0", "--diff-filter=R"]
    if staged:
        arguments.append("--cached")
    fields = git(tree, *arguments, base).split("\0")
    return {fields[index + 1]: fields[index + 2] for index in range(0, len(fields) - 2, 3)}


def python_files(root):
    return listed_files(root, "*.py")


def javascript_files(root):
    return listed_files(root, "*.js", "*.cjs", "*.mjs")


def rust_sources(root):
    """Production Rust (`crates/<crate>/src/`) with its test code blanked; see quality_rust."""
    names = [
        name
        for name in listed_files(root, f"{CRATES}/*.rs")
        if quality_rust.is_production_path(name)
    ]
    return quality_rust.production_sources({name: (root / name).read_text() for name in names})


def javascript_sources(root):
    return {
        name: (root / name).read_text()
        for name in javascript_files(root)
        if name.startswith(JAVASCRIPT_SOURCES)
        and not name.endswith(".test.js")
        and name != f"{EXTENSION}src/test-setup.js"
    }


def fingerprint(root):
    names = git(root, "ls-files", "--cached", "--others", "--exclude-standard", "-z")
    digest = hashlib.sha256()
    for name in sorted(set(names.split("\0")) - {""}):
        path = root / name
        if path.is_file():
            digest.update(name.encode() + b"\0" + path.read_bytes())
    return digest.hexdigest()


def versions(packages=None):
    pins = ROOT / "deploy/python/requirements-quality.txt"
    result = {}
    for line in pins.read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        package, version = line.split("==")
        if packages is not None and package not in packages:
            continue
        actual = importlib.metadata.version(package)
        if actual != version:
            raise ValueError(f"{package}: expected {version}, installed {actual}")
        result[package] = actual
    manifest = json.loads((NODE_TOOLS / "package.json").read_text())
    for package, version in manifest["devDependencies"].items():
        if packages is not None and package not in packages:
            continue
        result[package] = installed_version(NODE_TOOLS, package, version, "deploy/node")
    lock = ROOT / EXTENSION / "package-lock.json"
    if lock.is_file() and (packages is None or "jest" in packages):
        version = json.loads(lock.read_text())["packages"]["node_modules/jest"]["version"]
        result["jest"] = installed_version(ROOT / EXTENSION, "jest", version, EXTENSION.rstrip("/"))
    if packages is None:
        result.update(rust_tools())
    return result


def rust_tools():
    """rustc and cargo-llvm-cov as found: CI pins cargo-llvm-cov, rustc is the runner's stable."""
    result = {}
    for name, command in (
        ("rustc", ["rustc", "--version"]),
        ("cargo-llvm-cov", ["cargo", "llvm-cov", "--version"]),
    ):
        try:
            result[name] = run(command, ROOT).strip()
        except (OSError, RuntimeError) as error:
            raise ValueError(
                f"{name} is not available ({error}); install cargo-llvm-cov and the "
                "llvm-tools component (docs/quality.md)"
            ) from error
    return result


def installed_version(prefix, package, version, install):
    installed = prefix / "node_modules" / package / "package.json"
    actual = json.loads(installed.read_text())["version"] if installed.is_file() else None
    if actual != version:
        raise ValueError(
            f"{package}: expected {version}, installed {actual}; run npm ci --prefix {install}"
        )
    return actual


def tests(root, output):
    """The pytest suite (shell scripts and tooling) passes or fails; no Python is measured."""
    env = {**os.environ, "PYTHONPATH": str(root)}
    command = [PYTHON, "-m", "pytest", "-q", "-o", "addopts=", "-m", "not e2e"]
    run(command, root, env=env, log=output / "head-tests.log")


def cargo_target_dir():
    """One target directory for both trees, so the dependencies are built once."""
    return (ROOT / os.environ.get("CARGO_TARGET_DIR", "target")).resolve()


def present_targets(crate):
    """The crate's listed test targets that this tree has (`tests/<name>.rs`): both
    trees are measured with the head's list, and a baseline older than a target the
    list names must still run the rest."""
    arguments = RUST_RESTRICTED_TESTS[crate.name]
    kept = []
    position = 0
    while position < len(arguments):
        if arguments[position] != "--test":
            kept.append(arguments[position])
            position += 1
            continue
        name = arguments[position + 1]
        if (crate / "tests" / f"{name}.rs").is_file():
            kept += ["--test", name]
        position += 2
    return kept


def rust_coverage(root, output, label):
    """{production file: (line hits, branch hits)} of the workspace's tests from cargo llvm-cov."""
    artifact = output / f"{label}-rust.lcov"
    # Incremental compilation, which cargo leaves out when CI is set: without it rustc
    # inlines a small function into the crates that call it and keeps no copy of its own,
    # every test binary carries a record of it, and llvm-cov takes the first one it reads,
    # counted or not, in an order that follows the build's paths.
    env = {**os.environ, "CARGO_TARGET_DIR": str(cargo_target_dir()), "CARGO_INCREMENTAL": "1"}
    restricted = [name for name in RUST_RESTRICTED_TESTS if (root / CRATES / name).is_dir()]
    steps = [
        ("clean", ["clean", "--workspace"]),
        ("tests", ["--no-report", "--workspace", *(f"--exclude={name}" for name in restricted)]),
        *(
            (name, ["--no-report", "-p", name, *present_targets(root / CRATES / name)])
            for name in restricted
        ),
        ("report", ["report", "--lcov", "--output-path", str(artifact)]),
    ]
    for step, arguments in steps:
        log = output / f"{label}-rust-{step}.log"
        run(["cargo", "llvm-cov", *arguments], root, env=env, log=log)
    resolved = root.resolve()
    measured = {}
    for source, hits in quality_rust.parse_lcov(artifact.read_text()).items():
        path = Path(source).resolve()
        name = path.relative_to(resolved).as_posix() if path.is_relative_to(resolved) else ""
        if quality_rust.is_production_path(name):
            measured[name] = hits
    # Every crate has code built on every host: a crate without a measured file
    # means its tests did not run or the report's paths did not join.
    expected = {name.split("/")[1] for name in rust_sources(root)}
    missing = sorted(expected - {name.split("/")[1] for name in measured})
    if not expected or missing:
        raise ValueError(f"Missing Rust coverage of crates: {missing or 'no crates found'}")
    return measured


def javascript_coverage(root, output, label):
    """Istanbul function coverage of production JavaScript: jest for the extension, c8 for node:test."""
    from crapkit.covstream import parse_istanbul_file

    names = javascript_sources(root)
    extension = [name.removeprefix(EXTENSION) for name in names if name.startswith(EXTENSION)]
    web = [name for name in names if not name.startswith(EXTENSION)]
    artifacts = []
    if extension:
        directory = output / f"{label}-extension-coverage"
        command = [
            str(root / EXTENSION / "node_modules/.bin/jest"),
            "--ci",
            "--coverage",
            "--coverageProvider=babel",
            "--coverageReporters=json",
            f"--coverageDirectory={directory}",
            *(f"--collectCoverageFrom={name}" for name in extension),
        ]
        env = {**os.environ, "NODE_OPTIONS": "--experimental-vm-modules"}
        run(command, root / EXTENSION, env=env, log=output / f"{label}-extension-tests.log")
        artifacts.append(directory / "coverage-final.json")
    if web:
        directory = output / f"{label}-web-coverage"
        command = [
            str(C8),
            # Functions from the source's syntax tree: V8 alone omits never-compiled ones.
            "--experimental-monocart",
            "--all",
            "--src=.",
            "--reporter=json",
            f"--report-dir={directory}",
            f"--temp-directory={directory / 'v8'}",
            *(f"--include={name}" for name in web),
            "node",
            "--test",
            "tests/*.test.cjs",
        ]
        run(command, root, log=output / f"{label}-web-tests.log")
        artifacts.append(directory / "coverage-final.json")
    measured = {}
    for artifact in artifacts:
        measured.update(parse_istanbul_file(artifact, repo_root=str(root))[0])
    missing = sorted(set(names) - set(measured))
    if missing:
        raise ValueError(f"Missing JavaScript coverage: {missing}")
    return measured


def rust_function_coverage(rows, rust):
    """crapkit's per-function coverage for the Rust rows, and the rows no line was reported for.

    A function without a single instrumented line was not compiled on this host
    (`#[cfg(target_os = …)]`): it has no coverage to score here, and the other CI
    host measures it.
    """
    from crapkit.coverage_istanbul import FnCoverage

    by_path = {}
    for row in rows:
        if row.path.endswith(".rs"):
            by_path.setdefault(row.path, []).append(row)
    measured, unbuilt = {}, []
    for path, file_rows in by_path.items():
        lines, branches = rust.get(path, ({}, {}))
        spans = [(row.start, row.end) for row in file_rows]
        counted = quality_rust.function_coverage(spans, lines, branches)
        measured[path] = []
        for row, (_, total, hit, branch_total, branch_hit) in zip(file_rows, counted):
            if not total:
                unbuilt.append(row)
                continue
            measured[path].append(
                FnCoverage(
                    row.long_name, row.start, row.end, hit > 0, branch_total, branch_hit, total, hit
                )
            )
    return measured, unbuilt


def structural(root, sources, rust, javascript):
    from crapkit.analyze import analyze_source
    from crapkit.dup import find_duplicates
    from crapkit.keys import key_names, lookup
    from crapkit.merge import UnanalyzableFile
    from crapkit.score import score_rows
    from crapkit.snapshot import build_inventory_rows

    if not sources:
        raise ValueError("Empty production Rust inventory")
    sources = {**sources, **javascript_sources(root)}
    records = []
    for name, code in sources.items():
        is_rust = name.endswith(".rs")
        found = analyze_source(name, quality_rust.lizard_text(code) if is_rust else code)
        if isinstance(found, UnanalyzableFile):
            raise ValueError(found.reason)
        if is_rust:
            functions = [(row.long_name.split()[0], row.start, row.end) for row in found]
            quality_rust.check_functions(name, code, functions)
        records.extend(found)
    rows = build_inventory_rows({"production": records})
    measured, unbuilt = rust_function_coverage(rows, rust)
    measured.update(javascript)
    skipped = set(unbuilt)
    built = [row for row in rows if row not in skipped]
    scored = score_rows(built, measured, lane_scopes={"production"}, target=CRAP_CEILING)
    duplicates = find_duplicates(rows, lambda: sources, top=len(rows) ** 2)
    keys = key_names(rows)
    pair_names = {(row.path, row.long_name, row.start): keys[lookup(row)] for row in rows}
    for pair in duplicates:
        for function in pair["functions"]:
            function["key_name"] = pair_names[
                (function["path"], function["long_name"], function["start"])
            ]
    return {
        "functions": [{**row._asdict(), "key_name": keys[lookup(row)]} for row in scored],
        "duplicates": duplicates,
        "unbuilt": [f"{row.path}::{keys[lookup(row)]}" for row in unbuilt],
    }


def named_as_in_head(measured, moved):
    """The baseline's measurement with its moved files under the head's names, so that
    their functions and duplicate pairs are compared as the same ones."""

    def name(path):
        return moved.get(path, path)

    for row in measured["functions"]:
        row["path"] = name(row["path"])
    for pair in measured["duplicates"]:
        for function in pair["functions"]:
            function["path"] = name(function["path"])
    unbuilt = (item.partition("::") for item in measured["unbuilt"])
    measured["unbuilt"] = [f"{name(path)}::{key}" for path, _, key in unbuilt]
    return measured


def scb(sources):
    """SCB's informational metrics of production Rust, read from a copy of the blanked sources.

    The copy lives outside any repository: SCB skips files a .gitignore above them names.
    """
    executable = Path(sys.executable).parent / "scb-check"
    rust = {name: code for name, code in sources.items() if name.endswith(".rs")}
    with tempfile.TemporaryDirectory(prefix="recog-scb-") as directory:
        tree = Path(directory)
        for name, code in rust.items():
            (tree / name).parent.mkdir(parents=True, exist_ok=True)
            (tree / name).write_text(code)
        command = [
            str(executable),
            "check",
            CRATES,
            "--output-format",
            "json",
            "--config",
            str(CONFIG / "quality-scb.toml"),
        ]
        data = json.loads(run(command, tree, accepted=(0, 1)))
    for key in ("verbosity", "erosion", "cog_erosion", "total_loc", "files_scanned"):
        number(data[key])
    if any(data[key] > 1 for key in ("verbosity", "erosion", "cog_erosion")):
        raise ValueError("SCB ratios must be between zero and one")
    if any(type(data[key]) is not int for key in ("total_loc", "files_scanned")):
        raise ValueError("SCB source counts must be integers")
    if not data["files_scanned"] or data["files_scanned"] != len(rust):
        raise ValueError("SCB inventory does not match production Rust files")
    return data


def lint(root, name, source):
    command = [
        PYTHON,
        "-m",
        "ruff",
        "check",
        "--config",
        str(CONFIG / "quality-ruff.toml"),
        "--output-format",
        "json",
        "--stdin-filename",
        name,
        "-",
    ]
    return validate_lint(json.loads(run(command, root, input=source, accepted=(0, 1))))


def eslint(root, name, source):
    command = [
        str(ESLINT),
        "--config",
        str(CONFIG / "quality-eslint.config.mjs"),
        "--format",
        "json",
        "--no-warn-ignored",
        "--stdin",
        "--stdin-filename",
        name,
    ]
    reports = json.loads(run(command, root, input=source, accepted=(0, 1)))
    # A parse error has no rule; it still counts as a finding of its line.
    return validate_lint(
        [
            {
                "code": message["ruleId"] or "parse-error",
                "message": message["message"],
                "location": {"row": message["line"], "column": message["column"]},
            }
            for report in reports
            for message in report["messages"]
        ]
    )


def changed_files(baseline, head, names, moved):
    """The files of the head that differ from the baseline's, a moved one from itself
    under its former name."""
    former = {new: old for old, new in moved.items()}
    for name in names:
        source = (head / name).read_text()
        old = baseline / former.get(name, name)
        before = old.read_text() if old.is_file() else ""
        if source != before:
            yield name, before, source, unchanged_lines(before, source)


def style(baseline, head, moved):
    failures = []
    for name, before, source, mapping in changed_files(baseline, head, python_files(head), moved):
        findings = lint_regressions(lint(baseline, name, before), lint(head, name, source), mapping)
        failures.extend({"kind": "lint", "path": name, **finding} for finding in findings)
        formatted = run(
            [
                PYTHON,
                "-m",
                "ruff",
                "format",
                "--config",
                str(CONFIG / "quality-ruff.toml"),
                "--stdin-filename",
                name,
                "-",
            ],
            head,
            input=source,
        )
        failures.extend(
            {"kind": "format", "path": name, "line": line}
            for line in format_regressions(source, formatted, mapping)
        )
    for name, before, source, mapping in changed_files(
        baseline, head, javascript_files(head), moved
    ):
        findings = lint_regressions(
            eslint(baseline, name, before), eslint(head, name, source), mapping
        )
        failures.extend({"kind": "lint", "path": name, **finding} for finding in findings)
    return failures


def describe(failure):
    if failure["kind"] == "format":
        return f"{failure['path']}:{failure['line']}: differs from ruff format"
    location = failure["location"]
    return f"{failure['path']}:{location['row']}: {failure['code']} {failure['message']}"


def style_only(base, output):
    """Only the Ruff and ESLint checks of changed lines: seconds, no test suites, no report."""
    output.mkdir(parents=True, exist_ok=True)
    baseline = Path(tempfile.mkdtemp(prefix="style-", dir=output.resolve())) / "baseline-tree"
    try:
        versions({"ruff", "eslint", "@eslint/js", "globals"})
        merge_base = git(ROOT, "merge-base", "HEAD", base).strip()
        git(ROOT, "worktree", "add", "--detach", str(baseline), merge_base)
        failures = style(baseline, ROOT, moved_files(ROOT, merge_base))
    except Exception as error:
        print(f"quality: style check could not run: {type(error).__name__}: {error}")
        return 2
    finally:
        if baseline.exists():
            subprocess.run(
                ["git", "worktree", "remove", "--force", str(baseline)],
                cwd=ROOT,
                capture_output=True,
            )
        shutil.rmtree(baseline.parent, ignore_errors=True)
    for failure in failures:
        print(f"quality: {describe(failure)}")
    if any(failure["kind"] == "format" for failure in failures):
        print("quality: style fail; see ruff format --diff --config scripts/quality-ruff.toml")
        return 1
    if failures:
        print("quality: style fail")
        return 1
    print("quality: style pass")
    return 0


def report(output, data):
    (output / "report.json").write_text(json.dumps(data, indent=2) + "\n")
    lines = [
        "# Recog quality",
        "",
        f"Verdict: **{data['status']}**",
        "",
        f"Merge base: `{data.get('base', 'unresolved')}`",
        "",
    ]
    if "error" in data:
        lines.append(f"Measurement failed: {data['error']}")
    for label in ("baseline", "head"):
        if label in data:
            metrics = data[label]["scb"]
            functions = data[label]["functions"]
            over = [row for row in functions if row["crap"] > CRAP_CEILING]
            rust_over = sum(row["path"].endswith(".rs") for row in over)
            lines.append(
                f"{label}: {len(functions)} functions, {len(over)} over CRAP {CRAP_CEILING} "
                f"({rust_over} Rust, {len(over) - rust_over} JavaScript); "
                f"{len(data[label]['unbuilt'])} Rust functions not built on this host; "
                f"Rust LOC {metrics['total_loc']}; verbosity {metrics['verbosity']:.4f}; "
                f"erosion {metrics['erosion']:.4f}; cognitive erosion {metrics['cog_erosion']:.4f}."
            )
    if data["status"] in ("pass", "fail") and "baseline" in data and "head" in data:
        old = score_index(data["baseline"]["functions"])
        current = score_index(data["head"]["functions"])
        lines.extend(["", "## Informational changes", ""])
        for metric in ("total_loc", "verbosity", "erosion", "cog_erosion"):
            delta = data["head"]["scb"][metric] - data["baseline"]["scb"][metric]
            lines.append(f"- {metric}: {delta:+.4f}")
        lines.extend(["", "CRAP increases (including changes within the +1 tolerance):", ""])
        changes = sorted(
            (
                (row["crap"] - old[key]["crap"], key)
                for key, row in current.items()
                if key in old and row["crap"] > old[key]["crap"]
            ),
            reverse=True,
        )
        lines.extend(
            f"- `{key}`: {old[key]['crap']:.3f} → {current[key]['crap']:.3f} ({delta:+.3f})"
            for delta, key in changes[:20]
        )
        lines.extend(["", "Largest structural contributors (CC / cognitive / function LOC):", ""])
        largest = sorted(current.items(), key=lambda item: item[1]["ccn"], reverse=True)[:10]
        lines.extend(
            f"- `{key}`: {row['ccn']} / {row['cognitive']} / {row['nloc']}" for key, row in largest
        )
        lines.extend(["", "Highest CRAP (CRAP / CC / coverage):", ""])
        worst = sorted(current.items(), key=lambda item: item[1]["crap"], reverse=True)[:10]
        lines.extend(
            f"- `{key}`: {row['crap']:.1f} / {row['ccn']} / {row['cov']:.2f}" for key, row in worst
        )
    lines.extend(["", "## Gate findings", ""])
    lines.extend(
        f"- `{json.dumps(finding, ensure_ascii=False)}`" for finding in data.get("failures", [])
    )
    (output / "report.md").write_text("\n".join(lines) + "\n")


def snapshot_head(destination):
    git(ROOT, "worktree", "add", "--detach", str(destination), "HEAD")
    names = git(ROOT, "ls-files", "--cached", "--others", "--exclude-standard", "-z")
    allowed = set(names.split("\0")) - {""}
    names += "\0" + git(destination, "ls-files", "--cached", "-z")
    for name in sorted(set(names.split("\0")) - {""}):
        source, target = ROOT / name, destination / name
        if name not in allowed or (not source.exists() and not source.is_symlink()):
            target.unlink(missing_ok=True)
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.is_symlink():
            target.unlink()
        if source.is_symlink():
            if target.exists():
                target.unlink()
            target.symlink_to(os.readlink(source))
        else:
            shutil.copy2(source, target)
    git(destination, "add", "-A")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base", required=True, help="Target ref; compare its merge base with HEAD"
    )
    parser.add_argument("--output", default=".local/quality-work", type=Path)
    parser.add_argument(
        "--style-only",
        action="store_true",
        help="Only check Ruff lint and formatting of changed lines",
    )
    args = parser.parse_args(argv)
    if args.style_only:
        return style_only(args.base, args.output)
    args.output.mkdir(parents=True, exist_ok=True)
    output = Path(tempfile.mkdtemp(prefix="run-", dir=args.output.resolve()))
    data = {"status": "measurement-error", "failures": [], "python": sys.version}
    baseline = output / "baseline-tree"
    head = output / "head-tree"
    try:
        data["tools"] = versions()
        data["base"] = git(ROOT, "merge-base", "HEAD", args.base).strip()
        git(ROOT, "worktree", "add", "--detach", str(baseline), data["base"])
        data["source_fingerprint"] = fingerprint(ROOT)
        snapshot_head(head)
        if fingerprint(head) != data["source_fingerprint"]:
            raise ValueError("Head snapshot differs from the working tree; measurement refused")
        moved = moved_files(head, data["base"], staged=True)
        for label, root in [("baseline", baseline), ("head", head)]:
            modules = ROOT / EXTENSION / "node_modules"
            if modules.exists() and (root / EXTENSION).is_dir():
                (root / EXTENSION / "node_modules").symlink_to(modules, target_is_directory=True)
            print(f"quality: measuring {label}; logs in {output}", flush=True)
            rust = rust_coverage(root, output, label)
            javascript = javascript_coverage(root, output, label)
            sources = rust_sources(root)
            data[label] = {**structural(root, sources, rust, javascript), "scb": scb(sources)}
        named_as_in_head(data["baseline"], moved)
        print(f"quality: running pytest; log in {output}", flush=True)
        tests(head, output)
        data["failures"] = style(baseline, head, moved)
        data["failures"] += crap_regressions(
            data["baseline"]["functions"], data["head"]["functions"]
        )
        data["failures"] += erosion_regressions(
            data["baseline"]["functions"], data["head"]["functions"]
        )
        exceptions = json.loads((CONFIG / "quality-exceptions.json").read_text())
        data["failures"] += duplicate_regressions(
            data["baseline"]["duplicates"], data["head"]["duplicates"], exceptions
        )
        if fingerprint(ROOT) != data["source_fingerprint"]:
            raise ValueError("Working tree changed during measurement; rerun on a stable tree")
        data["status"] = "fail" if data["failures"] else "pass"
    except Exception as error:
        data["error"] = f"{type(error).__name__}: {error}"
    finally:
        for tree in (baseline, head):
            if tree.exists():
                # Only this invocation's disposable worktrees are removed.
                subprocess.run(
                    ["git", "worktree", "remove", "--force", str(tree)],
                    cwd=ROOT,
                    capture_output=True,
                )
        report(output, data)
    if "error" in data:
        print(f"quality: {data['error']}")
    print(f"quality: {data['status']}; {output / 'report.md'}")
    return 0 if data["status"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
