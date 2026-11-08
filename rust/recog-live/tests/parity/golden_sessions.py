"""Synthetic golden sessions for tests/replay_parity.rs: the Python side of the live
process's acceptance test (docs/plans/2026-11-07-golden-sessions-design.md).

Run as `python golden_sessions.py <repo root> <command> ...` (stdin is ignored):

- `make <name> <session>` records a session with the Python recorder
  (recog/golden.py): its extension frames, microphone and system captures, no ASR
  answers yet. Nothing in it is real: the "speech" is tones, one pitch per word, the
  names and the room are made up.
- `fill <session> <out> <tail>` replays the session into the Python live process
  (recog.golden_replay.replay) with every ASR call answered by a synthetic model that
  reads the tones back as words (the refine words capitalised, so a refine changes a
  row), then appends those answers to the session as `asr` events. `<out>` is then the
  Python replay of the session, and the Rust one answers every call by its key.
- `compare <session> <first> <second>` prints `compare_replays` as JSON.
"""

import base64
import json
import sys
from pathlib import Path

sys.path.insert(0, sys.argv[1])

import numpy as np  # noqa: E402

from recog import golden_replay  # noqa: E402
from recog.golden import GoldenRecorder, audio_key  # noqa: E402
from recog.wavo_backend import Result, Word  # noqa: E402

SR = 16000
VOCAB = [
    "привет",
    "коллеги",
    "сегодня",
    "обсудим",
    "план",
    "релиза",
    "да",
    "согласен",
    "давайте",
    "начнём",
    "вопрос",
    "спасибо",
]
WORD_S, GAP_S = 0.36, 0.12
FRAME = 320  # 20 ms
BASE_HZ, STEP_HZ = 300.0, 120.0
CHUNK_S, STRIDE_S = 3.0, 2.5
PCM_STRIDE_S, PCM_PAYLOAD_S = 3.0, 3.6

ME = {"id": "me", "name": "Tester", "isLocal": True, "micMuted": False}
ALICE = {"id": "alice-0001", "name": "Alice", "isLocal": False, "micMuted": False}
BOB = {"id": "bob-0002", "name": "Bob", "isLocal": False, "micMuted": False}
URL = "https://synthetic.ktalk.ru/room-synthetic"


# ---- the synthetic model ----


def phrase(words: list[int]) -> np.ndarray:
    """The tones of a phrase: one pitch per word, a short pause after each."""
    parts = []
    for k in words:
        t = np.arange(int(WORD_S * SR)) / SR
        parts.append((0.2 * np.sin(2 * np.pi * (BASE_HZ + STEP_HZ * k) * t)).astype(np.float32))
        parts.append(np.zeros(int(GAP_S * SR), np.float32))
    return np.concatenate(parts)


def decode(samples: np.ndarray) -> list[tuple[str, int, int]]:
    """The words of the tones in ``samples`` with their start and end in ms."""
    x = samples.astype(np.float32)
    if np.issubdtype(samples.dtype, np.integer):
        x = x / 32768.0
    words, current, begin = [], None, 0

    def close(end):
        if current is not None and end - begin >= 3:
            words.append((VOCAB[current], begin * 20, end * 20))

    for i in range(len(x) // FRAME):
        seg = x[i * FRAME : (i + 1) * FRAME]
        k = None
        if float(np.sqrt(np.mean(seg * seg))) >= 0.02:
            crossings = int(np.count_nonzero(np.signbit(seg[1:]) != np.signbit(seg[:-1])))
            k = round((crossings / 2 / (FRAME / SR) - BASE_HZ) / STEP_HZ)
            if not 0 <= k < len(VOCAB):
                k = None
        if k != current:
            close(i)
            current, begin = k, i
    close(len(x) // FRAME)
    return words


def answer(call: str, audio: np.ndarray):
    words = decode(audio)
    if call == "transcribe_with_timestamps":
        refined = [(w.capitalize(), s, e) for w, s, e in words]
        return {"text": " ".join(w for w, _, _ in refined), "words": [list(w) for w in refined]}
    text = " ".join(w for w, _, _ in words)
    return text if call == "transcribe_batch" or text else None


# ---- sessions ----


class Clock:
    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t


class Tape:
    """One source's audio over the session, from its first sample at session time 0."""

    def __init__(self, seconds: float):
        self.x = np.zeros(int(seconds * SR), np.float32)

    def say(self, at: float, words: list[int]) -> None:
        audio = phrase(words)
        start = int(at * SR)
        self.x[start : start + len(audio)] += audio[: len(self.x) - start]

    def cut(self, start: float, seconds: float) -> np.ndarray:
        a, b = int(round(start * SR)), int(round((start + seconds) * SR))
        out = np.zeros(b - a, np.float32)
        lo, hi = max(a, 0), min(b, len(self.x))
        if hi > lo:
            out[lo - a : hi - a] = self.x[lo:hi]
        return out


class Session:
    def __init__(self, directory: Path, seconds: float):
        self.clock = Clock()
        self.wall0 = 1_790_000_000.0
        self.rec = GoldenRecorder(
            directory, clock=self.clock, wall=lambda: self.wall0 + self.clock.t
        )
        self.events = []  # (t, callable)
        self.seconds = seconds

    def at(self, t, fn):
        self.events.append((t, len(self.events), fn))

    def frame(self, t, data, client=1):
        self.at(t, lambda: self.rec.frame(client, data))

    def ms(self, t):
        return (self.wall0 + t) * 1000.0

    def conference(self, participants, join_at=0.3, leave_at=None):
        self.at(0.1, lambda: self.rec.client_connected(1))
        self.frame(0.2, {"type": "clock_sync", "browser_time": self.ms(0.2)})
        self.frame(
            join_at, {"type": "meeting_join", "url": URL, "platform": "ktalk", "title": "Synthetic"}
        )
        self.frame(join_at + 0.1, {"type": "participants_update", "participants": participants})
        if leave_at is not None:
            self.frame(leave_at, {"type": "meeting_leave"})
            self.at(leave_at + 0.5, lambda: self.rec.client_disconnected(1))

    def speaker(self, t, who):
        self.frame(t, {"type": "speaker_update", "speaker": who, "timestamp": self.ms(t)})

    def captures(self, source, tape, first, last, start_shift=0.0):
        """Chunks of ``tape`` every 2.5 s, each recorded 3 s after its start."""
        start = first
        while start <= last:
            chunk = tape.cut(start, CHUNK_S)
            ts = self.wall0 + start + start_shift
            self.at(start + CHUNK_S + 0.05, lambda c=chunk, s=ts: self.rec.capture(source, c, s))
            start += STRIDE_S

    def pcm(self, who, tape, seqs):
        """The worklet's chunks of ``tape``: chunk n ends at 3 s * (n + 1) and holds the
        last 3.6 s."""
        for n in seqs:
            end = PCM_STRIDE_S * (n + 1)
            begin = end - PCM_PAYLOAD_S
            pcm = (np.clip(tape.cut(begin, PCM_PAYLOAD_S), -1, 1) * 32767).astype("<i2")
            data = {
                "type": "pcm_chunk",
                "pid": who["id"],
                "speaker_name": who["name"],
                "seq": n,
                "chunk_start_ms": self.ms(begin),
                "chunk_end_ms": self.ms(end),
                "sample_rate": SR,
                "pcm_base64": base64.b64encode(pcm.tobytes()).decode("ascii"),
            }
            self.frame(end + 0.05, data)

    def write(self):
        for t, _, fn in sorted(self.events, key=lambda e: (e[0], e[1])):
            self.clock.t = t
            fn()
        self.rec.close()


def mic_only(directory):
    """The user alone on the microphone, no conference: an orphan meeting; a phrase
    across chunk boundaries (dedup), a pause (a new segment), the phrase said again
    after the dedup window (kept)."""
    s = Session(directory, 22)
    mic = Tape(22)
    mic.say(0.5, [0, 1, 2, 3, 4, 5])
    mic.say(9.0, [8, 9, 10])
    mic.say(16.0, [0, 1, 2, 3, 4, 5])
    s.captures("mic", mic, 0.0, 19.0)
    s.write()


def ktalk_pcm(directory):
    """A KTalk call: Alice and Bob on per-speaker PCM (with refine after each stops),
    the user on the microphone, one system chunk before the first PCM (the mixed path
    attributes it to the active speaker)."""
    s = Session(directory, 24)
    s.conference([ME, ALICE, BOB], leave_at=22.0)
    s.speaker(0.5, ALICE)
    alice, bob, mic, system = Tape(24), Tape(24), Tape(24), Tape(24)
    alice.say(0.6, [0, 1, 2, 3, 4])
    alice.say(6.2, [5, 6, 7])
    bob.say(9.3, [8, 9, 10, 11])
    mic.say(13.5, [6, 7, 11])
    system.x += alice.x + bob.x
    s.pcm(ALICE, alice, range(0, 3))
    s.pcm(BOB, bob, range(2, 5))
    s.speaker(9.2, BOB)
    s.captures("system", system, -1.5, -1.5)
    s.captures("mic", mic, 0.0, 18.5)
    s.write()


def system_no_remote(directory):
    """A KTalk call with nobody else in it and a podcast on the system audio: the rows
    are "Системный звук"."""
    s = Session(directory, 14)
    s.conference([ME], leave_at=12.5)
    system = Tape(14)
    system.say(0.5, [2, 3, 4, 5])
    system.say(5.5, [10, 11])
    s.captures("system", system, 0.0, 7.5)
    s.write()


def silence_split(directory):
    """An orphan recording, then ten minutes without speech: the next phrase opens a new
    orphan meeting."""
    s = Session(directory, 9)
    mic = Tape(708)
    mic.say(0.5, [0, 1])
    mic.say(700.5, [6, 7])
    mic.say(703.0, [9, 10])
    s.captures("mic", mic, 0.0, 0.0)
    for start, t in [(700.0, 5.0), (702.5, 7.0)]:
        chunk = mic.cut(start, CHUNK_S)
        s.at(t, lambda c=chunk, ts=s.wall0 + start: s.rec.capture("mic", c, ts))
    s.write()


def watchdog(directory):
    """A KTalk call whose PCM stops for over 30 s while Alice goes on speaking on the
    system audio: the watchdog marker, rows on the mixed fallback, PCM back and the
    recovery marker."""
    s = Session(directory, 51)
    s.conference([ME, ALICE], leave_at=49.5)
    s.speaker(0.5, ALICE)
    alice = Tape(51)
    alice.say(0.6, [0, 1, 2])
    alice.say(37.0, [3, 4, 5, 6])
    alice.say(41.0, [7, 8])
    # PCM at 3.05 and 6.05, degraded from about 36.1, back at 45.05; the system
    # chunks arrive at 2.05 + 2.5 k, none within 0.5 s of those moments.
    s.pcm(ALICE, alice, [0, 1, 14, 15])
    s.captures("system", alice, -1.0, 45.0)
    s.write()


SESSIONS = {
    "mic_only": mic_only,
    "ktalk_pcm": ktalk_pcm,
    "system_no_remote": system_no_remote,
    "silence_split": silence_split,
    "watchdog": watchdog,
}


# ---- filling the answers through the Python replay ----


def fill(session: Path, out: Path, tail: float) -> dict:
    answers = []

    class SyntheticASR(golden_replay.ReplayASR):
        def _answer(self, audio, call):
            key = audio_key(np.asarray(audio))
            if key not in self.answers:
                self.answers[key] = answer(call, np.asarray(audio))
                answers.append((call, np.array(audio), self.answers[key]))
            return self.answers[key]

    golden_replay.ReplayASR = SyntheticASR
    golden_replay.TAIL_S = tail
    summary = golden_replay.replay(session, out)
    last = list(golden_replay.read_session(session))[-1]
    clock = iter([0.0] + [last["t"]] * (len(answers) + 1))
    rec = GoldenRecorder(session, clock=lambda: next(clock), wall=lambda: last["wall"])
    for call, audio, output in answers:
        if call == "transcribe_with_timestamps":
            # The recorder keeps a Result as its text and words.
            output = Result(output["text"], [Word(*w) for w in output["words"]])
        elif call == "transcribe_batch":
            # One clip per event: a batch answer is the list of its clips' texts.
            output = [output]
        rec.asr(call, [audio] if call == "transcribe_batch" else audio, output)
    rec.close()
    return {**summary, "answers": len(answers)}


def main() -> int:
    command, args = sys.argv[2], sys.argv[3:]
    if command == "make":
        SESSIONS[args[0]](Path(args[1]))
        print(json.dumps({"made": args[0]}))
    elif command == "fill":
        print(json.dumps(fill(Path(args[0]), Path(args[1]), float(args[2]))))
    elif command == "compare":
        result = golden_replay.compare_replays(Path(args[0]), Path(args[1]), Path(args[2]))
        print(json.dumps(result, ensure_ascii=False))
    else:
        raise SystemExit(f"unknown command {command}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
