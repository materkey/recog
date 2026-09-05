"""Tests for historical Recog session marker backfill."""

import importlib.util
import json
import sys
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parent.parent / "scripts" / "mark_recog_automation_sessions.py"
SPEC = importlib.util.spec_from_file_location("mark_recog_automation_sessions", SCRIPT_PATH)
mark_sessions = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = mark_sessions
SPEC.loader.exec_module(mark_sessions)


def test_custom_project_location_when_scanning_then_only_matching_sessions_are_selected(tmp_path, monkeypatch):
    monkeypatch.setattr(mark_sessions, "CLAUDE_PROJECT_PREFIX", "-work-projects-recog")
    expected = tmp_path / "-work-projects-recog" / "session.jsonl"
    other = tmp_path / "-work-projects-other" / "session.jsonl"
    agent = expected.parent / "agent-child.jsonl"
    for path in (expected, other, agent):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("{}\n")
    assert list(mark_sessions.iter_claude_files(tmp_path)) == [expected]


def test_mark_codex_text_marks_recog_exec_prompt_and_event():
    text = "\n".join([
        json.dumps({
            "type": "session_meta",
            "payload": {
                "cwd": mark_sessions.RECOG_PROJECT_PREFIX,
                "source": "exec",
                "originator": "codex_exec",
            },
        }, ensure_ascii=False),
        json.dumps({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "# AGENTS.md instructions for /Users/user/projects/recog\n\n<INSTRUCTIONS>...</INSTRUCTIONS>"}],
            },
        }, ensure_ascii=False),
        json.dumps({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Придумай короткое название (3-7 слов) для этого разговора."}],
            },
        }, ensure_ascii=False),
        json.dumps({
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "Придумай короткое название (3-7 слов) для этого разговора.",
            },
        }, ensure_ascii=False),
    ]) + "\n"

    result = mark_sessions.mark_codex_text(text)

    assert result.candidate is True
    assert result.changed is True
    assert result.tasks == ["title"]
    assert result.text.count("<!-- ccs-automation:recog task=title -->") == 2
    assert "# AGENTS.md instructions" in result.text


def test_mark_codex_text_ignores_interactive_recog_cli_session():
    text = json.dumps({
        "type": "session_meta",
        "payload": {
            "cwd": mark_sessions.RECOG_PROJECT_PREFIX,
            "source": "cli",
            "originator": "codex_cli_rs",
        },
    }, ensure_ascii=False) + "\n"

    result = mark_sessions.mark_codex_text(text)

    assert result.candidate is False
    assert result.changed is False


def test_mark_claude_text_marks_only_known_recog_prompt_family():
    text = "\n".join([
        json.dumps({
            "type": "user",
            "message": {"role": "user", "content": "ручная просьба по проекту"},
        }, ensure_ascii=False),
        json.dumps({
            "type": "user",
            "message": {
                "role": "user",
                "content": "Ты — помощник для быстрого восстановления контекста встречи.\nТранскрипт:",
            },
        }, ensure_ascii=False),
    ]) + "\n"

    result = mark_sessions.mark_claude_text(text)

    assert result.candidate is True
    assert result.changed is True
    assert "<!-- ccs-automation:recog task=anchor -->" in result.text
    assert "ручная просьба по проекту" in result.text
    assert result.text.count("<!-- ccs-automation:recog") == 1


def test_process_file_apply_creates_backup_and_is_idempotent(tmp_path):
    session = tmp_path / "rollout.jsonl"
    session.write_text(json.dumps({
        "type": "session_meta",
        "payload": {
            "cwd": mark_sessions.RECOG_PROJECT_PREFIX,
            "source": "exec",
            "originator": "codex_exec",
        },
    }, ensure_ascii=False) + "\n" + json.dumps({
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Say hello in one word\n"}],
        },
    }, ensure_ascii=False) + "\n", encoding="utf-8")
    backup_root = tmp_path / "backup"

    first = mark_sessions.process_file(
        session,
        mark_sessions.mark_codex_text,
        apply=True,
        backup_root=backup_root,
    )
    second = mark_sessions.process_file(
        session,
        mark_sessions.mark_codex_text,
        apply=True,
        backup_root=backup_root,
    )

    assert first.changed is True
    assert second.changed is False
    assert "<!-- ccs-automation:recog task=codex-exec -->" in session.read_text(encoding="utf-8")
    assert list(backup_root.rglob("rollout.jsonl"))
