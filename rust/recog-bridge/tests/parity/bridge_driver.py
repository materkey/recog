"""The Python side of recog-bridge's differential tests (tests/differential.rs).

Run as `python bridge_driver.py <repo root> <work dir>` with a scenario (JSON) on
stdin; prints on stdout what the Python bridge did: every frame each client
received, the closes, every call into the live side, the query results, the log
lines, the golden taps, the increment runs and the files the meeting storage wrote.
`recog-bridge-probe` runs the Rust bridge on the same scenario and prints the same
shape.

Time (the bridge's `time` and `datetime`, the storage's), meeting ids and title
generation are made deterministic here the way the probe configures the port;
everything else is recog's own code: `KTalkBridge` and a real `MeetingStorage`
under the work dir. The caller points HOME, RECOG_CONFIG_FILE and RECOG_DATA_DIR
at temp paths and sets TZ, so nothing reads the user's configuration or data.

Sockets are fakes handed to `_handle_client`, as the Python tests do, unless the
scenario's transport is "ws": then the bridge's handler serves real WebSockets
(`websockets.serve`, the server `_run_server` starts) and the clients connect to it.
"""

import asyncio
import contextlib
import io
import json
import sys
import time as real_time
import uuid as real_uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, sys.argv[1])

import websockets  # noqa: E402
from websockets.datastructures import Headers  # noqa: E402

import recog.ai_settings  # noqa: E402
import recog.title_generator  # noqa: E402
from recog import ktalk_bridge, meeting_storage  # noqa: E402

WORK = Path(sys.argv[2])
IDLE_TIMEOUT_S = 1.0


class Clock:
    micros = 0


class FakeTime:
    """The bridge's and the storage's `time`: virtual wall and monotonic clocks."""

    @staticmethod
    def time():
        return Clock.micros / 1e6

    @staticmethod
    def monotonic():
        return Clock.micros / 1e6

    sleep = staticmethod(real_time.sleep)


class FakeDatetime(datetime):
    @classmethod
    def now(cls, tz=None):
        moment = datetime(1970, 1, 1, tzinfo=timezone.utc) + timedelta(microseconds=Clock.micros)
        if tz is not None:
            return moment.astimezone(tz)
        return moment.astimezone().replace(tzinfo=None)


class FakeUuidModule:
    """Meeting ids count up; every other uuid4 (temp files) stays random."""

    count = 0

    @classmethod
    def uuid4(cls):
        if sys._getframe(1).f_code.co_name == "_create_meeting":
            cls.count += 1
            return real_uuid.UUID(hex=f"{cls.count:08x}" + "0" * 24)
        return real_uuid.uuid4()


class InlineExecutor:
    def __init__(self, *args, **kwargs):
        pass

    def submit(self, fn, *args):
        fn(*args)

    def shutdown(self, wait=True):
        pass


def fake_generate_title(transcript_text, settings=None, variables=None):
    if "untitled" in transcript_text:
        return None
    return (
        f"T{len(transcript_text)}|{variables['user_name']}|{variables['participants']}"
        f"|{variables['meeting_title']}"
    )


class FakeTitleSettings:
    def __init__(self, root):
        self.root = root

    def resolve_task(self, task):
        return {"providers": ["claude"], "models": {"claude": "m1"}, "revision": "r1"}


class PolicyStore:
    """`AiSettingsStore` for the bridge: the scenario's Copilot task."""

    def __init__(self, policy):
        self.policy = policy

    def resolve_task(self, task):
        if "error" in self.policy:
            raise ValueError(self.policy["error"])
        return {
            "prompt": self.policy["instructions"],
            "revision": self.policy["revision"],
            "enabled": self.policy["enabled"],
        }


class Recorder:
    """The live side: every callback the bridge makes, in order."""

    def __init__(self, root):
        self.root = root
        self.calls = []
        self.pcm_error = None
        self.meeting_change_error = None
        self.stats = None
        self.capture_active = False
        self.capture_start_error = None
        self.capture_stop_error = None
        self.identity = None
        self.identity_error = None
        self.apply_error = None
        self.increments = []
        self.golden = []

    def rel(self, path):
        if path is None:
            return None
        try:
            return str(Path(path).relative_to(self.root))
        except ValueError:
            return str(path)

    def speaker_changed(self, speaker, is_speaking):
        self.calls.append(
            {"call": "speaker_changed", "speaker": speaker, "is_speaking": is_speaking}
        )

    def segment_completed(self, s):
        self.calls.append(
            {
                "call": "segment_completed",
                "segment": {
                    "speaker_name": s.speaker_name,
                    "speaker_id": s.speaker_id,
                    "start_time": float(s.start_time),
                    "end_time": float(s.end_time),
                    "duration_ms": s.duration_ms,
                    "is_local": bool(s.is_local),
                },
            }
        )

    def before_meeting_switch(self):
        self.calls.append({"call": "before_meeting_switch"})

    def meeting_changed(self, meeting_id, meeting_path):
        self.calls.append(
            {"call": "meeting_changed", "meeting_id": meeting_id, "path": self.rel(meeting_path)}
        )
        if self.meeting_change_error:
            raise RuntimeError(self.meeting_change_error)

    def pcm_chunk(self, c):
        self.calls.append(
            {
                "call": "pcm_chunk",
                "chunk": {
                    "pid": c.pid,
                    "speaker_name": c.speaker_name,
                    "is_local": c.is_local,
                    "seq": c.seq,
                    "chunk_start_ts": c.chunk_start_ts,
                    "chunk_end_ts": c.chunk_end_ts,
                    "sample_rate": c.sample_rate,
                    "pcm": c.pcm.hex(),
                },
            }
        )
        if self.pcm_error:
            raise RuntimeError(self.pcm_error)

    def pcm_stats(self):
        if isinstance(self.stats, dict) and "error" in self.stats:
            raise RuntimeError(self.stats["error"])
        return dict(self.stats)

    def set_capture(self, active):
        self.calls.append({"call": "set_capture", "active": active})
        error = self.capture_start_error if active else self.capture_stop_error
        if error:
            raise RuntimeError(error)
        self.capture_active = active

    def recording_active(self):
        return self.capture_active

    def read_identity(self):
        if self.identity_error:
            raise RuntimeError(self.identity_error)
        return self.identity

    def apply_mic(self, identity):
        if self.apply_error:
            raise RuntimeError(self.apply_error)
        self.calls.append({"call": "apply_mic_identity", "identity": identity})


class Increments:
    def __init__(self, recorder):
        self.recorder = recorder

    async def generate_all(self, meeting_id, meeting_path):
        self.recorder.increments.append([meeting_id, self.recorder.rel(meeting_path)])


class Golden:
    def __init__(self, recorder):
        self.recorder = recorder

    def client_connected(self, client):
        self.recorder.golden.append({"event": "connect", "client": client})

    def frame(self, client, data):
        self.recorder.golden.append({"event": "frame", "client": client, "data": data})

    def client_disconnected(self, client):
        self.recorder.golden.append({"event": "disconnect", "client": client})


CLOSE = object()


class FakeWebSocket:
    """A client handed to `_handle_client`: frames queued in, sends recorded out."""

    def __init__(self, index, origins, mode):
        self.remote_address = ("127.0.0.1", 40000 + index)
        if origins is not None:
            self.request = SimpleNamespace(headers=Headers([("Origin", o) for o in origins]))
        self.queue = asyncio.Queue()
        self.sent = []
        self.closes = []
        self.mode = mode
        self.idle = asyncio.Event()

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self.queue.empty():
            self.idle.set()
        item = await self.queue.get()
        self.idle.clear()
        if item is CLOSE:
            self.idle.set()
            raise StopAsyncIteration
        if isinstance(item, Exception):
            self.idle.set()
            raise item
        return item

    async def send(self, text):
        if self.mode == "stall":
            await asyncio.Event().wait()
        if isinstance(self.mode, dict):
            raise RuntimeError(self.mode["fail"])
        self.sent.append(text)

    async def close(self, code=1000, reason=""):
        self.closes.append([code, reason])


async def settle():
    for _ in range(50):
        await asyncio.sleep(0)


class Driver:
    def __init__(self, scenario):
        self.scenario = scenario
        self.config = scenario.get("config", {})
        self.recorder = Recorder(WORK)
        self.clients = {}
        self.tasks = {}
        self.results = []
        self.ws_server = None
        self.ws_clients = {}
        self.ws_received = {}
        self.ws_closes = {}
        self.ws_readers = {}

    def build(self):
        cfg = self.config
        Clock.micros = int(cfg.get("start_micros", 1_783_159_200_000_000))
        ktalk_bridge.time = FakeTime
        ktalk_bridge.datetime = FakeDatetime
        ktalk_bridge.BROADCAST_SEND_TIMEOUT_S = cfg.get("send_timeout", 5.0)
        meeting_storage.datetime = FakeDatetime
        meeting_storage.time = FakeTime
        meeting_storage.uuid = FakeUuidModule
        meeting_storage.ThreadPoolExecutor = InlineExecutor
        recog.title_generator.generate_title = fake_generate_title
        recog.ai_settings.AiSettingsStore = FakeTitleSettings
        self.policy = PolicyStore(
            dict(
                cfg.get("policy", {"instructions": "Be brief.", "revision": "r1", "enabled": True})
            )
        )
        bridge = ktalk_bridge.KTalkBridge(
            ai_settings_store=self.policy, ktalk_only=cfg.get("ktalk_only", False)
        )
        bridge._debug_file = WORK / "ktalk_debug.json"
        bridge._loop = asyncio.get_running_loop()
        r = self.recorder
        bridge.set_speaker_callback(r.speaker_changed)
        bridge.set_segment_callback(r.segment_completed)
        bridge.set_pre_meeting_switch_callback(r.before_meeting_switch)
        bridge.set_meeting_change_callback(r.meeting_changed)
        if cfg.get("wants_pcm", True):
            bridge.set_pcm_chunk_callback(r.pcm_chunk)
        if "pcm_stats" in cfg:
            r.stats = cfg["pcm_stats"]
            bridge.set_pcm_stats_getter(r.pcm_stats)
        if cfg.get("capture"):
            bridge.set_meeting_capture_callback(r.set_capture, active_getter=r.recording_active)
        if "identity" in cfg:
            r.identity = cfg["identity"]
            bridge.configure_user_profile(cfg["identity"], r.read_identity, r.apply_mic)
        if cfg.get("storage", True):
            self.storage = meeting_storage.MeetingStorage(WORK / "meetings")
            self.storage.start_session(ktalk=True, ktalk_only=cfg.get("ktalk_only", False))
            bridge.set_meeting_storage(self.storage)
            bridge.set_increment_generator(Increments(r))
        else:
            self.storage = None
        if cfg.get("golden"):
            bridge.golden = Golden(r)
        self.bridge = bridge

    async def wait_idle(self, ws):
        with contextlib.suppress(asyncio.TimeoutError):
            await asyncio.wait_for(self._idle(ws), IDLE_TIMEOUT_S)
        await settle()

    async def _idle(self, ws):
        while True:
            await ws.idle.wait()
            if ws.queue.empty():
                return
            await asyncio.sleep(0)

    def mode_of(self, step):
        mode = step.get("mode", "record")
        return {"fail": step["fail"]} if mode == "fail" else mode

    async def connect(self, step):
        name = step["client"]
        ws = FakeWebSocket(len(self.clients) + 1, step.get("origins"), self.mode_of(step))
        self.clients[name] = ws
        self.tasks[name] = asyncio.get_running_loop().create_task(self.bridge._handle_client(ws))
        await self.wait_idle(ws)

    async def step(self, step):
        op = step["op"]
        if self.scenario.get("transport") == "ws" and op in (
            "connect",
            "send",
            "send_text",
            "close",
        ):
            await self.ws_step(step)
            return
        if op == "connect":
            await self.connect(step)
        elif op in ("send", "send_text"):
            ws = self.clients[step["client"]]
            ws.queue.put_nowait(
                json.dumps(step["frame"], ensure_ascii=False) if op == "send" else step["text"]
            )
            ws.idle.clear()
            await self.wait_idle(ws)
        elif op == "close":
            ws = self.clients[step["client"]]
            ws.queue.put_nowait(CLOSE)
            ws.idle.clear()
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(asyncio.shield(self.tasks[step["client"]]), IDLE_TIMEOUT_S)
            await settle()
        elif op == "fail":
            ws = self.clients[step["client"]]
            ws.queue.put_nowait(ConnectionError(step["message"]))
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(asyncio.shield(self.tasks[step["client"]]), IDLE_TIMEOUT_S)
            await settle()
        elif op == "mode":
            self.clients[step["client"]].mode = self.mode_of(step)
        elif op == "advance":
            Clock.micros += round(step["seconds"] * 1e6)
        elif op == "sleep":
            await asyncio.sleep(step["seconds"])
            await settle()
        elif op == "periodic":
            self.bridge._check_segment_timeout()
            await self.bridge._recompute_agent_state()
            self.bridge._write_debug_state()
            await settle()
        elif op == "call":
            await self.call(step)
        elif op == "query":
            self.results.append(self.query(step))
        elif op == "set":
            self.set(step)
        else:
            raise ValueError(f"unknown op {op}")

    async def call(self, step):
        method, args = step["method"], step.get("args", {})
        if method == "send_transcript":
            self.bridge.send_transcript(**args)
        elif method == "send_refine":
            self.bridge.send_refine(args["updates"], args.get("meeting_id"))
        elif method == "append_segment":
            self.storage.append_segment(**args)
        elif method == "end_meeting_on_extension_gone":
            await self.bridge._end_meeting_on_extension_gone()
        else:
            raise ValueError(f"unknown call {method}")
        await settle()

    def owner_name(self):
        owner = self.bridge._meeting_owner_ws
        return next((n for n, ws in self.clients.items() if ws is owner), None)

    def query(self, step):
        b, method, args = self.bridge, step["method"], step.get("args", [])
        if method == "state":
            tick = b._copilot_tick_state
            return {
                "pending": [[k, v] for k, v in b._copilot_pending_requests.items()],
                "owner": self.owner_name(),
                "manual": b._manual_copilot_active,
                "tick": None
                if tick is None
                else {
                    "meeting_id": tick["meeting_id"],
                    "meeting_title": tick["meeting_title"],
                    "seg_count": tick["seg_count"],
                    "last_seg_count": tick["last_seg_count"],
                    "last_emit_ts": tick["last_emit_ts"],
                },
                "buffer": [
                    dict(b._copilot_transcript_buffer[s])
                    for s in b._copilot_transcript_order
                    if s in b._copilot_transcript_buffer
                ],
                "last_seq": b._last_seq,
                "clock_offset": b._clock_offset,
                "agent": {
                    "status": b._supervisor_agent_status,
                    "sessions": b._claude_sessions_count,
                    "zero_since": b._sessions_zero_since,
                    "last": b._last_effective_agent_state,
                    "recovery_sent_at": b._channel_recovery_sent_at,
                },
                "roles": {n: self.roles(ws) for n, ws in self.clients.items()},
                "timed_out": sorted(
                    n for n, ws in self.clients.items() if ws in b._timed_out_clients
                ),
                "transport_stats": b._pcm_transport_stats,
            }
        if method == "get_participants":
            return {
                k: {"id": p.id, "name": p.name, "is_local": p.is_local, "mic_muted": p.mic_muted}
                for k, p in b.get_participants().items()
            }
        if method == "get_speaker_history":
            return [
                {
                    "speaker_name": e.speaker_name,
                    "speaker_id": e.speaker_id,
                    "timestamp": e.timestamp,
                    "is_local": e.is_local,
                }
                for e in b.get_speaker_history()
            ]
        if method == "get_all_completed_segments":
            return [
                {
                    "speaker_name": s.speaker_name,
                    "speaker_id": s.speaker_id,
                    "start_time": s.start_time,
                    "end_time": s.end_time,
                    "duration_ms": s.duration_ms,
                    "is_local": s.is_local,
                }
                for s in b.get_all_completed_segments()
            ]
        if method == "compute_meeting_state":
            return b._compute_meeting_state()
        value = getattr(b, method)(*args)
        return value

    def roles(self, ws):
        b = self.bridge
        sets = [
            ("extension", b._connected_clients),
            ("subscriber", b._subscriber_clients),
            ("channel", b._copilot_channel_clients),
            ("supervisor", b._supervisor_clients),
        ]
        return [name for name, members in sets if ws in members]

    def set(self, step):
        r = self.recorder
        for key, value in step.items():
            if key == "op":
                continue
            if key == "policy":
                self.policy.policy = dict(value)
            elif key == "identity":
                r.identity, r.identity_error = value, None
            elif key == "identity_error":
                r.identity_error = value
            elif key == "apply_error":
                r.apply_error = value
            elif key == "pcm_error":
                r.pcm_error = value
            elif key == "meeting_change_error":
                r.meeting_change_error = value
            elif key == "capture_start_error":
                r.capture_start_error = value
            elif key == "capture_stop_error":
                r.capture_stop_error = value
            elif key == "pcm_stats":
                r.stats = value
            else:
                raise ValueError(f"unknown setting {key}")

    async def ws_step(self, step):
        op, name = step["op"], step["client"]
        if op == "connect":
            headers = [("Origin", o) for o in step.get("origins") or []]
            url = f"ws://127.0.0.1:{self.ws_port}"
            self.ws_received[name] = []
            try:
                ws = await websockets.connect(url, additional_headers=headers)
            except websockets.InvalidStatus as e:
                body = e.response.body.decode("utf-8", "replace")
                self.ws_closes[name] = [["http", e.response.status_code, body]]
                return
            self.ws_clients[name] = ws
            self.ws_readers[name] = asyncio.get_running_loop().create_task(self.ws_read(name, ws))
        elif name not in self.ws_clients:
            pass
        elif op == "send":
            await self.ws_clients[name].send(json.dumps(step["frame"], ensure_ascii=False))
        elif op == "send_text":
            text = step["text"]
            await self.ws_clients[name].send(text.encode() if step.get("binary") else text)
        elif op == "close":
            await self.ws_clients[name].close()
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(self.ws_readers[name], 5)
        await asyncio.sleep(0.2)

    async def ws_read(self, name, ws):
        try:
            async for message in ws:
                self.ws_received[name].append(message)
        except websockets.ConnectionClosed:
            pass
        self.ws_closes[name] = (
            [[ws.close_code, ws.close_reason or ""]] if ws.close_code is not None else []
        )

    async def run(self):
        self.build()
        if self.scenario.get("transport") == "ws":
            self.ws_server = await websockets.serve(self.bridge._handle_client, "127.0.0.1", 0)
            self.ws_port = self.ws_server.sockets[0].getsockname()[1]
        for step in self.scenario["steps"]:
            await self.step(step)
        for name, ws in self.clients.items():
            if not self.tasks[name].done():
                ws.queue.put_nowait(CLOSE)
                with contextlib.suppress(asyncio.TimeoutError):
                    await asyncio.wait_for(asyncio.shield(self.tasks[name]), IDLE_TIMEOUT_S)
        for name, ws in self.ws_clients.items():
            if name not in self.ws_closes:
                await ws.close()
                with contextlib.suppress(asyncio.TimeoutError):
                    await asyncio.wait_for(self.ws_readers[name], 5)
        await settle()
        await self.bridge._drain_background_tasks(5.0)
        if self.ws_server is not None:
            self.ws_server.close()
            await self.ws_server.wait_closed()

    def output(self):
        if self.scenario.get("transport") == "ws":
            received = self.ws_received
            closes = {n: self.ws_closes.get(n, []) for n in self.ws_received}
        else:
            received = {n: ws.sent for n, ws in self.clients.items()}
            closes = {n: ws.closes for n, ws in self.clients.items()}
        files = {}
        for path in sorted(WORK.rglob("*")):
            if path.is_file():
                files[str(path.relative_to(WORK))] = path.read_text(encoding="utf-8")
        return {
            "received": received,
            "closes": closes,
            "live": self.recorder.calls,
            "results": self.results,
            "increments": self.recorder.increments,
            "golden": self.recorder.golden,
            "files": files,
        }


def main():
    scenario = json.load(sys.stdin)
    stdout, stderr = io.StringIO(), io.StringIO()
    driver = Driver(scenario)
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        asyncio.run(driver.run())
    out = driver.output()
    out["stdout"] = stdout.getvalue().splitlines()
    out["stderr"] = stderr.getvalue().splitlines()
    json.dump(out, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
