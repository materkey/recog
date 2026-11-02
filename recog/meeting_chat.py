"""A saved dialog about one meeting: its file and the prompt of the next question.

One line of ``chat.jsonl`` holds a whole exchange, so a question without an answer
cannot be stored: a failed model call leaves no trace and can simply be asked again.
Each line is one append, which lets several browser tabs ask at the same time.
"""

import json
import os
from dataclasses import asdict, dataclass
from pathlib import Path

CHAT_FILE = "chat.jsonl"
PROMPT_HISTORY_TURNS = 8


@dataclass(frozen=True)
class ChatTurn:
    asked_at: str
    question: str
    answer: str
    provider: str
    settings_revision: str
    transcript_truncated: bool


def _parse_turn(line: str) -> ChatTurn | None:
    try:
        raw = json.loads(line)
        turn = ChatTurn(**raw)
    except (ValueError, TypeError):
        return None
    return turn if isinstance(turn.question, str) and isinstance(turn.answer, str) else None


def read_turns(meeting_path: Path) -> list[ChatTurn]:
    """Read the dialog, skipping lines damaged by a crash or by hand."""
    try:
        text = (meeting_path / CHAT_FILE).read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        return []
    turns = (_parse_turn(line) for line in text.splitlines())
    return [turn for turn in turns if turn is not None]


def append_turn(meeting_path: Path, turn: ChatTurn) -> None:
    """Append one exchange. Raises FileNotFoundError if the meeting has been deleted."""
    line = json.dumps(asdict(turn), ensure_ascii=False) + "\n"
    fd = os.open(meeting_path / CHAT_FILE, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, line.encode("utf-8"))
    finally:
        os.close(fd)


def clear_turns(meeting_path: Path) -> None:
    (meeting_path / CHAT_FILE).unlink(missing_ok=True)


def build_chat_prompt(instructions: str, history: list[ChatTurn], question: str) -> str:
    """Add the dialog and the question after the editable instructions.

    They are not placeholders of the prompt: a question must reach the model whatever
    the user has done to the prompt in the settings.
    """
    parts = [instructions.rstrip()]
    recent = history[-PROMPT_HISTORY_TURNS:]
    if recent:
        exchanges = "\n\n".join(f"Вопрос: {turn.question}\nОтвет: {turn.answer}" for turn in recent)
        parts.append(f"Предыдущие вопросы и ответы этого разговора:\n\n{exchanges}")
    parts.append(f"Вопрос пользователя:\n{question}")
    return "\n\n".join(parts)
