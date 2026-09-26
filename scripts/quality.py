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

from quality_policy import (
    crap_regressions,
    duplicate_regressions,
    format_regressions,
    lint_regressions,
    number,
    score_index,
    unchanged_lines,
    validate_lint,
    validate_region,
)

ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / "scripts"
PYTHON = sys.executable


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


def python_files(root):
    names = git(
        root,
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        "*.py",
    )
    return sorted({name for name in names.split("\0") if name and (root / name).is_file()})


def source_files(root):
    return {
        name: (root / name).read_text()
        for name in python_files(root)
        if name.startswith("recog/") and "/native/" not in name
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
    return result


def coverage(root, output, label):
    artifact = output / f"{label}-coverage.json"
    env = {
        **os.environ,
        "COVERAGE_FILE": str(output / f"{label}.coverage"),
        "PYTHONPATH": str(root),
    }
    command = [
        PYTHON,
        "-m",
        "pytest",
        "-q",
        "-o",
        "addopts=",
        "-m",
        "not e2e",
        "--cov=recog",
        "--cov-branch",
        f"--cov-config={CONFIG / 'quality-coverage.ini'}",
        f"--cov-report=json:{artifact}",
    ]
    run(command, root, env=env, log=output / f"{label}-tests.log")
    data = json.loads(artifact.read_text())
    if data["meta"]["branch_coverage"] is not True or not data["files"]:
        raise ValueError("Missing branch coverage or file inventory")
    expected = source_files(root)
    for name in expected:
        measured = data["files"].get(name)
        if measured is None or not isinstance(measured.get("functions"), dict):
            raise ValueError(f"Missing coverage function regions: {name}")
        validate_region(measured)
        for region in measured["functions"].values():
            validate_region(region)
    return artifact


def structural(root, artifact):
    from crapkit.analyze import analyze_source
    from crapkit.covstream import parse_coveragepy_file
    from crapkit.dup import find_duplicates
    from crapkit.keys import key_names, lookup
    from crapkit.score import score_rows
    from crapkit.snapshot import build_inventory_rows

    sources = source_files(root)
    if not sources:
        raise ValueError("Empty production source inventory")
    records = [record for name, code in sources.items() for record in analyze_source(name, code)]
    rows = build_inventory_rows({"production": records})
    measured, _ = parse_coveragepy_file(artifact, path_prefix="")
    scored = score_rows(rows, measured, lane_scopes={"production"}, target=15)
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
    }


def scb(root):
    executable = Path(sys.executable).parent / "scb-check"
    data = json.loads(
        run(
            [
                str(executable),
                "check",
                "recog",
                "--output-format",
                "json",
                "--config",
                str(CONFIG / "quality-scb.toml"),
            ],
            root,
            accepted=(0, 1),
        )
    )
    for key in ("verbosity", "erosion", "cog_erosion", "total_loc", "files_scanned"):
        number(data[key])
    if any(data[key] > 1 for key in ("verbosity", "erosion", "cog_erosion")):
        raise ValueError("SCB ratios must be between zero and one")
    if any(type(data[key]) is not int for key in ("total_loc", "files_scanned")):
        raise ValueError("SCB source counts must be integers")
    if not data["files_scanned"] or data["files_scanned"] != len(source_files(root)):
        raise ValueError("SCB inventory does not match production Python files")
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


def style(baseline, head):
    failures = []
    for name in python_files(head):
        source = (head / name).read_text()
        before = (baseline / name).read_text() if (baseline / name).is_file() else ""
        if source == before:
            continue
        mapping = unchanged_lines(before, source)
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
    return failures


def describe(failure):
    if failure["kind"] == "format":
        return f"{failure['path']}:{failure['line']}: differs from ruff format"
    location = failure["location"]
    return f"{failure['path']}:{location['row']}: {failure['code']} {failure['message']}"


def style_only(base, output):
    """Only the Ruff checks of changed lines: seconds, no test suites and no report."""
    output.mkdir(parents=True, exist_ok=True)
    baseline = Path(tempfile.mkdtemp(prefix="style-", dir=output.resolve())) / "baseline-tree"
    try:
        versions({"ruff"})
        merge_base = git(ROOT, "merge-base", "HEAD", base).strip()
        git(ROOT, "worktree", "add", "--detach", str(baseline), merge_base)
        failures = style(baseline, ROOT)
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
    if failures:
        print("quality: style fail; see ruff format --diff --config scripts/quality-ruff.toml")
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
            lines.append(
                f"{label}: {len(data[label]['functions'])} functions; "
                f"LOC {metrics['total_loc']}; verbosity {metrics['verbosity']:.4f}; "
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
        for label, root in [("baseline", baseline), ("head", head)]:
            if (ROOT / "venv-gigaam").exists():
                (root / "venv-gigaam").symlink_to(ROOT / "venv-gigaam", target_is_directory=True)
            print(f"quality: measuring {label}; logs in {output}", flush=True)
            artifact = coverage(root, output, label)
            data[label] = {**structural(root, artifact), "scb": scb(root)}
        data["failures"] = style(baseline, head)
        data["failures"] += crap_regressions(
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
    print(f"quality: {data['status']}; {output / 'report.md'}")
    return 0 if data["status"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
