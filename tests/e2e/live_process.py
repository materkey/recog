"""The live process of an e2e run: the Rust `recog` binary as a child process.

The suite drives the program the services run, not Python objects: it starts
`recog live` with its own data directory, waits for its bridge, and afterwards
reads what the run produced from the meeting storage files of that directory.
Nothing here imports the recog package, so the suite needs only its test tools
(pytest, playwright, websocket-client).

The bridge port is fixed in the binary (8766, `BridgeConfig::default()` in
rust/recog-bridge) and the extension connects to that same port
(`WS_URL` in browser-extension/src/state.js), so an e2e run still needs the
installed live service stopped (tests/e2e/conftest.py). For the same reason
the bridge's debug file, `/tmp/ktalk_debug.json`, is the run's own while it
lasts; it is the only place the process reports whether PCM has arrived.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import threading
import time
from collections import deque
from pathlib import Path
from typing import Mapping, Optional

REPO_ROOT = Path(__file__).resolve().parents[2]
# Written by the bridge at most once a second (rust/recog-bridge/src/debug.rs).
DEBUG_FILE = Path("/tmp/ktalk_debug.json")
# Printed once the model is loaded and the bridge listens (session::run).
READY_LINE = "KTalk bridge started"


def find_binary(repo_root: Path = REPO_ROOT) -> Optional[Path]:
    """The `recog` program of the checkout, as recog-core's `install::program`
    picks it: `bin/recog` of an installation with `[install] runtime = "rust"`
    when it is executable, else the release build; None when neither exists."""
    for candidate in (
        repo_root / "bin" / "recog",
        repo_root / "rust" / "target" / "release" / "recog",
    ):
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate
    return None


def live_argv(binary: Path) -> list[str]:
    """`recog live` as the suite ran `LiveTranscriber(system_device=<BlackHole>,
    use_ktalk=True)`: the default microphone, BlackHole found by name for the
    system audio (`device:auto`), speakers from the extension."""
    return [str(binary), "live", "--ktalk", "--system-audio", "device:auto"]


def live_env(data_dir: Path, base: Mapping[str, str] = os.environ) -> dict[str, str]:
    """The environment of the live process: the caller's, with the run's own data
    directory, so its meetings, logs and profile stay out of the installed ones."""
    env = dict(base)
    env["RECOG_DATA_DIR"] = str(data_dir)
    return env


def pcm_ever_seen(debug_file: Path = DEBUG_FILE, since: float = 0.0) -> bool:
    """Whether the bridge's debug file, written at `since` or later, says a PCM
    chunk has reached the per-speaker ingest (`pcm.pcm_ever_seen`)."""
    try:
        state = json.loads(debug_file.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    if not isinstance(state, dict) or float(state.get("timestamp") or 0.0) < since:
        return False
    pcm = state.get("pcm")
    return isinstance(pcm, dict) and pcm.get("pcm_ever_seen") is True


def meeting_dirs(data_dir: Path) -> list[Path]:
    """The meetings under `data_dir`, oldest first (by `created_at` of meta.json)."""
    meetings = data_dir / "meetings"
    if not meetings.is_dir():
        return []
    found: list[tuple[str, str, Path]] = []
    for path in meetings.iterdir():
        try:
            meta = json.loads((path / "meta.json").read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        created = meta.get("created_at") if isinstance(meta, dict) else None
        found.append((str(created or ""), path.name, path))
    return [path for _, _, path in sorted(found)]


def read_segments(meeting_path: Path) -> list[dict]:
    """The rows of a meeting's transcript.jsonl; unreadable lines are skipped."""
    try:
        text = (meeting_path / "transcript.jsonl").read_text(encoding="utf-8")
    except OSError:
        return []
    rows: list[dict] = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict):
            rows.append(row)
    return rows


def transcript_line(row: dict) -> str:
    """A stored row as the live process prints it: a marker of the "Система"
    speaker as `*[…]* *[HH:MM:SS]*`, speech as `**Speaker** *[HH:MM:SS]*: text`."""
    timestamp = row.get("ts_display", "")
    text = row.get("text", "")
    if row.get("speaker_id") == "__system__":
        return f"\n*{text}* *[{timestamp}]*\n"
    return f"\n**{row.get('speaker', '')}** *[{timestamp}]*: {text}\n"


def read_transcript(data_dir: Path) -> tuple[list[str], Optional[Path]]:
    """Every meeting's rows of a run, oldest meeting first, as transcript lines,
    and the meeting the run ended in: the newest one that holds rows."""
    lines: list[str] = []
    last: Optional[Path] = None
    for path in meeting_dirs(data_dir):
        rows = read_segments(path)
        if rows:
            last = path
        lines.extend(transcript_line(row) for row in rows)
    return lines, last


class LiveProcess:
    """`recog live` as a child: started, watched for its ready line, stopped with
    SIGINT (the process's Ctrl+C, which ends the meeting and saves it)."""

    def __init__(self, argv: list[str], env: Mapping[str, str], log_path: Path):
        self.argv = argv
        self.env = dict(env)
        self.log_path = log_path
        self.output_tail: deque[str] = deque(maxlen=200)
        self.started_at = 0.0
        self._proc: Optional[subprocess.Popen] = None
        self._reader: Optional[threading.Thread] = None
        self._ready = threading.Event()

    def start(self, ready_timeout: float) -> Optional[str]:
        """Start the process; None once it is ready, else what went wrong."""
        self.log_path.parent.mkdir(parents=True, exist_ok=True)
        self.started_at = time.time()
        self._proc = subprocess.Popen(
            self.argv,
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        self._reader = threading.Thread(target=self._read_output, daemon=True)
        self._reader.start()
        deadline = time.time() + ready_timeout
        while time.time() < deadline:
            if self._ready.wait(0.2):
                return None
            if self._proc.poll() is not None:
                return f"live process exited with code {self._proc.returncode} before it was ready"
        return f"live process not ready after {ready_timeout:.0f}s"

    def _read_output(self) -> None:
        assert self._proc is not None and self._proc.stdout is not None
        with open(self.log_path, "a", encoding="utf-8") as log:
            for line in self._proc.stdout:
                log.write(line)
                log.flush()
                self.output_tail.append(line.rstrip("\n"))
                print(f"[live] {line}", end="", flush=True)
                if READY_LINE in line:
                    self._ready.set()

    def stop(self, timeout: float) -> Optional[str]:
        """Stop the process; None after a clean exit, else what went wrong."""
        proc = self._proc
        if proc is None:
            return None
        if proc.poll() is None:
            proc.send_signal(signal.SIGINT)
            try:
                proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
                return (
                    f"live process did not stop within {timeout:.0f}s after SIGINT and was killed"
                )
        if self._reader is not None:
            self._reader.join(timeout=5)
        if proc.returncode != 0:
            return f"live process exited with code {proc.returncode}"
        return None
