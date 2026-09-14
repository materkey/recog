"""Find only the explicitly selected application's main process, never helpers."""

import errno
import json
import os
import plistlib
import shlex
import socket
import struct
import sys
from dataclasses import asdict, dataclass
from functools import lru_cache
from pathlib import Path

import psutil

INSPECTOR_PORT = 9229
APP_READY_PORT = 44258


@dataclass(frozen=True)
class Target:
    pid: int
    started: float
    executable: str

    @property
    def key(self):
        return f"{self.pid}:{self.started:.6f}"

    def to_dict(self):
        return asdict(self)

    @classmethod
    def from_dict(cls, data):
        return cls(int(data["pid"]), float(data["started"]), str(data["executable"]))


def application_path(raw: str) -> Path:
    path = Path(raw).expanduser().resolve(strict=True)
    if sys.platform == "darwin":
        info = plistlib.loads((path / "Contents/Info.plist").read_bytes())
        if info.get("CFBundleIdentifier") != "kontur.talk":
            raise ValueError("Select the official KTalk application bundle")
        if not (path / "Contents/MacOS" / info["CFBundleExecutable"]).is_file():
            raise ValueError("Application executable is missing")
    elif sys.platform == "linux":
        if not path.is_file() or not os.access(path, os.X_OK):
            raise ValueError("Select an executable KTalk AppImage or binary")
    else:
        raise ValueError("Desktop integration supports Linux and macOS")
    return path


def is_ktalk_executable(executable: Path) -> bool:
    """Verify packaged app identity without executing the selected binary."""
    resources = (
        executable.parent.parent / "Resources"
        if sys.platform == "darwin"
        else executable.parent / "resources"
    )
    try:
        with (resources / "app.asar").open("rb") as archive:
            header = struct.unpack("<4I", archive.read(16))
            if not (0 < header[3] <= 8 * 1024 * 1024):
                return False
            tree = json.loads(archive.read(header[3]))
            package = tree["files"]["package.json"]
            if not 0 < int(package["size"]) <= 65536:
                return False
            archive.seek(8 + header[1] + int(package["offset"]))
            metadata = json.loads(archive.read(int(package["size"])))
            return metadata.get("name") == "ktalk" and metadata.get("main") == "build/main.js"
    except (OSError, ValueError, KeyError, TypeError, struct.error):
        return False


def fuse_source_matches(executable: Path, app: Path) -> bool:
    """Associate an AppImage mount with its backing runtime via kernel IDs.

    AppImage's FUSE runtime may be reparented, and its main process may not
    expose the runtime-added APPIMAGE variable through /proc/environ.
    """
    device = executable.stat().st_dev
    if os.major(device) != 0:
        return False
    connection = os.minor(device)
    for candidate in psutil.process_iter(["pid", "name"]):
        name = candidate.info.get("name") or ""
        if not name or not app.name.startswith(name):
            continue
        try:
            if candidate.uids().real != os.getuid() or Path(candidate.exe()).resolve() != app:
                continue
            for fd in (Path("/proc") / str(candidate.pid) / "fdinfo").iterdir():
                for line in fd.read_text().splitlines():
                    if (
                        line.startswith("fuse_connection:")
                        and int(line.split(":", 1)[1]) == connection
                    ):
                        return True
        except (psutil.Error, OSError, ValueError):
            continue
    return False


def matches(proc, app: Path) -> bool:
    if proc.uids().real != os.getuid():
        return False
    args = proc.cmdline()
    if any(arg.startswith("--type=") for arg in args):
        return False
    executable = Path(proc.exe()).resolve()
    if not is_ktalk_executable(executable):
        return False
    if sys.platform == "darwin":
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        return executable == (app / "Contents/MacOS" / info["CFBundleExecutable"]).resolve()
    if executable == app:
        return True
    # AppImage's runtime mounts a new path each launch. Match its selected
    # source file as well as the actual main executable, not the process name.
    image = proc.environ().get("APPIMAGE", "")
    if executable.name != "ktalk":
        return False
    if image and Path(image).resolve() == app:
        return True
    return fuse_source_matches(executable, app)


def find_targets(app: Path) -> list[Target]:
    targets = []
    for proc in psutil.process_iter(["pid", "name"]):
        if proc.info["name"] not in ("ktalk", "Толк", app.stem, app.name):
            continue
        try:
            if matches(proc, app):
                targets.append(Target(proc.pid, proc.create_time(), proc.exe()))
        except (psutil.Error, OSError, ValueError, KeyError):
            continue
    return targets


def same_process(target: Target, app: Path | None = None) -> bool:
    try:
        proc = psutil.Process(target.pid)
        return (
            proc.uids().real == os.getuid()
            and proc.create_time() == target.started
            and proc.exe() == target.executable
            and (app is None or matches(proc, app))
        )
    except (psutil.Error, OSError, ValueError, KeyError):
        return False


def listeners(target: Target) -> list[tuple[str, int]]:
    if not same_process(target):
        return []
    return [
        (c.laddr.ip, c.laddr.port)
        for c in psutil.Process(target.pid).net_connections(kind="tcp")
        if c.status == psutil.CONN_LISTEN
    ]


def owns_inspector(target: Target) -> bool:
    addresses = [host for host, port in listeners(target) if port == INSPECTOR_PORT]
    return bool(addresses) and all(host in ("127.0.0.1", "::1") for host in addresses)


def port_available() -> bool:
    # BSD permits a specific-address SO_REUSEADDR bind beside a wildcard
    # listener, so binding alone cannot establish that the port is unused.
    with socket.socket() as probe:
        probe.settimeout(0.2)
        if probe.connect_ex(("127.0.0.1", INSPECTOR_PORT)) != errno.ECONNREFUSED:
            return False
    with socket.socket() as sock:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            sock.bind(("127.0.0.1", INSPECTOR_PORT))
            return True
        except OSError:
            return False


def ready(target: Target) -> bool:
    proc = psutil.Process(target.pid)
    environment = proc.environ()
    if "ELECTRON_RUN_AS_NODE" in environment:
        return False
    try:
        options = proc.cmdline() + shlex.split(environment.get("NODE_OPTIONS", ""))
    except ValueError:
        return False
    if any(
        arg.replace("_", "-").startswith(
            (
                "--inspect",
                "--remote-debugging",
                "--debug",
                "--disable-sigusr1",
                "--experimental-config-file",
                "--experimental-default-config-file",
            )
        )
        for arg in options
    ):
        return False
    # KTalk opens its native URL-handler listener after initialization. A
    # process name alone does not establish that Node installed SIGUSR1 yet.
    return any(
        port == APP_READY_PORT and host in ("127.0.0.1", "::1") for host, port in listeners(target)
    )


FUSE_SENTINEL = b"dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX"


@lru_cache(maxsize=8)
def _inspect_fuses(path: str, fingerprint: tuple) -> bool:
    """Read every architecture's v1 fuse wire, in bounded-memory chunks."""
    found = {}
    offset = 0
    tail = b""
    overlap = len(FUSE_SENTINEL) + 2 + 255
    with open(path, "rb") as binary:
        while True:
            chunk = binary.read(65536)
            data = tail + chunk
            index = data.find(FUSE_SENTINEL)
            while index >= 0:
                position = offset - len(tail) + index
                header = index + len(FUSE_SENTINEL)
                if len(data) >= header + 2:
                    version, length = data[header : header + 2]
                    if version != 1 or length < 4:
                        return False
                    if len(data) >= header + 2 + length:
                        # EnableNodeCliInspectArguments is fuse index 3.
                        found[position] = data[header + 2 + 3] == 0x31
                    elif not chunk:
                        return False
                elif not chunk:
                    return False
                index = data.find(FUSE_SENTINEL, index + 1)
            if not chunk:
                break
            offset += len(chunk)
            tail = data[-overlap:]
    return 1 <= len(found) <= 2 and all(found.values())


def inspector_supported(target: Target) -> bool:
    executable = Path(target.executable)
    binary = (
        executable.parent.parent / "Frameworks/Electron Framework.framework/Electron Framework"
        if sys.platform == "darwin"
        else executable
    )
    try:
        binary = binary.resolve(strict=True)
        stat = binary.stat()
        # If the on-disk image changed after exec, its fuses cannot attest to
        # the running image. Wait for a normal application restart.
        if stat.st_ctime > target.started:
            return False
        identity = (
            stat.st_dev,
            stat.st_ino,
            stat.st_size,
            stat.st_mtime_ns,
            stat.st_ctime_ns,
        )
        return _inspect_fuses(str(binary), identity)
    except (OSError, ValueError):
        return False
