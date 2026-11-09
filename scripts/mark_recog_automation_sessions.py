#!/usr/bin/env python3
"""Mark historical Recog-generated CLI sessions as automation for ccs."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import tempfile
from collections import Counter
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent

# The marker recog's AI calls put on their prompts (crates/recog-ai, provider.rs; once
# recog/ai_provider.py).
CCS_AUTOMATION_MARKER_PREFIX = "<!-- ccs-automation:recog"


def _sanitize_ccs_task(task: str) -> str:
    normalized = (task or "analysis").strip().lower()
    safe = "".join(ch if ch.isalnum() or ch in "._:-" else "-" for ch in normalized).strip("-")
    return safe or "analysis"


def mark_ccs_automation(prompt: str, task: str = "analysis") -> str:
    """Mark a Recog-generated prompt so session search can classify it as automation."""
    if prompt.lstrip().startswith(CCS_AUTOMATION_MARKER_PREFIX):
        return prompt
    return f"{CCS_AUTOMATION_MARKER_PREFIX} task={_sanitize_ccs_task(task)} -->\n{prompt}"

RECOG_PROJECT_PREFIX = str(REPO_ROOT)
CODEX_DEFAULT_ROOT = Path.home() / ".codex" / "sessions"
CLAUDE_DEFAULT_ROOT = Path.home() / ".claude" / "projects"
CLAUDE_PROJECT_PREFIX = re.sub(r"[^a-zA-Z0-9]", "-", str(REPO_ROOT))
BACKUP_DEFAULT_ROOT = Path.home() / ".ccs-recog-automation-backups"


@dataclass
class RewriteResult:
    candidate: bool = False
    changed: bool = False
    tasks: list[str] = field(default_factory=list)
    text: str | None = None


@dataclass
class RunSummary:
    scanned: int = 0
    candidates: int = 0
    changed: int = 0
    tasks: Counter[str] = field(default_factory=Counter)
    changed_paths: list[Path] = field(default_factory=list)

    def add(self, path: Path, result: RewriteResult) -> None:
        self.scanned += 1
        if result.candidate:
            self.candidates += 1
        if result.changed:
            self.changed += 1
            self.changed_paths.append(path)
            self.tasks.update(result.tasks)


def is_marked(text: str) -> bool:
    return text.lstrip().startswith(CCS_AUTOMATION_MARKER_PREFIX)


def is_bootstrap_text(text: str) -> bool:
    stripped = text.lstrip()
    return (
        stripped.startswith("<environment_context>")
        or stripped.startswith("# AGENTS.md instructions for ")
    )


def detect_task(text: str) -> str | None:
    if "Придумай короткое название (3-7 слов)" in text:
        return "title"
    if "Какой инкремент" in text:
        return "increment"
    if "быстрого восстановления контекста встречи" in text:
        return "anchor"
    if (
        "Дай краткое summary" in text
        or "Дай краткое изложение того, что обсуждалось на встрече" in text
        or "Дай полное summary встречи." in text
        or "Контекст встречи (summary):" in text
        or "Ты помощник Вячеслава Ковалева на рабочей встрече." in text
    ):
        return "summary"
    return None


def codex_session_meta(objects: list[dict[str, Any] | None]) -> dict[str, Any] | None:
    for obj in objects:
        if not isinstance(obj, dict):
            continue
        if obj.get("type") == "session_meta":
            payload = obj.get("payload")
            if isinstance(payload, dict):
                return payload
    return None


def is_recog_codex_exec(meta: dict[str, Any] | None) -> bool:
    if not meta:
        return False
    cwd = str(meta.get("cwd") or "")
    source = str(meta.get("source") or "")
    originator = str(meta.get("originator") or "")
    return cwd.startswith(RECOG_PROJECT_PREFIX) and (
        source == "exec" or originator == "codex_exec"
    )


def parse_jsonl(text: str) -> list[tuple[str, dict[str, Any] | None]]:
    parsed = []
    for line in text.splitlines():
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            obj = None
        parsed.append((line, obj))
    return parsed


def dump_jsonl_line(obj: dict[str, Any]) -> str:
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def mark_codex_text(text: str) -> RewriteResult:
    parsed = parse_jsonl(text)
    objects = [obj for _, obj in parsed]
    if not is_recog_codex_exec(codex_session_meta(objects)):
        return RewriteResult()

    result = RewriteResult(candidate=True)
    new_lines: list[str] = []
    selected_task: str | None = None
    marked_response_item = False
    marked_event_msg = False

    for original_line, obj in parsed:
        changed_line = False

        if isinstance(obj, dict) and obj.get("type") == "response_item":
            payload = obj.get("payload")
            if isinstance(payload, dict) and payload.get("type") == "message":
                if payload.get("role") == "user":
                    content = payload.get("content")
                    if isinstance(content, list):
                        for block in content:
                            if not isinstance(block, dict):
                                continue
                            if block.get("type") != "input_text":
                                continue
                            prompt = block.get("text")
                            if not isinstance(prompt, str):
                                continue
                            if is_bootstrap_text(prompt):
                                continue
                            task = detect_task(prompt) or "codex-exec"
                            selected_task = selected_task or task
                            if not is_marked(prompt) and not marked_response_item:
                                block["text"] = mark_ccs_automation(prompt, task)
                                result.tasks.append(task)
                                result.changed = True
                                changed_line = True
                            marked_response_item = True
                            break

        if isinstance(obj, dict) and obj.get("type") == "event_msg":
            payload = obj.get("payload")
            if isinstance(payload, dict) and payload.get("type") == "user_message":
                prompt = payload.get("message")
                if isinstance(prompt, str) and not is_bootstrap_text(prompt):
                    task = detect_task(prompt) or selected_task or "codex-exec"
                    selected_task = selected_task or task
                    if not is_marked(prompt) and not marked_event_msg:
                        payload["message"] = mark_ccs_automation(prompt, task)
                        if not result.tasks:
                            result.tasks.append(task)
                        result.changed = True
                        changed_line = True
                    marked_event_msg = True

        new_lines.append(dump_jsonl_line(obj) if changed_line and obj else original_line)

    if result.changed:
        result.text = "\n".join(new_lines) + ("\n" if text.endswith("\n") else "")
    return result


def should_scan_claude_path(path: Path) -> bool:
    path_s = str(path)
    return (
        "/subagents/" not in path_s
        and not path.name.startswith("agent-")
        and path.parent.name.startswith(CLAUDE_PROJECT_PREFIX)
    )


def mark_claude_content(content: Any) -> tuple[Any, list[str], bool]:
    tasks: list[str] = []
    changed = False

    if isinstance(content, str):
        task = detect_task(content)
        if task and not is_marked(content):
            return mark_ccs_automation(content, task), [task], True
        return content, tasks, False

    if isinstance(content, list):
        for block in content:
            if not isinstance(block, dict):
                continue
            text = block.get("text")
            if not isinstance(text, str):
                continue
            task = detect_task(text)
            if task and not is_marked(text):
                block["text"] = mark_ccs_automation(text, task)
                tasks.append(task)
                changed = True
        return content, tasks, changed

    return content, tasks, False


def mark_claude_text(text: str) -> RewriteResult:
    parsed = parse_jsonl(text)
    result = RewriteResult()
    new_lines: list[str] = []

    for original_line, obj in parsed:
        changed_line = False
        if isinstance(obj, dict) and obj.get("type") == "user":
            message = obj.get("message")
            if isinstance(message, dict):
                content, tasks, changed = mark_claude_content(message.get("content"))
                if tasks:
                    result.candidate = True
                if changed:
                    message["content"] = content
                    result.tasks.extend(tasks)
                    result.changed = True
                    changed_line = True
        new_lines.append(dump_jsonl_line(obj) if changed_line and obj else original_line)

    if result.changed:
        result.text = "\n".join(new_lines) + ("\n" if text.endswith("\n") else "")
    return result


def atomic_write(path: Path, text: str) -> None:
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    tmp_path = Path(tmp_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(text)
        shutil.copystat(path, tmp_path)
        os.replace(tmp_path, path)
    finally:
        if tmp_path.exists():
            tmp_path.unlink()


def backup_file(path: Path, backup_root: Path) -> None:
    try:
        rel = path.relative_to(Path.home())
    except ValueError:
        rel = Path(path.name)
    dest = backup_root / rel
    dest.parent.mkdir(parents=True, exist_ok=True)
    if not dest.exists():
        shutil.copy2(path, dest)


def process_file(
    path: Path,
    rewriter,
    *,
    apply: bool,
    backup_root: Path | None,
) -> RewriteResult:
    text = path.read_text(encoding="utf-8")
    result = rewriter(text)
    if apply and result.changed and result.text is not None:
        if backup_root is None:
            raise ValueError("backup_root is required when apply=True")
        backup_file(path, backup_root)
        atomic_write(path, result.text)
    return result


def iter_codex_files(root: Path):
    if root.exists():
        yield from root.rglob("*.jsonl")


def iter_claude_files(root: Path):
    if root.exists():
        for path in root.glob(f"{CLAUDE_PROJECT_PREFIX}*/*.jsonl"):
            if should_scan_claude_path(path):
                yield path


def run(
    *,
    codex_root: Path,
    claude_root: Path,
    apply: bool,
    backup_root: Path | None,
) -> RunSummary:
    summary = RunSummary()
    for path in iter_codex_files(codex_root):
        result = process_file(
            path,
            mark_codex_text,
            apply=apply,
            backup_root=backup_root,
        )
        summary.add(path, result)
    for path in iter_claude_files(claude_root):
        result = process_file(
            path,
            mark_claude_text,
            apply=apply,
            backup_root=backup_root,
        )
        summary.add(path, result)
    return summary


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true", help="write changes to session files")
    parser.add_argument("--codex-root", type=Path, default=CODEX_DEFAULT_ROOT)
    parser.add_argument("--claude-root", type=Path, default=CLAUDE_DEFAULT_ROOT)
    parser.add_argument("--backup-root", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    backup_root = args.backup_root
    if args.apply and backup_root is None:
        stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
        backup_root = BACKUP_DEFAULT_ROOT / stamp

    summary = run(
        codex_root=args.codex_root.expanduser(),
        claude_root=args.claude_root.expanduser(),
        apply=args.apply,
        backup_root=backup_root.expanduser() if backup_root else None,
    )

    mode = "apply" if args.apply else "dry-run"
    print(f"mode: {mode}")
    print(f"scanned: {summary.scanned}")
    print(f"candidates: {summary.candidates}")
    print(f"changed: {summary.changed}")
    if summary.tasks:
        print("tasks:")
        for task, count in sorted(summary.tasks.items()):
            print(f"  {task}: {count}")
    if args.apply:
        print(f"backup_root: {backup_root}")
    else:
        print("dry-run only; rerun with --apply to write changes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
