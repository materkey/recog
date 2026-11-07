"""Replay of a golden session into the Python live process.

Design: docs/plans/2026-11-07-golden-sessions-design.md. The real ``LiveTranscriber``
runs with its real bridge, ingest and storage, in a data directory of its own and on
a free port, while

- the microphone and system captures hand out the recorded chunks at their recorded
  times, their capture times moved to now;
- ASR answers from the recording, looked up by the exact input;
- the extension's frames reach the bridge over a WebSocket at their recorded times,
  one connection per recorded client, so its clock sync moves them to now as well;
- nothing calls an LLM: meeting titles and summaries are switched off.

It replays in real time: the live process sleeps and polls on the wall clock. The
outputs land under ``<out>/data/meetings``; ``compare`` sets them against the meeting
the session was recorded in.

    python -m recog.golden_replay replay <session> <out>
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import threading
import time
from collections import deque
from pathlib import Path
from typing import Any, Optional

import numpy as np

from .audio_capture import BufferedAudioCapture
from .golden import audio_key, read_session
from .wavo_backend import Result, WavoASR, Word

# How long the live process runs on after the last recorded event, so that the
# last chunk, batch and refine get through.
TAIL_S = 8.0


class ReplayCapture(BufferedAudioCapture):
    """A capture that hands out the recorded chunks, one per ``get_chunk``."""

    def __init__(self, name: str, sample_rate: int = 16000):
        super().__init__(sample_rate, name)
        self._chunks: deque = deque()

    def start(self):
        self.running = True

    def stop(self):
        self.running = False

    def feed(self, chunk: np.ndarray, start_ts: float) -> None:
        with self._lock:
            self._chunks.append((chunk, start_ts))

    def get_chunk(self, duration: float):
        with self._lock:
            return self._chunks.popleft() if self._chunks else None

    def request_restart(self, reason: str) -> None:
        pass


class ReplayASR(WavoASR):
    """Answers each ASR call from the recording by the exact input; counts misses."""

    def __init__(self, events):
        super().__init__()
        self.answers: dict[str, Any] = {}
        for event in events:
            if event["kind"] != "asr":
                continue
            if event["call"] == "transcribe_batch":
                for key, text in zip(event["inputs"], event["output"]):
                    self.answers[key] = text
            else:
                self.answers[event["inputs"][0]] = event["output"]
        self.misses: list[str] = []

    def load_model(self, quiet: bool = False):
        return None

    def _answer(self, audio, call: str):
        key = audio_key(np.asarray(audio))
        if key not in self.answers:
            self.misses.append(call)
            return None
        return self.answers[key]

    def transcribe_chunk(self, audio, sample_rate: int = 16000) -> Optional[str]:
        text = self._answer(audio, "transcribe_chunk")
        return text or None

    def transcribe_batch(self, pcm_list):
        return [self._answer(pcm, "transcribe_batch") or "" for pcm in pcm_list]

    def transcribe_with_timestamps(self, audio, sample_rate: int = 16000, quiet: bool = False):
        answer = self._answer(audio, "transcribe_with_timestamps") or {"text": "", "words": []}
        return Result(answer["text"], [Word(*w) for w in answer["words"]])


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _isolate(data_dir: Path, port: int, captures: dict, asr: ReplayASR) -> None:
    """Points this process's recog at the replay's data, port, captures and ASR."""
    os.environ["RECOG_DATA_DIR"] = str(data_dir)
    from . import increment, live, meeting_storage, system_audio
    from . import ktalk_bridge as bridge_module
    from .install_config import INSTALL_CONFIG

    INSTALL_CONFIG.live.golden_record = False
    INSTALL_CONFIG.live.echo_cancel = False
    live.create_mic_capture = lambda *a, **k: captures["mic"]
    system_audio.create_system_capture = lambda *a, **k: captures["system"]
    live.create_echo_stage = lambda *a, **k: None
    live.build_asr = lambda: asr

    class _Bridge(bridge_module.KTalkBridge):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, port=port, **kwargs)

    bridge_module.KTalkBridge = _Bridge

    async def _no_summary(self, *args, **kwargs):
        return None

    increment.IncrementGenerator.generate_all = _no_summary
    meeting_storage.MeetingStorage._generate_title_for_meeting = lambda self, path: None


def _drive(events: list[dict], captures: dict, port: int, transcriber, start: float) -> None:
    """Sends the recording into the running live process at its recorded times."""
    from websockets.sync.client import connect

    first_wall = events[0]["wall"]
    shift = start - first_wall
    clients: dict[int, Any] = {}
    _wait_for_bridge(port)
    for event in events:
        delay = start + event["t"] - events[0]["t"] - time.time()
        if delay > 0:
            time.sleep(delay)
        kind = event["kind"]
        if kind == "connect":
            clients[event["client"]] = connect(f"ws://127.0.0.1:{port}")
        elif kind == "disconnect":
            ws = clients.pop(event["client"], None)
            if ws is not None:
                ws.close()
        elif kind == "frame" and event["client"] in clients:
            clients[event["client"]].send(json.dumps(event["data"], ensure_ascii=False))
        elif kind == "capture":
            captures[event["source"]].feed(event["audio"], event["start_ts"] + shift)
    time.sleep(TAIL_S)
    for ws in clients.values():
        ws.close()
    transcriber.running = False


def _wait_for_bridge(port: int, timeout_s: float = 30.0) -> None:
    """Until the replayed bridge accepts a WebSocket: a bare TCP probe would make it
    log a failed handshake."""
    from websockets.sync.client import connect

    deadline = time.time() + timeout_s
    while True:
        try:
            connect(f"ws://127.0.0.1:{port}", open_timeout=1).close()
            return
        except OSError:
            if time.time() > deadline:
                raise RuntimeError("the replayed bridge did not start") from None
            time.sleep(0.05)


def replay(session: Path, out: Path, profile: Optional[Path] = None) -> dict:
    """Replays ``session`` into a live process whose data goes to ``out/data``."""
    events = list(read_session(session))
    if not events:
        raise ValueError(f"{session} holds no events")
    data_dir = Path(out) / "data"
    data_dir.mkdir(parents=True, exist_ok=True)
    if profile is not None and profile.exists():
        (data_dir / profile.name).write_bytes(profile.read_bytes())
    captures = {"mic": ReplayCapture("mic"), "system": ReplayCapture("system")}
    asr = ReplayASR(events)
    port = _free_port()
    _isolate(data_dir, port, captures, asr)
    from .live import LiveTranscriber

    # The system backend only picks the code path; the captures are the replay's.
    backend = "screencapture" if sys.platform == "darwin" else "pulse"
    transcriber = LiveTranscriber(
        mic_device="replay", system_audio=backend, use_ktalk=True, save_audio=False
    )
    start = time.time()
    driver = threading.Thread(
        target=_drive, args=(events, captures, port, transcriber, start), daemon=True
    )
    driver.start()
    transcriber.run()
    summary = {
        "session": str(session),
        "start_wall": start,
        "recorded_start_wall": events[0]["wall"],
        "events": len(events),
        "asr_misses": len(asr.misses),
    }
    (Path(out) / "replay.json").write_text(json.dumps(summary, indent=1))
    return summary


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="python -m recog.golden_replay")
    sub = parser.add_subparsers(dest="command", required=True)
    r = sub.add_parser("replay", help="replay a golden session into the Python live process")
    r.add_argument("session", type=Path)
    r.add_argument("out", type=Path)
    r.add_argument("--profile", type=Path, help="user-profile.json to replay with")
    args = parser.parse_args(argv)
    summary = replay(args.session, args.out, args.profile)
    print(json.dumps(summary, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
