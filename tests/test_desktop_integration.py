"""Behavioral boundaries for the optional desktop KTalk loader."""

import asyncio
import json
import os
from pathlib import Path
from types import SimpleNamespace

import pytest

from recog.desktop import processes, service, storage, worker


def bundle(tmp_path):
    source = tmp_path / "dist"
    (source / "src/audio-worklet").mkdir(parents=True)
    for name in ("manifest.json", "injected.js", "src/audio-worklet/downsampler.js"):
        (source / name).write_text(name)
    return source


def test_snapshot_is_immutable_and_detects_tampering(tmp_path):
    source = bundle(tmp_path)
    root = tmp_path / "data"
    digest, snapshot = storage.prepare_bundle(source, root)
    (source / "injected.js").write_text("new build")
    assert (snapshot / "injected.js").read_text() == "injected.js"
    assert storage.validate_bundle(root, {"bundle": digest}) == snapshot
    (snapshot / "injected.js").write_text("tampered")
    with pytest.raises(ValueError, match="integrity"):
        storage.validate_bundle(root, {"bundle": digest})


def test_snapshot_rejects_symlink_in_source_and_snapshot(tmp_path):
    source = bundle(tmp_path)
    (source / "escape.js").symlink_to(tmp_path / "outside.js")
    with pytest.raises(ValueError, match="symbolic"):
        storage.prepare_bundle(source, tmp_path / "data")
    (source / "escape.js").unlink()
    digest, snapshot = storage.prepare_bundle(source, tmp_path / "data")
    (snapshot / "escape.js").symlink_to(tmp_path / "outside.js")
    with pytest.raises(ValueError, match="symbolic"):
        storage.validate_bundle(tmp_path / "data", {"bundle": digest})


def test_process_identity_rejects_pid_reuse_and_helpers(monkeypatch, tmp_path):
    app = tmp_path / "ktalk"
    app.write_text("")
    monkeypatch.setattr(processes.sys, "platform", "linux")
    monkeypatch.setattr(processes, "is_ktalk_executable", lambda executable: True)
    current = {"started": 12.0, "args": ["ktalk"]}

    class Process:
        pid = 42
        info = {"name": "ktalk"}

        def uids(self):
            return SimpleNamespace(real=os.getuid())

        def create_time(self):
            return current["started"]

        def exe(self):
            return str(app)

        def cmdline(self):
            return current["args"]

        def net_connections(self, **kwargs):
            return []

    monkeypatch.setattr(processes.psutil, "Process", lambda pid: Process())
    monkeypatch.setattr(processes.psutil, "process_iter", lambda attrs: [Process()])
    target = processes.find_targets(app)[0]
    current["args"] = ["ktalk", "--type=renderer"]
    assert processes.find_targets(app) == []
    assert not processes.same_process(target, app)
    current["args"] = ["ktalk"]
    current["started"] = 13.0
    assert not processes.same_process(target, app)


def test_ready_requires_native_listener_and_rejects_debugging(monkeypatch):
    target = processes.Target(42, 12.0, "/ktalk")
    args = ["ktalk"]
    monkeypatch.setattr(
        processes.psutil,
        "Process",
        lambda pid: SimpleNamespace(cmdline=lambda: args, environ=dict),
    )
    monkeypatch.setattr(processes, "listeners", lambda target: [("127.0.0.1", 40000)])
    assert not processes.ready(target)
    monkeypatch.setattr(
        processes, "listeners", lambda target: [("127.0.0.1", processes.APP_READY_PORT)]
    )
    assert processes.ready(target)
    args.append("--inspect")
    assert not processes.ready(target)


def test_inspector_must_be_owned_and_loopback_only(monkeypatch):
    target = processes.Target(42, 12.0, "/ktalk")
    monkeypatch.setattr(processes, "listeners", lambda target: [])
    assert not processes.owns_inspector(target)
    monkeypatch.setattr(
        processes, "listeners", lambda target: [("0.0.0.0", processes.INSPECTOR_PORT)]
    )
    assert not processes.owns_inspector(target)
    monkeypatch.setattr(
        processes, "listeners", lambda target: [("127.0.0.1", processes.INSPECTOR_PORT)]
    )
    assert processes.owns_inspector(target)


def test_status_disabled_and_stale_heartbeat(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    assert service.status(root)["state"] == "disabled"
    storage.write_json(root / "config.json", {"enabled": True})
    storage.write_json(root / "status.json", {"state": "attached", "app_version": "1"})
    monkeypatch.setattr(service.time, "time", lambda: 100)
    assert service.status(root)["state"] == "server_offline"
    storage.write_json(root / "heartbeat.json", {"running": True, "updated_at": 99})
    assert service.status(root)["state"] == "attached"
    storage.write_json(root / "config.json", {"enabled": False})
    assert service.status(root)["state"] == "disabled"


@pytest.mark.asyncio
async def test_observer_remains_inert_without_configuration(tmp_path, monkeypatch):
    observer = service.DesktopIntegration(tmp_path / "desktop")
    monkeypatch.setattr(
        observer, "_claim", lambda: pytest.fail("observer claimed inert installation")
    )
    await observer._tick()
    assert not observer.root.exists()


@pytest.mark.asyncio
async def test_observer_retries_busy_port_but_caches_success_across_restart(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    storage.write_json(root / "config.json", {"enabled": True, "app": "/ktalk"})
    target = processes.Target(42, 12.0, "/ktalk")
    monkeypatch.setattr(service, "application_path", lambda raw: Path(raw))
    monkeypatch.setattr(service, "find_targets", lambda app: [target])
    monkeypatch.setattr(service, "ready", lambda target: True)
    monkeypatch.setattr(service, "inspector_supported", lambda target: True)
    available = {"value": False}
    monkeypatch.setattr(service, "port_available", lambda: available["value"])
    calls = []

    async def launch(root, selected, recover=False):
        calls.append(selected)
        result = {"state": "attached", "reason": None, "target": selected.to_dict()}
        storage.write_json(root / "status.json", result)
        return result

    monkeypatch.setattr(service, "launch_worker", launch)
    first = service.DesktopIntegration(root)
    await first._tick()
    assert service.status(root)["reason"] == "inspector_port_busy"
    available["value"] = True
    await first._tick()
    assert calls == [target]
    os.close(first._lock_fd)
    first._lock_fd = None
    second = service.DesktopIntegration(root)
    await second._tick()
    assert calls == [target]
    os.close(second._lock_fd)
    second._lock_fd = None


@pytest.mark.asyncio
async def test_foreign_port_prevents_worker_signal(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    source = bundle(tmp_path)
    digest, _ = storage.prepare_bundle(source, root)
    storage.write_json(root / "config.json", {"app": "/ktalk", "bundle": digest})
    monkeypatch.setattr(worker, "same_process", lambda *args: True)
    monkeypatch.setattr(worker, "port_available", lambda: False)
    monkeypatch.setattr(
        worker.os, "kill", lambda *args: pytest.fail("signalled target on foreign port")
    )
    with pytest.raises(worker.AttachError, match="inspector_port_busy"):
        await worker.attach(root, target)
    assert storage.read_json(root / "operation.json") == {}


@pytest.mark.asyncio
async def test_worker_failed_bootstrap_closes_only_its_inspector(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    source = bundle(tmp_path)
    digest, _ = storage.prepare_bundle(source, root)
    storage.write_json(root / "config.json", {"app": "/ktalk", "bundle": digest})
    monkeypatch.setattr(worker, "same_process", lambda *args: True)
    monkeypatch.setattr(worker, "port_available", lambda: True)
    monkeypatch.setattr(worker, "ready", lambda target: True)
    monkeypatch.setattr(worker, "inspector_supported", lambda target: True)
    monkeypatch.setattr(worker, "owns_inspector", lambda target: True)
    signals = []
    monkeypatch.setattr(worker.os, "kill", lambda pid, sig: signals.append((pid, sig)))
    monkeypatch.setattr(
        worker,
        "endpoint",
        lambda target, expected=None: asyncio.sleep(0, result="ws://127.0.0.1:9229/x"),
    )
    expressions = []

    async def evaluate(url, expression):
        expressions.append(expression)
        if expression == worker.CLOSE:
            return True
        if "return {pid: process.pid}" in expression:
            return {"pid": target.pid}
        raise worker.AttachError("inspector_evaluation_failed")

    monkeypatch.setattr(worker, "evaluate", evaluate)
    monkeypatch.setattr(worker, "close_owned", lambda operation: asyncio.sleep(0, result=True))
    result = await worker.attach(root, target)
    assert len(signals) == 1
    assert result["reason"] == "inspector_evaluation_failed"
    assert result["inspector_closed"] is True
    assert storage.read_json(root / "operation.json")["phase"] == "closed"


@pytest.mark.asyncio
async def test_cleanup_never_evaluates_foreign_or_reused_inspector(monkeypatch):
    target = processes.Target(42, 12.0, "/ktalk")
    operation = {"target": target.to_dict(), "endpoint": "ws://127.0.0.1:9229/old"}
    monkeypatch.setattr(worker, "same_process", lambda target: True)
    monkeypatch.setattr(worker, "owns_inspector", lambda target: False)
    monkeypatch.setattr(
        processes, "listeners", lambda target: [("127.0.0.1", processes.INSPECTOR_PORT)]
    )
    monkeypatch.setattr(worker, "evaluate", lambda *args: pytest.fail("closed foreign inspector"))
    assert await worker.close_owned(operation) is False
    monkeypatch.setattr(worker, "same_process", lambda target: False)
    assert await worker.close_owned(operation) is True


def test_interrupted_worker_recovers_without_signalling_again(tmp_path, monkeypatch):
    from contextlib import nullcontext

    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    storage.write_json(
        root / "operation.json",
        {"target": target.to_dict(), "phase": "opened", "token": "old"},
    )
    monkeypatch.setattr(worker, "attachment_lock", nullcontext)
    monkeypatch.setattr(worker, "close_owned", lambda operation: asyncio.sleep(0, result=True))
    monkeypatch.setattr(worker.os, "kill", lambda *args: pytest.fail("signalled during recovery"))
    result = worker.run(root, recover=True)
    assert result["reason"] == "worker_interrupted"
    assert result["inspector_closed"] is True
    assert storage.read_json(root / "operation.json")["phase"] == "closed"


def test_web_status_get_is_read_only(tmp_path, monkeypatch):
    from starlette.testclient import TestClient

    from recog.web import server

    monkeypatch.setattr(server, "DATA_DIR", tmp_path)
    with TestClient(server.app) as client:
        response = client.get("/api/desktop/status")
    assert response.status_code == 200
    assert response.json()["state"] == "disabled"
    assert not (tmp_path / "desktop").exists()


def test_asar_identity_requires_ktalk_package_and_main(tmp_path, monkeypatch):
    import struct

    executable = tmp_path / "ktalk"
    executable.write_text("")
    resources = tmp_path / "resources"
    resources.mkdir()
    monkeypatch.setattr(processes.sys, "platform", "linux")

    def write_asar(metadata):
        body = json.dumps(metadata).encode()
        tree = json.dumps({"files": {"package.json": {"size": len(body), "offset": "0"}}}).encode()
        (resources / "app.asar").write_bytes(
            struct.pack("<4I", 4, 8 + len(tree), 4, len(tree)) + tree + body
        )

    write_asar({"name": "ktalk", "main": "build/main.js"})
    assert processes.is_ktalk_executable(executable)
    write_asar({"name": "impostor", "main": "build/main.js"})
    assert not processes.is_ktalk_executable(executable)
    write_asar({"name": "ktalk", "main": "other.js"})
    assert not processes.is_ktalk_executable(executable)
    (resources / "app.asar").write_bytes(b"truncated")
    assert not processes.is_ktalk_executable(executable)


@pytest.mark.asyncio
async def test_opening_phase_cleanup_never_discovers_or_evaluates_inspector(
    monkeypatch,
):
    target = processes.Target(42, 12.0, "/ktalk")
    monkeypatch.setattr(worker, "same_process", lambda target: True)
    monkeypatch.setattr(worker, "owns_inspector", lambda target: True)
    monkeypatch.setattr(
        worker, "endpoint", lambda *args: pytest.fail("discovered unrecorded inspector")
    )
    monkeypatch.setattr(
        worker, "evaluate", lambda *args: pytest.fail("evaluated unrecorded inspector")
    )
    assert await worker.close_owned({"target": target.to_dict(), "phase": "opening"}) is False


@pytest.mark.asyncio
async def test_observer_clears_old_operation_after_target_exits_then_attaches_new(
    tmp_path, monkeypatch
):
    root = tmp_path / "desktop"
    old = processes.Target(42, 12.0, "/ktalk")
    new = processes.Target(43, 13.0, "/ktalk")
    storage.write_json(root / "config.json", {"enabled": True, "app": "/ktalk"})
    storage.write_json(
        root / "operation.json",
        {"target": old.to_dict(), "phase": "opened", "token": "old"},
    )
    alive = {"old": True}
    monkeypatch.setattr(
        service, "same_process", lambda target: alive["old"] if target == old else True
    )
    monkeypatch.setattr(service, "application_path", lambda raw: Path(raw))
    monkeypatch.setattr(service, "find_targets", lambda app: [new])
    monkeypatch.setattr(service, "ready", lambda target: True)
    monkeypatch.setattr(service, "inspector_supported", lambda target: True)
    monkeypatch.setattr(service, "port_available", lambda: True)

    async def launch(root, target=None, recover=False):
        if recover:
            return {
                "state": "inspector_unclosed",
                "reason": "cleanup_failed",
                "target": old.to_dict(),
            }
        assert target == new
        result = {"state": "attached", "target": new.to_dict()}
        storage.write_json(root / "status.json", result)
        return result

    monkeypatch.setattr(service, "launch_worker", launch)
    monkeypatch.setattr(
        service.DesktopIntegration, "_notify", lambda self, result: asyncio.sleep(0)
    )
    observer = service.DesktopIntegration(root)
    try:
        await observer._tick()
        assert storage.read_json(root / "operation.json")["phase"] == "opened"
        alive["old"] = False
        await observer._tick()
        assert storage.read_json(root / "operation.json")["phase"] == "closed"
        await observer._tick()
        assert storage.read_json(root / "status.json")["state"] == "attached"
    finally:
        os.close(observer._lock_fd)


@pytest.mark.asyncio
async def test_busy_recovery_lock_does_not_consume_token(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    storage.write_json(
        root / "operation.json",
        {"target": target.to_dict(), "phase": "opened", "token": "old"},
    )
    monkeypatch.setattr(service, "same_process", lambda selected: True)
    calls = []

    async def launch(root, target=None, recover=False):
        calls.append(recover)
        return {
            "state": "error",
            "reason": "attachment_in_progress" if len(calls) == 1 else "worker_interrupted",
        }

    monkeypatch.setattr(service, "launch_worker", launch)
    monkeypatch.setattr(
        service.DesktopIntegration, "_notify", lambda self, result: asyncio.sleep(0)
    )
    observer = service.DesktopIntegration(root)
    try:
        await observer._tick()
        await observer._tick()
        await observer._tick()
        assert calls == [True, True]
    finally:
        os.close(observer._lock_fd)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "previous,runtime_target,expected",
    [
        ("page_initialization_failed", "same", "attached"),
        ("page_initialization_failed", "other", "incompatible"),
        ("unsupported_version", "same", "incompatible"),
    ],
)
async def test_runtime_recovery_when_page_reinjects_then_restores_only_page_failure(
    tmp_path, monkeypatch, previous, runtime_target, expected
):
    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    other = processes.Target(43, 13.0, "/ktalk")
    storage.write_json(root / "config.json", {"enabled": True, "app": "/ktalk"})
    storage.write_json(
        root / "status.json",
        {
            "state": "incompatible",
            "reason": previous,
            "target": target.to_dict(),
            "inspector_closed": True,
            "bundle": "digest",
        },
    )
    storage.write_json(
        root / "runtime.json",
        {
            "state": "ready",
            "reason": None,
            "target": (target if runtime_target == "same" else other).to_dict(),
        },
    )
    monkeypatch.setattr(service, "application_path", lambda raw: Path(raw))
    monkeypatch.setattr(service, "find_targets", lambda app: [target])
    monkeypatch.setattr(
        service.DesktopIntegration, "_notify", lambda self, result: asyncio.sleep(0)
    )
    observer = service.DesktopIntegration(root)
    try:
        await observer._tick()
        await observer._tick()
        result = storage.read_json(root / "status.json")
        assert result["state"] == expected
        assert result["reason"] == (None if expected == "attached" else previous)
        assert result["inspector_closed"] is True
        assert result["bundle"] == "digest"
    finally:
        os.close(observer._lock_fd)


@pytest.mark.asyncio
async def test_observer_failure_when_repeated_then_logs_cause_rate_limited(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    storage.write_json(root / "config.json", {"enabled": True, "app": "/missing"})
    observer = service.DesktopIntegration(root)
    ticks = []
    logged = []

    async def tick():
        ticks.append(True)
        if len(ticks) == 3:
            observer._stop.set()
        raise FileNotFoundError("/missing/ktalk.AppImage")

    monkeypatch.setattr(observer, "_tick", tick)
    monkeypatch.setattr(observer, "_claim", lambda: True)
    monkeypatch.setattr(service, "POLL_SECONDS", 0)
    monkeypatch.setattr(
        service, "log_rate_limited", lambda key, msg, **kw: logged.append((key, msg, kw))
    )
    monkeypatch.setattr(
        service.DesktopIntegration, "_notify", lambda self, result: asyncio.sleep(0)
    )
    observer._lock_fd = os.open(root / "observer.lock", os.O_CREAT | os.O_RDWR, 0o600)
    await observer._run()
    assert len(logged) == 3
    key, message, kwargs = logged[0]
    assert key == "desktop-observer:FileNotFoundError"
    assert "FileNotFoundError: /missing/ktalk.AppImage" in message
    assert kwargs["interval_s"] >= service.POLL_SECONDS
    assert storage.read_json(root / "status.json")["reason"] == "observer_failed"


def test_inspector_connect_when_listener_redirects_then_refuses():
    from websockets.datastructures import Headers
    from websockets.exceptions import InvalidStatus
    from websockets.http11 import Response

    redirect = InvalidStatus(
        Response(302, "Found", Headers({"Location": "ws://127.0.0.1:9229/other"}))
    )
    connection = worker.connect_without_redirects("ws://127.0.0.1:9229/x", open_timeout=1)
    assert isinstance(connection, worker.websockets.connect)
    assert worker.websockets.connect.process_redirect(connection, redirect) == (
        "ws://127.0.0.1:9229/other"
    )
    assert connection.process_redirect(redirect) is redirect


@pytest.mark.asyncio
async def test_late_runtime_incompatibility_overrides_same_target_attachment(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    storage.write_json(root / "config.json", {"enabled": True, "app": "/ktalk"})
    storage.write_json(
        root / "status.json",
        {"state": "attached", "target": target.to_dict(), "inspector_closed": True},
    )
    storage.write_json(
        root / "runtime.json",
        {
            "state": "incompatible",
            "reason": "unsupported_version",
            "target": target.to_dict(),
        },
    )
    monkeypatch.setattr(service, "application_path", lambda raw: Path(raw))
    monkeypatch.setattr(service, "find_targets", lambda app: [target])
    monkeypatch.setattr(
        service.DesktopIntegration, "_notify", lambda self, result: asyncio.sleep(0)
    )
    observer = service.DesktopIntegration(root)
    try:
        await observer._tick()
        result = storage.read_json(root / "status.json")
        assert result["state"] == "incompatible"
        assert result["reason"] == "unsupported_version"
        assert result["inspector_closed"] is True
    finally:
        os.close(observer._lock_fd)


def test_disabled_configuration_preserves_unclosed_inspector_warning(tmp_path):
    root = tmp_path / "desktop"
    storage.write_json(root / "config.json", {"enabled": False})
    storage.write_json(
        root / "status.json",
        {
            "state": "inspector_unclosed",
            "reason": "cleanup_failed",
            "inspector_closed": False,
            "target": {"pid": 42},
        },
    )
    result = service.status(root)
    assert result["enabled"] is False
    assert result["state"] == "inspector_unclosed"
    assert result["reason"] == "cleanup_failed"
    assert result["inspector_closed"] is False
    assert result["target"] == {"pid": 42}


def test_delayed_close_does_not_close_replacement_inspector():
    import shutil
    import subprocess

    node = shutil.which("node")
    if not node:
        pytest.skip("Node is unavailable")
    script = r"""
const vm = require('node:vm');
const expression = JSON.parse(require('node:fs').readFileSync(0, 'utf8'));
let endpoint = 'ws://127.0.0.1:9229/first';
let closes = 0;
let callback;
const inspector = { url: () => endpoint, close: () => { closes++; endpoint = undefined; } };
const context = {
  require: name => { if (name !== 'node:inspector') throw Error(name); return inspector; },
  setTimeout: fn => { callback = fn; return 1; },
  clearTimeout: () => {},
};
vm.createContext(context);
if (vm.runInContext(expression, context) !== true) throw Error('close was not scheduled');
endpoint = 'ws://127.0.0.1:9229/replacement';
callback();
if (closes !== 0) throw Error('replacement inspector closed');
vm.runInContext(expression, context);
callback();
if (closes !== 1) throw Error('same inspector was not closed');
"""
    result = subprocess.run(
        [node, "-e", script],
        input=json.dumps(worker.CLOSE),
        text=True,
        capture_output=True,
        timeout=5,
    )
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize(
    "environment",
    [
        {"ELECTRON_RUN_AS_NODE": ""},
        {"ELECTRON_RUN_AS_NODE": "0"},
        {"NODE_OPTIONS": "--inspect"},
        {"NODE_OPTIONS": "--inspect=127.0.0.1:9333"},
        {"NODE_OPTIONS": "--inspect=0.0.0.0:9229"},
        {"NODE_OPTIONS": "--inspect-brk=127.0.0.1:9333"},
        {"NODE_OPTIONS": "--debug=127.0.0.1:9333"},
        {"NODE_OPTIONS": "--remote-debugging-port=9333"},
        {"NODE_OPTIONS": "--max-old-space-size=512 --inspect-port=9333"},
        {"NODE_OPTIONS": '"unterminated'},
    ],
)
def test_ready_rejects_node_mode_debug_env_and_malformed_options(monkeypatch, environment):
    target = processes.Target(42, 12.0, "/ktalk")
    monkeypatch.setattr(
        processes.psutil,
        "Process",
        lambda pid: SimpleNamespace(cmdline=lambda: ["ktalk"], environ=lambda: environment),
    )
    monkeypatch.setattr(
        processes,
        "listeners",
        lambda selected: [("127.0.0.1", processes.APP_READY_PORT)],
    )
    assert not processes.ready(target)


def test_ready_accepts_non_debug_node_options(monkeypatch):
    target = processes.Target(42, 12.0, "/ktalk")
    monkeypatch.setattr(
        processes.psutil,
        "Process",
        lambda pid: SimpleNamespace(
            cmdline=lambda: ["ktalk"],
            environ=lambda: {"NODE_OPTIONS": "--max-old-space-size=512"},
        ),
    )
    monkeypatch.setattr(
        processes,
        "listeners",
        lambda selected: [("127.0.0.1", processes.APP_READY_PORT)],
    )
    assert processes.ready(target)


@pytest.mark.parametrize(
    "runtime_uid,backing_source,connection_matches,device_major,expected",
    [
        ("owner", True, True, 0, True),
        ("other", True, True, 0, False),
        ("owner", False, True, 0, False),
        ("owner", True, False, 0, False),
        ("owner", True, True, 8, False),
    ],
)
def test_fuse_appimage_requires_runtime_owner_source_and_connection(
    tmp_path,
    monkeypatch,
    runtime_uid,
    backing_source,
    connection_matches,
    device_major,
    expected,
):
    app = tmp_path / "KTalk.AppImage"
    other = tmp_path / "Other.AppImage"
    executable = tmp_path / "mounted" / "ktalk"
    executable.parent.mkdir()
    for path in (app, other, executable):
        path.write_text("")
    connection = 17
    original_stat = Path.stat

    def fake_stat(path, *args, **kwargs):
        if path == executable:
            return SimpleNamespace(st_dev=os.makedev(device_major, connection))
        return original_stat(path, *args, **kwargs)

    monkeypatch.setattr(Path, "stat", fake_stat)
    candidate = SimpleNamespace(
        pid=901,
        info={"name": "KTalk"},
        uids=lambda: SimpleNamespace(
            real=os.getuid() if runtime_uid == "owner" else os.getuid() + 1
        ),
        exe=lambda: str(app if backing_source else other),
    )
    monkeypatch.setattr(processes.psutil, "process_iter", lambda attrs: [candidate])
    original_iterdir = Path.iterdir
    original_read_text = Path.read_text

    def fake_iterdir(path):
        if str(path) == "/proc/901/fdinfo":
            return iter([path / "7"])
        return original_iterdir(path)

    def fake_read_text(path, *args, **kwargs):
        if str(path) == "/proc/901/fdinfo/7":
            observed = connection if connection_matches else connection + 1
            return f"pos:\t0\nfuse_connection:\t{observed}\n"
        return original_read_text(path, *args, **kwargs)

    monkeypatch.setattr(Path, "iterdir", fake_iterdir)
    monkeypatch.setattr(Path, "read_text", fake_read_text)
    assert processes.fuse_source_matches(executable, app) is expected


def test_bare_fuse_mount_name_cannot_identify_selected_appimage(tmp_path, monkeypatch):
    app = tmp_path / "KTalk.AppImage"
    executable = tmp_path / "mount_KTalk.AppImage" / "ktalk"
    executable.parent.mkdir()
    app.write_text("")
    executable.write_text("")
    monkeypatch.setattr(processes.psutil, "process_iter", lambda attrs: [])
    assert not processes.fuse_source_matches(executable, app)


@pytest.mark.parametrize("listener_host", ["127.0.0.1", "0.0.0.0"])
def test_port_available_rejects_live_loopback_or_wildcard_listener(monkeypatch, listener_host):
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((listener_host, 0))
        listener.listen(1)
        monkeypatch.setattr(processes, "INSPECTOR_PORT", listener.getsockname()[1])
        assert not processes.port_available()


def test_port_available_accepts_free_ephemeral_port(monkeypatch):
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    monkeypatch.setattr(processes, "INSPECTOR_PORT", port)
    assert processes.port_available()


@pytest.mark.asyncio
async def test_disabled_observer_clears_unclosed_warning_after_target_exits(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    old = processes.Target(42, 12.0, "/ktalk")
    storage.write_json(root / "config.json", {"enabled": False, "app": "/ktalk"})
    storage.write_json(
        root / "operation.json",
        {"target": old.to_dict(), "phase": "opened", "token": "old"},
    )
    storage.write_json(
        root / "status.json",
        {
            "state": "inspector_unclosed",
            "reason": "cleanup_failed",
            "inspector_closed": False,
        },
    )
    monkeypatch.setattr(service, "same_process", lambda target: False)
    monkeypatch.setattr(
        service,
        "launch_worker",
        lambda *args, **kwargs: pytest.fail("recovered an exited process"),
    )
    observer = service.DesktopIntegration(root)
    try:
        await observer._tick()
        assert storage.read_json(root / "operation.json")["phase"] == "closed"
        assert storage.read_json(root / "status.json")["state"] == "disabled"
        assert service.status(root) == {
            "enabled": False,
            "state": "disabled",
            "reason": None,
            "app_version": None,
            "inspector_closed": None,
        }
    finally:
        os.close(observer._lock_fd)


def test_corrupt_bundle_cannot_overwrite_unclosed_inspector(tmp_path, monkeypatch):
    from contextlib import nullcontext

    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    storage.write_json(root / "config.json", {"app": "/ktalk", "bundle": "bad"})
    storage.write_json(
        root / "operation.json",
        {"target": target.to_dict(), "phase": "opened", "token": "old"},
    )
    critical = {
        "state": "inspector_unclosed",
        "reason": "cleanup_failed",
        "inspector_closed": False,
        "target": target.to_dict(),
    }
    storage.write_json(root / "status.json", critical)
    monkeypatch.setattr(worker, "attachment_lock", nullcontext)
    result = worker.run(root, target)
    assert result == critical
    assert storage.read_json(root / "status.json") == critical


def test_malformed_config_does_not_mask_unclosed_inspector(tmp_path):
    root = tmp_path / "desktop"
    root.mkdir()
    (root / "config.json").write_text("{ invalid")
    storage.write_json(
        root / "status.json",
        {
            "state": "inspector_unclosed",
            "reason": "cleanup_failed",
            "inspector_closed": False,
        },
    )
    result = service.status(root)
    assert result["state"] == "inspector_unclosed"
    assert result["reason"] == "cleanup_failed"
    assert result["inspector_closed"] is False


@pytest.mark.asyncio
async def test_observer_error_cannot_overwrite_unclosed_inspector(tmp_path):
    root = tmp_path / "desktop"
    storage.write_json(root / "operation.json", {"phase": "opened"})
    critical = {
        "state": "inspector_unclosed",
        "reason": "cleanup_failed",
        "inspector_closed": False,
    }
    storage.write_json(root / "status.json", critical)
    observer = service.DesktopIntegration(root)
    await observer._save({"state": "error", "reason": "observer_failed", "inspector_closed": None})
    assert storage.read_json(root / "status.json") == critical


def test_status_updates_normally_and_confirmed_closure_clears_warning(tmp_path):
    root = tmp_path / "desktop"
    attached = {"state": "attached", "inspector_closed": True}
    error = {"state": "error", "reason": "worker_failed", "inspector_closed": None}
    assert storage.write_status(root, attached) == attached
    assert storage.write_status(root, error) == error
    storage.write_json(root / "operation.json", {"phase": "opened"})
    storage.write_json(
        root / "status.json", {"state": "inspector_unclosed", "inspector_closed": False}
    )
    closed = {"state": "waiting", "reason": None, "inspector_closed": True}
    assert storage.write_status(root, closed) == closed
    assert storage.read_json(root / "status.json") == closed
    storage.write_json(
        root / "status.json", {"state": "inspector_unclosed", "inspector_closed": False}
    )
    storage.write_json(root / "operation.json", {"phase": "closed"})
    assert storage.write_status(root, error) == error
    assert storage.read_json(root / "status.json") == error


def fuse_wire(enabled=True, version=1, length=8):
    payload = bytearray(b"00000000")
    payload[3] = 0x31 if enabled else 0x30
    return processes.FUSE_SENTINEL + bytes((version, length)) + bytes(payload[:length])


@pytest.mark.parametrize(
    "wire,expected",
    [
        (fuse_wire(), True),
        (fuse_wire(False), False),
        (fuse_wire(version=2), False),
        (fuse_wire(length=3), False),
        (fuse_wire() + b"gap" + fuse_wire(), True),
        (fuse_wire() + b"gap" + fuse_wire(False), False),
        (fuse_wire() * 3, False),
        (b"ordinary binary", False),
    ],
)
def test_fuse_wire_accepts_only_known_enabled_architectures(tmp_path, wire, expected):
    binary = tmp_path / "Electron Framework"
    binary.write_bytes(wire)
    processes._inspect_fuses.cache_clear()
    assert processes._inspect_fuses(str(binary), (binary.stat().st_size,)) is expected


def test_fuse_wire_crosses_chunk_boundary_and_rejects_truncated_end(tmp_path):
    binary = tmp_path / "Electron Framework"
    binary.write_bytes(b"padding" * 9360 + fuse_wire())
    processes._inspect_fuses.cache_clear()
    assert processes._inspect_fuses(str(binary), ("boundary",))
    binary.write_bytes(b"prefix" + processes.FUSE_SENTINEL + b"\x01\x08" + b"000")
    processes._inspect_fuses.cache_clear()
    assert not processes._inspect_fuses(str(binary), ("truncated",))


def test_inspector_support_cache_changes_with_binary_identity(tmp_path, monkeypatch):
    import time

    monkeypatch.setattr(processes.sys, "platform", "linux")
    binary = tmp_path / "ktalk"
    binary.write_bytes(fuse_wire())
    target = processes.Target(42, time.time() + 100, str(binary))
    processes._inspect_fuses.cache_clear()
    assert processes.inspector_supported(target)
    binary.write_bytes(fuse_wire(False))
    os.utime(binary, (time.time() + 5, time.time() + 5))
    assert not processes.inspector_supported(target)
    binary.unlink()
    assert not processes.inspector_supported(target)


@pytest.mark.asyncio
async def test_unsupported_inspector_never_signals_target(tmp_path, monkeypatch):
    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    digest, _ = storage.prepare_bundle(bundle(tmp_path), root)
    storage.write_json(root / "config.json", {"app": "/ktalk", "bundle": digest})
    monkeypatch.setattr(worker, "same_process", lambda *args: True)
    monkeypatch.setattr(worker, "port_available", lambda: True)
    monkeypatch.setattr(worker, "ready", lambda selected: True)
    monkeypatch.setattr(worker, "inspector_supported", lambda selected: False)
    monkeypatch.setattr(
        worker.os, "kill", lambda *args: pytest.fail("signalled unsupported target")
    )
    with pytest.raises(worker.AttachError, match="inspector_disabled_or_unverified"):
        await worker.attach(root, target)
    assert storage.read_json(root / "operation.json") == {}


@pytest.mark.parametrize(
    "options",
    [
        "--disable-sigusr1",
        "--disable_sigusr1",
        "--experimental-config-file=/tmp/node.json",
        "--experimental_default_config_file=/tmp/node.json",
    ],
)
def test_ready_rejects_flags_that_change_signal_or_node_config(monkeypatch, options):
    target = processes.Target(42, 12.0, "/ktalk")
    monkeypatch.setattr(
        processes.psutil,
        "Process",
        lambda pid: SimpleNamespace(
            cmdline=lambda: ["ktalk"], environ=lambda: {"NODE_OPTIONS": options}
        ),
    )
    monkeypatch.setattr(
        processes,
        "listeners",
        lambda selected: [("127.0.0.1", processes.APP_READY_PORT)],
    )
    assert not processes.ready(target)


def test_inspector_support_rejects_binary_changed_after_process_start(tmp_path, monkeypatch):
    import time

    monkeypatch.setattr(processes.sys, "platform", "linux")
    binary = tmp_path / "ktalk"
    binary.write_bytes(fuse_wire())
    target = processes.Target(42, time.time() - 100, str(binary))
    processes._inspect_fuses.cache_clear()
    assert not processes.inspector_supported(target)


@pytest.mark.parametrize(
    "targets,reason",
    [
        ({}, "ambiguous_inspector"),
        ([], "ambiguous_inspector"),
        ([{}, {}], "ambiguous_inspector"),
        (
            [{"webSocketDebuggerUrl": "http://127.0.0.1:9229/x"}],
            "invalid_inspector_endpoint",
        ),
        (
            [{"webSocketDebuggerUrl": "ws://example.com:9229/x"}],
            "invalid_inspector_endpoint",
        ),
        (
            [{"webSocketDebuggerUrl": "ws://127.0.0.1:9999/x"}],
            "invalid_inspector_endpoint",
        ),
        (
            [{"webSocketDebuggerUrl": "ws://user@127.0.0.1:9229/x"}],
            "invalid_inspector_endpoint",
        ),
        (
            [{"webSocketDebuggerUrl": "ws://:secret@127.0.0.1:9229/x"}],
            "invalid_inspector_endpoint",
        ),
        (
            [{"webSocketDebuggerUrl": "ws://127.0.0.1:9229/x?query"}],
            "invalid_inspector_endpoint",
        ),
        (
            [{"webSocketDebuggerUrl": "ws://127.0.0.1:9229/x#fragment"}],
            "invalid_inspector_endpoint",
        ),
    ],
)
def test_untrusted_inspector_discovery_when_validated_then_rejected(targets, reason):
    with pytest.raises(worker.AttachError, match=reason):
        worker.inspector_url(targets)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "owners,expected,large,reason",
    [
        ([True, True], None, False, None),
        ([False], None, False, "inspector_owner_mismatch"),
        ([True, False], None, False, "inspector_owner_mismatch"),
        ([True], "ws://127.0.0.1:9229/old", False, "inspector_session_changed"),
        ([True], None, True, "invalid_inspector_response"),
    ],
)
async def test_discovered_endpoint_when_ownership_changes_then_refused(
    monkeypatch, owners, expected, large, reason
):
    target = processes.Target(42, 12.0, "/ktalk")
    ownership = iter(owners)
    monkeypatch.setattr(worker, "owns_inspector", lambda target: next(ownership))
    url = "ws://127.0.0.1:9229/current"

    class Client:
        def __init__(self, **kwargs):
            assert kwargs == {"trust_env": False, "timeout": 1}

        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            pass

        async def get(self, address):
            assert address == "http://127.0.0.1:9229/json/list"
            return SimpleNamespace(
                content=b" " * (65537 if large else 10),
                raise_for_status=lambda: None,
                json=lambda: [{"webSocketDebuggerUrl": url}],
            )

    monkeypatch.setattr(worker.httpx, "AsyncClient", Client)
    if reason:
        with pytest.raises(worker.AttachError, match=reason):
            await worker.endpoint(target, expected)
    else:
        assert await worker.endpoint(target, expected) == url


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "reply,error,version",
    [
        ({"id": 1, "result": {"result": {"value": {"pid": 42}}}}, False, "15.0"),
        (
            {"id": 1, "result": {"exceptionDetails": {"description": "private"}}},
            True,
            "14.0",
        ),
        ({"id": 1, "error": {"message": "private"}}, True, "15.0"),
    ],
)
async def test_inspector_rpc_when_events_or_errors_arrive_then_filters_and_redacts(
    monkeypatch, reply, error, version
):
    messages = iter([json.dumps({"method": "Debugger.paused"}), json.dumps(reply)])
    sent = []

    class Socket:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            pass

        async def send(self, value):
            sent.append(json.loads(value))

        async def recv(self):
            return next(messages)

    class connect:
        def __init__(self, url, **kwargs):
            assert ("proxy" in kwargs) == version.startswith("15")
            self.socket = Socket()

        async def __aenter__(self):
            return self.socket

        async def __aexit__(self, *args):
            pass

    monkeypatch.setattr(worker.websockets, "__version__", version)
    monkeypatch.setattr(worker.websockets, "connect", connect)
    if error:
        with pytest.raises(worker.AttachError, match="^inspector_evaluation_failed$"):
            await worker.evaluate("ws://127.0.0.1:9229/x", "safe expression")
    else:
        assert await worker.evaluate("ws://127.0.0.1:9229/x", "safe expression") == {"pid": 42}
    assert sent[0]["params"]["expression"] == "safe expression"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "outcome,closed,reason",
    [
        ("success", True, None),
        ("timeout", True, "attachment_timeout"),
        ("unexpected", True, "attachment_failed"),
        ("success", False, "cleanup_failed"),
        ("changed", True, "process_changed"),
        ("exited", True, "process_exited"),
        ("waiting", True, None),
    ],
)
async def test_attachment_when_operation_ends_then_cleanup_matches_signal_ownership(
    tmp_path, monkeypatch, outcome, closed, reason
):
    root = tmp_path / "desktop"
    target = processes.Target(42, 12.0, "/ktalk")
    digest, _ = storage.prepare_bundle(bundle(tmp_path), root)
    storage.write_json(root / "config.json", {"app": "/ktalk", "bundle": digest})
    checks = iter([True, outcome != "changed", outcome != "exited"])
    monkeypatch.setattr(worker, "same_process", lambda *args: next(checks))
    monkeypatch.setattr(worker, "port_available", lambda: True)
    monkeypatch.setattr(worker, "ready", lambda target: True)
    monkeypatch.setattr(worker, "inspector_supported", lambda target: True)
    ownership = iter([False, True])
    monkeypatch.setattr(
        worker,
        "owns_inspector",
        lambda target: next(ownership) if outcome == "waiting" else outcome != "exited",
    )
    signals = []
    monkeypatch.setattr(worker.os, "kill", lambda *args: signals.append(args))
    monkeypatch.setattr(
        worker,
        "endpoint",
        lambda target: asyncio.sleep(0, result="ws://127.0.0.1:9229/x"),
    )

    async def evaluate(url, expression):
        if outcome == "timeout":
            raise TimeoutError
        if outcome == "unexpected":
            raise RuntimeError("private")
        if "return {pid: process.pid}" in expression:
            return {"pid": 42}
        return {"app_version": "1"}

    monkeypatch.setattr(worker, "evaluate", evaluate)
    cleanup = []

    async def close(operation):
        cleanup.append(operation)
        return closed

    monkeypatch.setattr(worker, "close_owned", close)
    result = await worker.attach(root, target)
    assert result["reason"] == reason
    assert result["inspector_closed"] is closed
    assert len(signals) == len(cleanup) == (0 if outcome == "changed" else 1)
    assert storage.read_json(root / "operation.json")["phase"] == ("closed" if closed else "opened")


@pytest.mark.parametrize(
    "recover,phase,closed",
    [(True, None, True), (True, "closed", True), (True, "opened", False)],
)
def test_worker_recovery_when_journal_changes_then_preserves_unclosed_state(
    tmp_path, monkeypatch, recover, phase, closed
):
    from contextlib import nullcontext

    target = processes.Target(42, 12.0, "/ktalk")
    if phase:
        storage.write_json(
            tmp_path / "operation.json", {"phase": phase, "target": target.to_dict()}
        )
    monkeypatch.setattr(worker, "attachment_lock", nullcontext)
    monkeypatch.setattr(worker, "close_owned", lambda operation: asyncio.sleep(0, result=closed))
    result = worker.run(tmp_path, recover=recover)
    assert result["inspector_closed"] is closed
    assert result["state"] == ("waiting" if closed else "inspector_unclosed")


@pytest.mark.parametrize("critical", [False, True])
def test_attachment_lock_when_busy_then_keeps_existing_cleanup_warning(
    tmp_path, monkeypatch, critical
):
    from contextlib import contextmanager

    @contextmanager
    def busy():
        raise BlockingIOError
        yield

    monkeypatch.setattr(worker, "attachment_lock", busy)
    if critical:
        storage.write_json(
            tmp_path / "status.json",
            {"state": "inspector_unclosed", "reason": "cleanup_failed"},
        )
    result = worker.run(tmp_path)
    assert result["reason"] == ("cleanup_failed" if critical else "attachment_in_progress")


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "mode", ["success", "stale", "attaching", "unclosed", "interrupted", "poll_timeout"]
)
async def test_child_worker_when_polled_then_heartbeats_and_recovers_interruption(
    tmp_path, monkeypatch, mode
):
    target = processes.Target(42, 12.0, "/ktalk")
    calls = []
    kills = []

    class Child:
        returncode = None

        async def wait(self):
            self.returncode = 0
            return 0

        def kill(self):
            kills.append(self)

    async def create(*args, **kwargs):
        calls.append(args)
        recovering = "--recover" in args
        result = {"state": "attached", "updated_at": 101, "target": target.to_dict()}
        if mode == "stale":
            result["updated_at"] = 99
        if mode == "attaching":
            result["state"] = "attaching"
        if mode == "unclosed":
            result["state"] = "inspector_unclosed"
        if mode == "interrupted":
            storage.write_json(
                tmp_path / "operation.json",
                {"phase": "closed" if recovering else "opened"},
            )
        storage.write_json(tmp_path / "status.json", result)
        return Child()

    monkeypatch.setattr(service.asyncio, "create_subprocess_exec", create)
    monkeypatch.setattr(service.time, "time", lambda: 100)
    # Don't replace time.monotonic: asyncio owns the same module's clock.
    if mode == "poll_timeout":
        original_wait_for = asyncio.wait_for
        attempts = []

        async def wait_for(awaitable, timeout):
            if not attempts:
                attempts.append(True)
                awaitable.close()
                raise TimeoutError
            return await original_wait_for(awaitable, timeout)

        monkeypatch.setattr(service.asyncio, "wait_for", wait_for)
    result = await service.launch_worker(tmp_path, target)
    assert result["state"] == (
        "error"
        if mode in ("stale", "attaching")
        else "inspector_unclosed"
        if mode == "unclosed"
        else "attached"
    )
    assert len(calls) == (2 if mode == "interrupted" else 1)
    if mode == "poll_timeout":
        assert storage.read_json(tmp_path / "heartbeat.json")["running"] is True
    assert kills == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "platform,available,mode",
    [
        ("darwin", True, "success"),
        ("linux", True, "success"),
        ("linux", False, "success"),
        ("darwin", True, "timeout"),
        ("darwin", True, "missing"),
    ],
)
async def test_notification_when_error_repeats_then_attempts_once_per_target(
    tmp_path, monkeypatch, platform, available, mode
):
    observer = service.DesktopIntegration(tmp_path)
    monkeypatch.setattr(service.sys, "platform", platform)
    monkeypatch.setattr(
        service.shutil, "which", lambda name: "/bin/notify-send" if available else None
    )
    calls = []
    terminations = []

    class Child:
        async def wait(self):
            return 0

        def terminate(self):
            terminations.append(True)

    async def create(*args, **kwargs):
        calls.append(args)
        if mode == "missing":
            raise OSError
        return Child()

    monkeypatch.setattr(service.asyncio, "create_subprocess_exec", create)
    if mode == "timeout":

        async def wait_for(awaitable, timeout):
            awaitable.close()
            raise TimeoutError

        monkeypatch.setattr(service.asyncio, "wait_for", wait_for)
    result = {
        "state": "inspector_unclosed",
        "reason": "cleanup_failed",
        "target": {"pid": 42},
    }
    await observer._notify({"state": "attached"})
    await observer._notify(result)
    await observer._notify(result)
    assert len(calls) == (1 if available else 0)
    assert bool(terminations) == (mode == "timeout")


@pytest.mark.parametrize(
    "platform,valid",
    [
        ("linux", True),
        ("linux", False),
        ("darwin", True),
        ("darwin", False),
        ("win32", False),
    ],
)
def test_selected_application_when_identity_or_permissions_invalid_then_rejected(
    tmp_path, monkeypatch, platform, valid
):
    import plistlib

    monkeypatch.setattr(processes.sys, "platform", platform)
    app = tmp_path / "KTalk"
    if platform == "darwin":
        (app / "Contents/MacOS").mkdir(parents=True)
        (app / "Contents/Info.plist").write_bytes(
            plistlib.dumps(
                {
                    "CFBundleIdentifier": "kontur.talk" if valid else "other",
                    "CFBundleExecutable": "ktalk",
                }
            )
        )
        (app / "Contents/MacOS/ktalk").write_text("")
    else:
        app.write_text("")
        app.chmod(0o700 if valid else 0o600)
    if valid:
        assert processes.application_path(str(app)) == app
    else:
        with pytest.raises(ValueError):
            processes.application_path(str(app))


@pytest.mark.parametrize(
    "image,executable_name,fuse_match,expected",
    [
        ("selected", "ktalk", False, True),
        ("other", "ktalk", False, False),
        ("", "ktalk", True, True),
        ("selected", "helper", True, False),
    ],
)
def test_appimage_process_when_resolved_then_requires_selected_source(
    tmp_path, monkeypatch, image, executable_name, fuse_match, expected
):
    monkeypatch.setattr(processes.sys, "platform", "linux")
    app = tmp_path / "KTalk.AppImage"
    executable = tmp_path / "mount" / executable_name
    proc = SimpleNamespace(
        uids=lambda: SimpleNamespace(real=os.getuid()),
        cmdline=lambda: ["ktalk"],
        exe=lambda: str(executable),
        environ=lambda: {"APPIMAGE": str(app) if image == "selected" else image},
    )
    monkeypatch.setattr(processes, "is_ktalk_executable", lambda path: True)
    monkeypatch.setattr(processes, "fuse_source_matches", lambda *args: fuse_match)
    assert processes.matches(proc, app) is expected


def test_process_listeners_when_pid_reused_then_never_reads_connections(monkeypatch):
    target = processes.Target(42, 12.0, "/ktalk")
    monkeypatch.setattr(processes, "same_process", lambda target: False)
    monkeypatch.setattr(processes.psutil, "Process", lambda pid: pytest.fail("read reused PID"))
    assert processes.listeners(target) == []
    monkeypatch.setattr(processes, "same_process", lambda target: True)
    connections = [
        SimpleNamespace(
            status=processes.psutil.CONN_LISTEN,
            laddr=SimpleNamespace(ip="127.0.0.1", port=9229),
        ),
        SimpleNamespace(status="ESTABLISHED", laddr=SimpleNamespace(ip="127.0.0.1", port=1)),
    ]
    monkeypatch.setattr(
        processes.psutil,
        "Process",
        lambda pid: SimpleNamespace(net_connections=lambda **kwargs: connections),
    )
    assert processes.listeners(target) == [("127.0.0.1", 9229)]


@pytest.mark.asyncio
@pytest.mark.parametrize("same,closed", [(False, True), (True, False), (True, True)])
async def test_previous_operation_when_attaching_then_requires_identity_and_cleanup_before_signal(
    tmp_path, monkeypatch, same, closed
):
    target = processes.Target(42, 12.0, "/ktalk")
    old = processes.Target(41, 11.0, "/ktalk")
    root = tmp_path / "desktop"
    digest, _ = storage.prepare_bundle(bundle(tmp_path), root)
    storage.write_json(root / "config.json", {"app": "/ktalk", "bundle": digest})
    storage.write_json(root / "operation.json", {"target": old.to_dict(), "phase": "opened"})
    monkeypatch.setattr(worker, "same_process", lambda *args: same)
    monkeypatch.setattr(worker, "close_owned", lambda operation: asyncio.sleep(0, result=closed))
    monkeypatch.setattr(worker, "port_available", lambda: False)
    monkeypatch.setattr(worker.os, "kill", lambda *args: pytest.fail("signal before preflight"))
    if same and not closed:
        result = await worker.attach(root, target)
        assert result == {
            "state": "inspector_unclosed",
            "reason": "cleanup_failed",
            "inspector_closed": False,
            "target": old.to_dict(),
        }
    else:
        with pytest.raises(
            worker.AttachError,
            match="inspector_port_busy" if same else "process_changed",
        ):
            await worker.attach(root, target)
    assert storage.read_json(root / "operation.json")["phase"] == (
        "closed" if same and closed else "opened"
    )


@pytest.mark.asyncio
async def test_worker_deadline_when_exceeded_then_kills_only_spawned_child(tmp_path, monkeypatch):
    target = processes.Target(42, 12.0, "/ktalk")
    clock = iter([0, 18])
    monkeypatch.setattr(
        service,
        "time",
        SimpleNamespace(time=lambda: 100, monotonic=lambda: next(clock)),
    )
    events = []

    class Child:
        returncode = None

        def kill(self):
            events.append("kill child")

        async def wait(self):
            events.append("reap child")
            self.returncode = -9

    async def create(*args, **kwargs):
        return Child()

    monkeypatch.setattr(service.asyncio, "create_subprocess_exec", create)
    monkeypatch.setattr(service.os, "kill", lambda *args: pytest.fail("signalled application"))
    result = await service.launch_worker(tmp_path, target)
    assert events == ["kill child", "reap child"]
    assert result["reason"] == "worker_failed"
    assert result["target"] == target.to_dict()
