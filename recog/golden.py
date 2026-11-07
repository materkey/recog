"""Golden sessions: one live process's inputs and ASR answers, recorded for replay.

Design: docs/plans/2026-11-07-golden-sessions-design.md. A session directory holds

- ``events.jsonl``: one event per line, in the order they happened, each with ``t``
  (seconds since the recording started, monotonic) and ``wall`` (``time.time()``);
- ``audio.bin``: the samples of ``capture`` events, appended as they come, each
  event naming its dtype, offset and length in bytes.

Events: ``connect``/``disconnect`` of a bridge client (numbered per session, not by
address), ``frame`` (a parsed bridge message as received, ``pcm_chunk`` frames with
their PCM), ``capture`` (a microphone or system-audio chunk as ``get_chunk`` gave it,
with its capture time) and ``asr`` (a call's input keys and its answer). The
recording holds personal audio and text: it stays under the data directory.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import sys
import threading
import time
from pathlib import Path
from typing import Any, Callable, Iterator, Sequence, Union

import numpy as np

EVENTS = "events.jsonl"
AUDIO = "audio.bin"


def audio_key(audio: np.ndarray) -> str:
    """What an ASR call was given: its dtype and samples, hashed."""
    arr = np.ascontiguousarray(audio)
    return hashlib.sha1(arr.dtype.str.encode() + arr.tobytes()).hexdigest()


def _plain(output: Any) -> Any:
    """An ASR answer as JSON: text, a list of texts, or text with words in ms."""
    if output is None or isinstance(output, (str, list)):
        return output
    words = getattr(output, "words", None) or []
    return {
        "text": getattr(output, "text", ""),
        "words": [[w.text, int(w.start), int(w.end)] for w in words],
    }


class GoldenRecorder:
    """Appends events to a session directory; safe to call from any thread."""

    def __init__(
        self,
        directory: Path,
        *,
        clock: Callable[[], float] = time.monotonic,
        wall: Callable[[], float] = time.time,
    ):
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)
        self._clock, self._wall = clock, wall
        self._start = clock()
        self._lock = threading.Lock()
        self._events = open(self.directory / EVENTS, "a", encoding="utf-8")
        self._audio = open(self.directory / AUDIO, "ab")

    def _write(self, kind: str, fields: dict, audio: np.ndarray | None = None) -> None:
        with self._lock:
            if self._events.closed:
                return
            event = {"t": round(self._clock() - self._start, 6), "wall": self._wall(), "kind": kind}
            try:
                if audio is not None:
                    data = np.ascontiguousarray(audio).tobytes()
                    event.update(dtype=audio.dtype.str, offset=self._audio.tell(), nbytes=len(data))
                    self._audio.write(data)
                event.update(fields)
                self._events.write(json.dumps(event, ensure_ascii=False) + "\n")
            except OSError as error:
                # A recording must never cost the live process a frame or a chunk.
                print(f"[golden] golden recording stopped: {error}", file=sys.stderr)
                self._close_locked()

    def _close_locked(self) -> None:
        for f in (self._events, self._audio):
            try:
                f.close()
            except OSError:
                pass

    def client_connected(self, client: int) -> None:
        self._write("connect", {"client": client})

    def client_disconnected(self, client: int) -> None:
        self._write("disconnect", {"client": client})

    def frame(self, client: int, data: dict) -> None:
        self._write("frame", {"client": client, "data": data})

    def capture(self, source: str, chunk: np.ndarray, start_ts: float) -> None:
        self._write("capture", {"source": source, "start_ts": start_ts}, audio=chunk)

    def asr(self, call: str, audio: Union[np.ndarray, Sequence[np.ndarray]], output: Any) -> None:
        clips = [audio] if isinstance(audio, np.ndarray) else list(audio)
        self._write(
            "asr", {"call": call, "inputs": [audio_key(c) for c in clips], "output": _plain(output)}
        )

    def close(self) -> None:
        with self._lock:
            self._close_locked()


KEEP_SUFFIX = "-keep"


def remove_old_sessions(root: Path, max_age_days: float, now: float | None = None) -> list[Path]:
    """Deletes the sessions under ``root`` last written more than ``max_age_days`` ago.

    A session whose directory name ends with ``-keep`` is kept whatever its age, and
    only directories holding an ``events.jsonl`` are sessions.
    """
    now = time.time() if now is None else now
    removed = []
    for directory in sorted(Path(root).iterdir()) if Path(root).is_dir() else []:
        events = directory / EVENTS
        if directory.name.endswith(KEEP_SUFFIX) or not events.is_file():
            continue
        if now - events.stat().st_mtime > max_age_days * 86400:
            shutil.rmtree(directory)
            removed.append(directory)
    return removed


def read_session(directory: Path) -> Iterator[dict]:
    """The events of a session in order; ``capture`` events carry their ``audio``."""
    directory = Path(directory)
    with (
        open(directory / AUDIO, "rb") as audio,
        open(directory / EVENTS, encoding="utf-8") as events,
    ):
        for line in events:
            event = json.loads(line)
            if "nbytes" in event:
                audio.seek(event["offset"])
                event["audio"] = np.frombuffer(
                    audio.read(event["nbytes"]), dtype=np.dtype(event["dtype"])
                )
            yield event
