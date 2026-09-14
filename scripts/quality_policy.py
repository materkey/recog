"""Project policy over pinned tool results; no source metrics are computed here."""

import difflib
import math


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"Expected a numeric metric: {value!r}")
    if not math.isfinite(value) or value < 0:
        raise ValueError(f"Invalid metric: {value!r}")
    return value


def identity(row):
    return f"{row['path']}::{row['key_name']}"


def score_index(rows):
    if not isinstance(rows, list) or not rows:
        raise ValueError("Missing CRAP inventory")
    result = {}
    for row in rows:
        key = identity(row)
        if key in result or row["flag"] != "measured":
            raise ValueError(f"Ambiguous or unmeasured function: {key}")
        number(row["crap"])
        if not 0 <= number(row["cov"]) <= 1:
            raise ValueError(f"Invalid function coverage: {key}")
        result[key] = row
    return result


def crap_regressions(baseline, head):
    previous, current = score_index(baseline), score_index(head)
    failures = []
    for key, row in current.items():
        old = previous.get(key)
        delta = row["crap"] - old["crap"] if old else None
        if row["crap"] > 15 and (old is None or delta > 1):
            failures.append(
                {
                    "kind": "crap",
                    "function": key,
                    "baseline": old["crap"] if old else None,
                    "head": row["crap"],
                    "delta": delta,
                }
            )
    return failures


def unchanged_lines(before, after):
    matcher = difflib.SequenceMatcher(a=before.splitlines(), b=after.splitlines(), autojunk=False)
    return {
        new + offset + 1: old + offset + 1
        for old, new, size in matcher.get_matching_blocks()
        for offset in range(size)
    }


def lint_regressions(baseline, head, mapping):
    def key(row, line):
        return row["code"], row["message"], line, row["location"]["column"]

    validate_lint(baseline)
    validate_lint(head)
    old = {key(row, row["location"]["row"]) for row in baseline}
    return [row for row in head if key(row, mapping.get(row["location"]["row"])) not in old]


def format_regressions(source, formatted, mapping):
    changes = difflib.SequenceMatcher(
        a=source.splitlines(), b=formatted.splitlines(), autojunk=False
    )
    return [
        start + 1
        for tag, start, end, _, _ in changes.get_opcodes()
        if tag != "equal"
        and any(line not in mapping for line in range(start + 1, max(start + 2, end + 1)))
    ]


def pair_key(pair):
    functions = pair["functions"]
    if len(functions) != 2 or not 0 <= number(pair["similarity"]) <= 1:
        raise ValueError("Malformed duplicate candidate")
    return tuple(sorted(identity(row) for row in functions))


def duplicate_regressions(baseline, head, exceptions):
    old = {pair_key(pair) for pair in baseline}
    allowed = {}
    for entry in exceptions["duplicates"]:
        key = tuple(sorted(entry["functions"]))
        if len(key) != 2 or not entry["reason"].strip() or key in allowed:
            raise ValueError("Duplicate exceptions need an exact pair and a review reason")
        allowed[key] = entry["reason"]
    return [
        {"kind": "duplicate", **pair}
        for pair in head
        if pair_key(pair) not in old and pair_key(pair) not in allowed
    ]


def validate_lint(rows):
    if not isinstance(rows, list):
        raise ValueError("Ruff findings must be an array")
    for row in rows:
        if not isinstance(row["code"], str) or not isinstance(row["message"], str):
            raise ValueError("Malformed Ruff diagnostic")
        for coordinate in ("row", "column"):
            value = row["location"][coordinate]
            if type(value) is not int or value < 1:
                raise ValueError("Malformed Ruff location")
    return rows


def validate_region(region):
    summary = region["summary"]
    for total, covered, missing, executed_field, missing_field in (
        ("num_statements", "covered_lines", "missing_lines", "executed_lines", "missing_lines"),
        (
            "num_branches",
            "covered_branches",
            "missing_branches",
            "executed_branches",
            "missing_branches",
        ),
    ):
        counts = [summary[key] for key in (total, covered, missing)]
        if any(type(value) is not int or value < 0 for value in counts):
            raise ValueError("Invalid coverage counts")
        if counts[1] + counts[2] != counts[0]:
            raise ValueError("Inconsistent coverage counts")
        for field, count in ((executed_field, counts[1]), (missing_field, counts[2])):
            entries = region[field]
            if not isinstance(entries, list) or len(entries) != count:
                raise ValueError("Missing or inconsistent coverage lines/branches")
            for entry in entries:
                values = entry if "branches" in field else [entry]
                if not isinstance(values, list) or any(type(value) is not int for value in values):
                    raise ValueError("Invalid coverage location")
                if "branches" in field and len(values) != 2:
                    raise ValueError("Invalid coverage arc")
