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

``compare-replays <session> <first> <second>`` sets two replays of one session
against each other: the Python one and the Rust one (``rust/target/release/recog
replay <session> <out>``, the same player in the Rust live process).
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
from datetime import datetime
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


def _connect(port: int):
    """A client of the replayed bridge whose broadcasts are read and dropped.

    Unread, they fill the client's queue (16 frames), the client stops reading the
    socket, the bridge's pong to its keepalive ping is never seen, and the connection
    dies with "keepalive ping timeout" a few minutes into a real session.
    """
    from websockets.exceptions import ConnectionClosed
    from websockets.sync.client import connect

    ws = connect(f"ws://127.0.0.1:{port}")

    def drain():
        try:
            for _ in ws:
                pass
        except ConnectionClosed:
            pass

    threading.Thread(target=drain, daemon=True).start()
    return ws


def _drive(events: list[dict], captures: dict, port: int, transcriber, start: float) -> None:
    """Sends the recording into the running live process at its recorded times."""
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
            clients[event["client"]] = _connect(port)
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


# Fields that make two transcript rows the same line; ids, offsets and the exact
# time differ between runs and are not compared.
ROW_KEY = ("speaker", "is_local", "text", "refined", "uncertain")
# The same for a marker (a system row: the PCM watchdog, the system capture).
MARKER_KEY = ("speaker", "text", "uncertain_reason")
# How far apart in time the same line may land in the two runs, in seconds.
TOLERANCE_S = 1.5


def _rows_between(
    meetings: Path, start: float, end: float, shift: float, markers: bool = False
) -> list[dict]:
    """Speech rows of every meeting under ``meetings`` with ``ts - shift`` in [start, end]
    (with ``markers``, the system markers instead, keyed by ``MARKER_KEY``)."""
    rows = []
    keys = MARKER_KEY if markers else ROW_KEY
    for path in sorted(Path(meetings).glob("*/transcript.jsonl")):
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            # Markers: the replay's own pause, etc.
            if (row.get("speaker_id") == "__system__") != markers:
                continue
            t = datetime.fromisoformat(row["ts"]).timestamp() - shift
            if start <= t <= end:
                rows.append({"t": round(t, 3), **{k: row.get(k) for k in keys}})
    return sorted(rows, key=lambda r: r["t"])


def _match(before: list[dict], after: list[dict], keys=ROW_KEY) -> tuple[list, list]:
    """The rows of ``before`` with no twin in ``after`` (same ``keys``, within
    ``TOLERANCE_S``), and the rows of ``after`` left without one."""
    unmatched = list(after)
    only_before = []
    for row in before:
        twin = next(
            (
                r
                for r in unmatched
                if all(r[k] == row[k] for k in keys) and abs(r["t"] - row["t"]) <= TOLERANCE_S
            ),
            None,
        )
        if twin is None:
            only_before.append(row)
        else:
            unmatched.remove(twin)
    return only_before, unmatched


def _recorded_audio_span(events) -> tuple[float, float]:
    """Wall times from the first to the last recorded audio: captures (from their
    capture time) and PCM frames. Rows outside came from audio the recorder left out,
    the microphone and system audio between conferences."""
    times = []
    for event in events:
        if event["kind"] == "capture":
            times += [event["start_ts"], event["wall"]]
        elif event["kind"] == "frame" and event["data"].get("type") == "pcm_chunk":
            times.append(event["wall"])
    if not times:
        return 0.0, -1.0
    return min(times) - TOLERANCE_S, max(times) + TOLERANCE_S


def compare(session: Path, original: Path, out: Path) -> dict:
    """The meeting rows the live service wrote while ``session`` was recorded, under
    ``original`` (a meetings directory), against the rows of its replay in ``out``.

    Rows match on ``ROW_KEY`` within ``TOLERANCE_S``; what does not match on either
    side is listed with its time from the recorded clock.
    """
    summary = json.loads((Path(out) / "replay.json").read_text())
    start, end = _recorded_audio_span(read_session(session))
    shift = summary["start_wall"] - summary["recorded_start_wall"]
    before = _rows_between(original, start, end, 0.0)
    after = _rows_between(Path(out) / "data" / "meetings", start, end, shift)
    only_original, unmatched = _match(before, after)
    return {
        "matched": len(before) - len(only_original),
        "only_original": only_original,
        "only_replay": unmatched,
    }


# meta.json fields two replays of one session must agree on; ids, times and paths
# differ between runs.
META_KEYS = (
    "version",
    "source",
    "end_reason",
    "mode",
    "chunk_duration_ms",
    "conference",
    "title",
    "title_source",
    "participants",
    "recording_active",
)
META_STATS = ("segment_count", "words_count")


def _metas(meetings: Path) -> list[dict]:
    """The comparable part of every meeting's meta.json, in creation order."""
    metas = []
    for path in Path(meetings).glob("*/meta.json"):
        meta = json.loads(path.read_text(encoding="utf-8"))
        # When a participant was first and last seen is wall-clock time.
        participants = {
            pid: {k: v for k, v in info.items() if not k.endswith("_seen")}
            for pid, info in (meta.get("participants") or {}).items()
        }
        metas.append(
            {
                "created_at": meta.get("created_at"),
                **{k: meta.get(k) for k in META_KEYS},
                "participants": participants,
                "stats": {k: meta.get("stats", {}).get(k) for k in META_STATS},
                "prev": meta.get("prev_meeting_id") is not None,
                "next": meta.get("next_meeting_id") is not None,
            }
        )
    metas.sort(key=lambda m: m["created_at"] or "")
    for meta in metas:
        del meta["created_at"]
    return metas


def compare_replays(session: Path, first: Path, second: Path) -> dict:
    """Two replays of ``session`` (say the Python and the Rust live process) against
    each other: speech rows as ``compare`` matches them, the markers on their own, and
    the comparable fields of every meeting's meta.json."""
    start, end = _recorded_audio_span(read_session(session))

    def rows(out: Path, markers: bool) -> list[dict]:
        summary = json.loads((Path(out) / "replay.json").read_text())
        shift = summary["start_wall"] - summary["recorded_start_wall"]
        # Markers are stamped when they are written, not with a capture time.
        lo, hi = (float("-inf"), float("inf")) if markers else (start, end)
        return _rows_between(Path(out) / "data" / "meetings", lo, hi, shift, markers)

    a, b = rows(first, False), rows(second, False)
    only_first, only_second = _match(a, b)
    ma, mb = rows(first, True), rows(second, True)
    markers_first, markers_second = _match(ma, mb, MARKER_KEY)
    metas_first = _metas(Path(first) / "data" / "meetings")
    metas_second = _metas(Path(second) / "data" / "meetings")
    misses = [
        json.loads((Path(out) / "replay.json").read_text())["asr_misses"] for out in (first, second)
    ]
    return {
        "matched": len(a) - len(only_first),
        "only_first": only_first,
        "only_second": only_second,
        "markers_matched": len(ma) - len(markers_first),
        "markers_only_first": markers_first,
        "markers_only_second": markers_second,
        "meta_equal": metas_first == metas_second,
        "meta_first": metas_first,
        "meta_second": metas_second,
        "asr_misses": misses,
    }


def replays_agree(result: dict) -> bool:
    """Whether ``compare_replays`` found the two replays the same."""
    return result["meta_equal"] and not any(
        result[k]
        for k in (
            "only_first",
            "only_second",
            "markers_only_first",
            "markers_only_second",
        )
    )


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="python -m recog.golden_replay")
    sub = parser.add_subparsers(dest="command", required=True)
    r = sub.add_parser("replay", help="replay a golden session into the Python live process")
    r.add_argument("session", type=Path)
    r.add_argument("out", type=Path)
    r.add_argument("--profile", type=Path, help="user-profile.json to replay with")
    c = sub.add_parser(
        "compare", help="compare a replay with the meeting the session was recorded in"
    )
    c.add_argument("session", type=Path)
    c.add_argument("out", type=Path, help="the replay's output directory")
    c.add_argument("--meetings", type=Path, help="default: <data dir>/meetings")
    cr = sub.add_parser(
        "compare-replays",
        help="compare two replays of a session (the Python and the Rust live process)",
    )
    cr.add_argument("session", type=Path)
    cr.add_argument("first", type=Path, help="the first replay's output directory")
    cr.add_argument("second", type=Path, help="the second replay's output directory")
    args = parser.parse_args(argv)
    if args.command == "replay":
        print(json.dumps(replay(args.session, args.out, args.profile), indent=1))
        return 0
    if args.command == "compare-replays":
        result = compare_replays(args.session, args.first, args.second)
        print(json.dumps(result, ensure_ascii=False, indent=1))
        return 0 if replays_agree(result) else 1
    from .config import get_data_dir

    result = compare(args.session, args.meetings or get_data_dir() / "meetings", args.out)
    print(json.dumps(result, ensure_ascii=False, indent=1))
    return 0 if not result["only_original"] and not result["only_replay"] else 1


if __name__ == "__main__":
    sys.exit(main())
