"""The Python side of recog-core's parity tests (rust/recog-core/tests/parity.rs).

Run as `python harness.py <repo root> <module>` with a JSON request on stdin; it
answers on stdout with what the Python implementation produced. The Rust test runs the
port on the same request in another directory and compares answers and files.

Time, meeting ids, the local zone (TZ, set by the caller), title generation and the
title executor are made deterministic here, the way the Rust test configures the
port; everything else is recog's own code. The caller points HOME, RECOG_CONFIG_FILE,
RECOG_CONFERENCE_HOSTS_FILE and RECOG_DATA_DIR at temp paths, so nothing reads or
writes the user's real configuration or data.
"""

import base64
import contextlib
import dataclasses
import io
import json
import math
import os
import sys
import uuid as real_uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, sys.argv[1])

from recog import install_config  # noqa: E402

ERROR_KINDS = [
    (json.JSONDecodeError, "Json"),
    (UnicodeDecodeError, "Unicode"),
    (KeyError, "Key"),
    (AttributeError, "Attribute"),
    (TypeError, "Type"),
    (OSError, "Os"),
    (RuntimeError, "Runtime"),
    (ValueError, "Value"),
]


def error(e):
    kind = next(name for cls, name in ERROR_KINDS if isinstance(e, cls))
    return {"error": [kind, str(e)]}


def number(x):
    """Floats as their repr, so the comparison covers the formatting too."""
    if isinstance(x, float):
        return "float:" + repr(x)
    return x


def dump_config(config):
    out = {
        "jitsi": list(config.jitsi),
        "matrix": list(config.matrix),
        "source_aliases": dict(config.source_aliases),
        "source_labels": dict(config.source_labels),
        "from_file": sorted(config.from_file),
    }
    for name in install_config.SECTIONS:
        section = getattr(config, name)
        out[name] = {
            f.name: [number(v) for v in value] if isinstance(value, tuple) else number(value)
            for f in dataclasses.fields(section)
            for value in [getattr(section, f.name)]
        }
    return out


def run_install_config(request):
    results = []
    for case in request["cases"]:
        install_config.REPO_ROOT = Path(case["repo"])
        os.environ.clear()
        os.environ.update(case["env"])
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            try:
                install_config.load_dotenv()
                config = install_config._load()
                result = {
                    "config": dump_config(config),
                    "hosts_json": json.dumps(
                        {p: list(getattr(config, p)) for p in install_config.CONFIGURABLE_PLATFORMS}
                    ),
                }
            except Exception as e:  # noqa: BLE001 - every failure is compared
                result = error(e)
        result["stderr"] = stderr.getvalue().splitlines()
        result["environ"] = dict(os.environ)
        results.append(result)
    keys = {
        variable: install_config.SETTING_KEYS[variable] for variable in install_config.SETTING_KEYS
    }
    return {"results": results, "setting_keys": keys}


def run_data_dir(request):
    from recog.config import get_data_dir

    results = []
    for case in request["cases"]:
        os.environ.clear()
        os.environ.update(case["env"])
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            try:
                result = {"path": str(get_data_dir(Path(case["repo"])))}
            except Exception as e:  # noqa: BLE001
                result = error(e)
        result["stderr"] = stderr.getvalue().splitlines()
        results.append(result)
    return {"results": results}


def run_ws_origin(request):
    from recog.ws_origin import origin_allowed

    return {"results": [origin_allowed(origin, extra) for origin, extra in request["cases"]]}


def run_user_profile(request):
    from recog.user_profile import UserProfileStore, resolve_identity

    store = UserProfileStore(Path(request["data_dir"]))
    results = []
    for op in request["ops"]:
        try:
            if op["op"] == "read":
                results.append(store.read())
            elif op["op"] == "write":
                results.append(store.write(op["name"], op["aliases"]))
            elif op["op"] == "clear":
                results.append(store.clear())
            elif op["op"] == "write_raw":
                store.path.parent.mkdir(parents=True, exist_ok=True)
                store.path.write_bytes(base64.b64decode(op["base64"]))
                results.append(None)
            elif op["op"] == "identity":
                profile = store.read()
                results.append(
                    resolve_identity(profile, legacy_name=op["legacy"], override=op["override"])
                )
        except Exception as e:  # noqa: BLE001
            results.append(error(e))
    return {"results": results}


def run_privacy(request):
    from recog.privacy import CopilotPrivacy, PrivacyStore

    store = PrivacyStore(Path(request["data_dir"]))
    privacy = CopilotPrivacy(store)
    results = []
    for op in request["ops"]:
        try:
            if op["op"] == "read":
                results.append(store.read())
            elif op["op"] == "write":
                results.append(store.write(op["enabled"]))
            elif op["op"] == "write_raw":
                store.path.parent.mkdir(parents=True, exist_ok=True)
                store.path.write_text(op["text"])
                results.append(None)
            elif op["op"] == "apply":
                results.append(privacy.apply(op["payload"]))
        except Exception as e:  # noqa: BLE001
            results.append(error(e))
    return {"results": results}


def sample(x):
    return {"nan": math.nan, "inf": math.inf, "-inf": -math.inf}.get(x, x)


def run_wav(request):
    import numpy as np

    from recog.live import IncrementalWavWriter

    writer = None
    results = []
    stderr = io.StringIO()
    with contextlib.redirect_stderr(stderr):
        for op in request["ops"]:
            if op["op"] == "new":
                writer = IncrementalWavWriter(
                    Path(op["path"]),
                    sample_rate=op["sample_rate"],
                    channels=op["channels"],
                    overlap_seconds=op["overlap_seconds"],
                    max_data_bytes=op["max_data_bytes"],
                )
                results.append(None)
            elif op["op"] == "open":
                writer.open(continuation=op["continuation"])
                results.append(None)
            elif op["op"] == "write":
                chunk = np.array([sample(x) for x in op["samples"]], dtype=np.float32)
                with np.errstate(invalid="ignore"):
                    results.append(writer.write_chunk(chunk))
            elif op["op"] == "mark_discontinuity":
                writer.mark_discontinuity()
                results.append(None)
            elif op["op"] == "offset":
                results.append(writer.get_offset_ms())
            elif op["op"] == "finalize":
                writer.finalize()
                results.append(None)
    return {"results": results, "stderr": stderr.getvalue().splitlines()}


class Clock:
    micros = 0


class FakeDatetime(datetime):
    """`datetime` whose now() is Clock.micros; the rest is the real class."""

    @classmethod
    def now(cls, tz=None):
        moment = datetime(1970, 1, 1, tzinfo=timezone.utc) + timedelta(microseconds=Clock.micros)
        if tz is not None:
            return moment.astimezone(tz)
        return moment.astimezone().replace(tzinfo=None)


class FakeTime:
    @staticmethod
    def time():
        return Clock.micros / 1e6


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
    """Title jobs run at once: the Rust test drains its title thread after every op."""

    def __init__(self, *args, **kwargs):
        pass

    def submit(self, fn, *args):
        fn(*args)

    def shutdown(self, wait=True):
        pass


def fake_generate_title(transcript_text, settings=None, variables=None):
    if "boom" in transcript_text:
        raise RuntimeError("boom")
    if "untitled" in transcript_text:
        return None
    return (
        f"T{len(transcript_text)}|{variables['user_name']}|{variables['participants']}"
        f"|{variables['meeting_title']}"
    )


class FakeAiSettingsStore:
    def __init__(self, root):
        self.root = root

    def resolve_task(self, task):
        return {"providers": ["claude"], "models": {"claude": "m1"}, "revision": "r1"}


def jsonable(value):
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, tuple):
        return [jsonable(v) for v in value]
    return value


def run_meeting_storage(request):
    import recog.ai_settings
    import recog.title_generator
    from recog import meeting_storage

    install_config.INSTALL_CONFIG = dataclasses.replace(
        install_config.INSTALL_CONFIG, source_aliases=dict(request.get("aliases", {}))
    )
    meeting_storage.datetime = FakeDatetime
    meeting_storage.time = FakeTime
    meeting_storage.uuid = FakeUuidModule
    meeting_storage.ThreadPoolExecutor = InlineExecutor
    meeting_storage.EXTERNAL_INDEX_CACHE_DIR = Path(request["root"]) / "cache"
    recog.title_generator.generate_title = fake_generate_title
    recog.ai_settings.AiSettingsStore = FakeAiSettingsStore

    root = Path(request["root"])
    storage = None
    results, logs = [], []

    def ref(value):
        if isinstance(value, str) and value.startswith("@"):
            return results[int(value[1:])]
        return value

    def path_arg(value):
        return None if value is None else root / value

    for op in request["ops"]:
        stdout, stderr = io.StringIO(), io.StringIO()
        name = op["op"]
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            try:
                if name == "set_time":
                    Clock.micros = op["micros"]
                    result = None
                elif name == "advance":
                    Clock.micros += op["micros"]
                    result = None
                elif name == "write_file":
                    path = root / op["path"]
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(base64.b64decode(op["base64"]))
                    result = None
                elif name == "touch":
                    os.utime(root / op["path"], (op["mtime"], op["mtime"]))
                    result = None
                elif name == "chmod":
                    os.chmod(root / op["path"], op["mode"])
                    result = None
                elif name == "new":
                    external = [(root / p, s) for p, s in op.get("external", [])]
                    storage = meeting_storage.MeetingStorage(
                        root / "meetings", external_paths=external
                    )
                    result = None
                elif name == "start_session":
                    result = storage.start_session(**op["args"])
                elif name == "join":
                    result = storage.on_meeting_join(op["url"], op.get("title"), op["platform"])
                elif name == "ktalk_join":
                    result = storage.on_ktalk_join(op["url"], op.get("title"))
                elif name == "leave":
                    result = storage.on_ktalk_leave(op["reason"])
                elif name == "split":
                    result = storage.split_on_silence_timeout()
                elif name == "end_session":
                    result = storage.end_session()
                elif name == "append":
                    result = storage.append_segment(**op["args"])
                elif name == "update_segments":
                    meeting = op.get("meeting")
                    target = None if meeting is None else storage.base_path / ref(meeting)
                    result = storage.update_segments(op["updates"], meeting_path=target)
                elif name == "update_title":
                    result = storage.update_title(op["title"])
                elif name == "update_participants":
                    result = storage.update_participants(op["participants"])
                elif name == "current_transcript":
                    result = storage.get_current_transcript()
                elif name == "list_meetings":
                    result = storage.list_meetings(op["include_active"], op["refresh"])
                elif name == "list_active":
                    result = storage.list_active_meetings()
                elif name == "get_meeting":
                    result = storage.get_meeting(ref(op["id"]))
                elif name == "find_meeting_path":
                    result = storage.find_meeting_path(ref(op["id"]))
                elif name == "get_audio_paths":
                    result = storage.get_audio_paths(ref(op["id"]))
                elif name == "set_recording_active":
                    result = storage.set_recording_active(op["active"])
                elif name == "set_audio_paths":
                    meeting = op.get("meeting")
                    target = None if meeting is None else storage.base_path / ref(meeting)
                    result = storage.set_audio_paths(
                        op.get("mic"), op.get("system"), meeting_path=target
                    )
                elif name == "create_backup":
                    result = storage.create_transcript_backup(ref(op["id"]))
                elif name == "restore_backup":
                    result = storage.restore_transcript_backup(ref(op["id"]))
                elif name == "cleanup":
                    result = storage.cleanup_old_audio(op["hours"])
                elif name == "delete":
                    result = storage.delete_meeting(ref(op["id"]))
                elif name == "set_diarization":
                    result = storage.set_diarization_result(
                        ref(op["id"]), op["track"], op["speakers"]
                    )
                elif name == "is_conference_source":
                    result = meeting_storage.is_conference_source(op["source"])
                elif name == "current_id":
                    result = storage.get_current_meeting_id()
                else:
                    raise SystemExit(f"unknown op {name}")
                result = jsonable(result)
            except Exception as e:  # noqa: BLE001
                result = error(e)
        results.append(result)
        logs.append(
            {"stdout": stdout.getvalue().splitlines(), "stderr": stderr.getvalue().splitlines()}
        )
    return {"results": results, "logs": logs}


def run_external_paths(request):
    from recog import meeting_storage

    out = []
    for entries in request["cases"]:
        web = dataclasses.replace(
            install_config.INSTALL_CONFIG.web, external_meetings_paths=tuple(entries)
        )
        install_config.INSTALL_CONFIG = dataclasses.replace(install_config.INSTALL_CONFIG, web=web)
        meeting_storage.INSTALL_CONFIG = install_config.INSTALL_CONFIG
        out.append([[str(path), source] for path, source in meeting_storage.get_external_paths()])
    return {"results": out}


RUNNERS = {
    "install_config": run_install_config,
    "data_dir": run_data_dir,
    "ws_origin": run_ws_origin,
    "user_profile": run_user_profile,
    "privacy": run_privacy,
    "wav": run_wav,
    "meeting_storage": run_meeting_storage,
    "external_paths": run_external_paths,
}


if __name__ == "__main__":
    answer = RUNNERS[sys.argv[2]](json.load(sys.stdin))
    sys.stdout.write(json.dumps(answer, ensure_ascii=False))
