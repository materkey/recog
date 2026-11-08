# The Python side of rust/recog-desktop/tests/differential.rs: recog/desktop/ run on
# a scripted world, a request on stdin and the answer on stdout (JSON).
#
#     python -I desktop_driver.py            one request (see `handle`)
#     python -I desktop_driver.py child ...  the observer's worker child: the
#                                            arguments of `python -m recog.desktop.worker`
#
# psutil, os.kill and the module ports are replaced as the Python tests monkeypatch
# them: the process table is the JSON file DESKTOP_WORLD names, re-read on every call
# (the test's fake inspector edits it when the application is signalled and when the
# inspector closes), SIGUSR1 appends {"pid": ...} to DESKTOP_SIGNALS, the inspector
# port is DESKTOP_INSPECTOR_PORT. Nothing touches a real process.
import asyncio
import contextlib
import io
import json
import os
import runpy
import sys
from pathlib import Path
from types import SimpleNamespace


def world():
    return json.loads(Path(os.environ["DESKTOP_WORLD"]).read_text())


def install(repo):
    sys.path.insert(0, repo)
    import psutil

    from recog.desktop import processes, service, worker

    def find(pid):
        for entry in world()["processes"]:
            if entry["pid"] == pid:
                return entry
        return None

    class FakeProcess:
        def __init__(self, pid):
            entry = find(pid)
            if entry is None:
                raise psutil.NoSuchProcess(pid)
            error = entry.get("errors", {}).get("open")
            if error:
                raise getattr(psutil, error)(pid)
            self.pid = pid

        def _field(self, name):
            entry = find(self.pid)
            if entry is None:
                raise psutil.NoSuchProcess(self.pid)
            error = entry.get("errors", {}).get(name)
            if error:
                raise getattr(psutil, error)(self.pid)
            return entry

        def name(self):
            return self._field("name")["name"]

        def uids(self):
            uid = self._field("uid").get("uid", "self")
            return SimpleNamespace(real=os.getuid() if uid == "self" else uid)

        def create_time(self):
            return float(self._field("started")["started"])

        def exe(self):
            return self._field("exe")["exe"]

        def cmdline(self):
            return list(self._field("cmdline").get("cmdline", []))

        def environ(self):
            return dict(self._field("environ").get("environ", {}))

        def net_connections(self, kind="inet"):
            assert kind == "tcp"
            return [
                SimpleNamespace(
                    status=psutil.CONN_LISTEN, laddr=SimpleNamespace(ip=host, port=port)
                )
                for host, port in self._field("listeners").get("listeners", [])
            ]

    def process_iter(attrs=None):
        assert attrs == ["pid", "name"]
        for entry in sorted(world()["processes"], key=lambda e: e["pid"]):
            try:
                proc = FakeProcess(entry["pid"])
            except psutil.NoSuchProcess:
                continue
            try:
                name = proc.name()
            except (psutil.AccessDenied, psutil.ZombieProcess):
                name = None
            except psutil.NoSuchProcess:
                continue
            proc.info = {"pid": entry["pid"], "name": name}
            yield proc

    def kill(pid, signal):
        with open(os.environ["DESKTOP_SIGNALS"], "a") as f:
            f.write(json.dumps({"pid": pid, "signal": int(signal)}) + "\n")

    psutil.Process = FakeProcess
    psutil.process_iter = process_iter
    os.kill = kill
    port = int(os.environ["DESKTOP_INSPECTOR_PORT"])
    processes.INSPECTOR_PORT = port
    worker.INSPECTOR_PORT = port
    processes.APP_READY_PORT = int(os.environ["DESKTOP_APP_READY_PORT"])
    return processes, service, worker


def failure(exc):
    return {"error": {"kind": type(exc).__name__, "message": str(exc)}}


def call(function, *args):
    try:
        value = function(*args)
    except Exception as exc:  # noqa: BLE001 - every class is compared
        return failure(exc)
    if isinstance(value, Path):
        value = str(value)
    if isinstance(value, list) and value and hasattr(value[0], "to_dict"):
        value = [t.to_dict() for t in value]
    return {"value": value}


def functions(request, processes, service, worker):
    from recog.desktop import storage

    results = []
    for item in request["calls"]:
        name, args = item["fn"], item.get("args", [])
        if name == "shlex":
            import shlex

            results.append(call(shlex.split, args[0]))
        elif name == "inspector_url":
            results.append(call(worker.inspector_url, args[0]))
        elif name == "status":
            service.time.time = lambda now=args[1]: now
            results.append(call(service.status, Path(args[0])))
        elif name == "application_path":
            results.append(call(processes.application_path, args[0]))
        elif name == "is_ktalk_executable":
            results.append(call(processes.is_ktalk_executable, Path(args[0])))
        elif name == "inspect_fuses":
            processes._inspect_fuses.cache_clear()
            results.append(call(processes._inspect_fuses, args[0], ("x",)))
        elif name == "find_targets":
            results.append(call(processes.find_targets, Path(args[0])))
        elif name == "ready":
            results.append(call(processes.ready, processes.Target.from_dict(args[0])))
        elif name == "same_process":
            app = Path(args[1]) if len(args) > 1 and args[1] else None
            results.append(
                call(processes.same_process, processes.Target.from_dict(args[0]), app)
            )
        elif name == "owns_inspector":
            results.append(
                call(processes.owns_inspector, processes.Target.from_dict(args[0]))
            )
        elif name == "inspector_supported":
            processes._inspect_fuses.cache_clear()
            results.append(
                call(processes.inspector_supported, processes.Target.from_dict(args[0]))
            )
        elif name == "target_from_dict":
            results.append(
                call(lambda d: processes.Target.from_dict(d).to_dict(), args[0])
            )
        elif name == "read_json":
            results.append(call(storage.read_json, Path(args[0])))
        elif name == "write_status":
            results.append(call(storage.write_status, Path(args[0]), args[1]))
        elif name == "bundle_digest":
            results.append(call(storage.bundle_digest, Path(args[0])))
        elif name == "prepare_bundle":
            results.append(
                call(
                    lambda s, r: list(
                        map(str, storage.prepare_bundle(Path(s), Path(r)))
                    ),
                    *args,
                )
            )
        elif name == "validate_bundle":
            results.append(call(storage.validate_bundle, Path(args[0]), args[1]))
        else:
            raise ValueError(name)
    return results


class Recorder:
    """Notifications and log lines of the observer."""

    def __init__(self, service, notify_send):
        self.notifications = []
        real = service.asyncio.create_subprocess_exec

        async def create(*args, **kwargs):
            if args and args[0] in ("osascript", "notify-send"):
                self.notifications.append(list(args))
                return SimpleNamespace(
                    wait=lambda: asyncio.sleep(0, 0), terminate=lambda: None
                )
            return await real(*args, **kwargs)

        service.asyncio.create_subprocess_exec = create
        service.shutil.which = lambda name: (
            "/usr/bin/notify-send" if notify_send else None
        )


def snapshot(root, service, extra):
    files = {}
    for name in ("config", "status", "operation", "heartbeat", "runtime"):
        path = root / f"{name}.json"
        files[name] = json.loads(path.read_text()) if path.exists() else None
    return {"files": files, "status": service.status(root), **extra}


async def observer(request, processes, service, worker):
    root = Path(request["root"])
    recorder = Recorder(service, request.get("notify_send", False))
    service.sys.executable = request["wrapper"]
    current = service.DesktopIntegration(root)
    out = []
    for step in request["steps"]:
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            for name, value in step.get("files", {}).items():
                path = root / name
                if value is None:
                    path.unlink(missing_ok=True)
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(json.dumps(value))
            if "world" in step:
                Path(os.environ["DESKTOP_WORLD"]).write_text(json.dumps(step["world"]))
            if step.get("restart"):
                if current._lock_fd is not None:
                    os.close(current._lock_fd)
                current = service.DesktopIntegration(root)
            error = None
            if step.get("tick"):
                try:
                    await current._tick()
                except Exception as exc:  # noqa: BLE001
                    error = {"kind": type(exc).__name__, "message": str(exc)}
            if step.get("run"):
                ticks = {"n": 0}
                original = current._tick

                async def counted(
                    original=original, ticks=ticks, limit=step["run"], observer=current
                ):
                    ticks["n"] += 1
                    if ticks["n"] >= limit:
                        observer._stop.set()
                    await original()

                current._tick = counted
                service.POLL_SECONDS = 0
                current._stop.clear()
                await current._run()
                del current._tick
        logs = [line for line in stderr.getvalue().splitlines() if line]
        out.append(snapshot(root, service, {"error": error, "logs": logs}))
    out.append({"notifications": recorder.notifications})
    if current._lock_fd is not None:
        os.close(current._lock_fd)
    return out


def handle(request):
    repo = request["repo"]
    if request["kind"] == "cli":
        sys.path.insert(0, repo)
        from recog import cli
        from recog.desktop import cli as desktop_cli

        desktop_cli.__file__ = str(Path(request["fake_repo"]) / "recog/desktop/cli.py")
        sys.argv = ["recog", *request["argv"]]
        out, err = io.StringIO(), io.StringIO()
        code = 0
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                cli.main()
            except SystemExit as exc:
                code = exc.code if isinstance(exc.code, int) else 1
        return {"code": code, "stdout": out.getvalue(), "stderr": err.getvalue()}
    if request["kind"] == "python":
        # Which urlsplit this interpreter has: the bracketed-netloc check came with a
        # 3.12 patch release, and the port follows it.
        import urllib.parse

        return {"bracketed_netloc": hasattr(urllib.parse, "_check_bracketed_netloc")}
    if request["kind"] == "psutil":
        # The real psutil on a process the test started itself.
        import psutil

        proc = psutil.Process(request["pid"])
        environ = proc.environ()
        listening = [
            [c.laddr.ip, c.laddr.port]
            for c in proc.net_connections(kind="tcp")
            if c.status == psutil.CONN_LISTEN
        ]
        return {
            "name": proc.name(),
            "uid": proc.uids().real,
            "started": proc.create_time(),
            "exe": proc.exe(),
            "cmdline": proc.cmdline(),
            "probe": environ.get("RECOG_DESKTOP_PROBE"),
            "environ_count": len(environ),
            "listeners": sorted(listening, key=json.dumps),
        }
    processes, service, worker = install(repo)
    if request["kind"] == "functions":
        return functions(request, processes, service, worker)
    if request["kind"] == "worker":
        target = (
            processes.Target.from_dict(request["target"])
            if request.get("target")
            else None
        )
        try:
            return {
                "value": worker.run(
                    Path(request["root"]), target, request.get("recover", False)
                )
            }
        except Exception as exc:  # noqa: BLE001
            return failure(exc)
    if request["kind"] == "cli_attach":
        from recog.desktop import cli as desktop_cli

        args = SimpleNamespace(desktop_command="attach")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = desktop_cli.main(args, Path(request["data_dir"]))
        return {"code": code, "stdout": out.getvalue()}
    if request["kind"] == "observer":
        return asyncio.run(observer(request, processes, service, worker))
    raise ValueError(request["kind"])


def child(argv):
    # `python -m recog.desktop.worker ...` as the observer starts it.
    assert argv[:2] == ["-m", "recog.desktop.worker"], argv
    install(os.environ["DESKTOP_REPO"])
    sys.argv = ["worker", *argv[2:]]
    runpy.run_module("recog.desktop.worker", run_name="__main__", alter_sys=True)


if __name__ == "__main__":
    if sys.argv[1:2] == ["child"]:
        child(sys.argv[2:])
    else:
        json.dump(handle(json.load(sys.stdin)), sys.stdout, ensure_ascii=False)
