"""End to end: each service's Python entry point with its runtime set to "rust" becomes
the Rust binary (recog/rust_runtime.py), which then answers.

Every process runs on a temporary data directory and HOME, on free ports (never the
installed services' 8765/8766/8767/9080), with no installation config. Skipped when
the release binaries are not built (`./setup.sh`, or the cargo call in the message).
"""

import json
import os
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

import pytest

from recog import rust_runtime

REPO_ROOT = Path(__file__).resolve().parent.parent
# The installed services' ports: a smoke test must never bind or reach them.
SERVICE_PORTS = {8765, 8766, 8767, 9080}
START_TIMEOUT_S = 30.0


def _require(binary: Path, build: str) -> None:
    if not (binary.is_file() and os.access(binary, os.X_OK)):
        pytest.skip(f"{binary} is not built: cd rust && {build}")


def _free_port() -> int:
    while True:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        if port not in SERVICE_PORTS:
            return port


def _env(tmp_path: Path, **extra: str) -> dict[str, str]:
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = dict(os.environ)
    env.update(
        HOME=str(home),
        RECOG_DATA_DIR=str(tmp_path / "data"),
        RECOG_CONFIG_FILE=str(tmp_path / "absent-config.toml"),
        RECOG_CONFERENCE_HOSTS_FILE=str(tmp_path / "absent-hosts.json"),
        PYTHONUNBUFFERED="1",
    )
    env.update(extra)
    return env


class Service:
    """`python -m <module> <args>` from the checkout, its stderr in a file."""

    def __init__(self, tmp_path: Path, module: str, args: list[str], env: dict[str, str]):
        self.stderr_path = tmp_path / f"{module}.stderr.log"
        self._stderr = self.stderr_path.open("wb")
        self.process = subprocess.Popen(
            [sys.executable, "-m", module, *args],
            cwd=REPO_ROOT,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=self._stderr,
        )

    def stderr(self) -> str:
        return self.stderr_path.read_text(errors="replace")

    def program(self) -> str:
        """argv[0] of the process now: the exec replaced the interpreter's."""
        return subprocess.run(
            ["ps", "-p", str(self.process.pid), "-o", "args="],
            capture_output=True,
            text=True,
        ).stdout.strip()

    def wait_for(self, probe, what: str):
        deadline = time.monotonic() + START_TIMEOUT_S
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                pytest.fail(
                    f"exited with {self.process.returncode} before {what}:\n{self.stderr()}"
                )
            try:
                result = probe()
            except OSError:
                result = None
            if result is not None:
                return result
            time.sleep(0.2)
        pytest.fail(f"no {what} within {START_TIMEOUT_S} s:\n{self.stderr()}")

    def stop(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self._stderr.close()


@pytest.fixture
def services():
    started: list[Service] = []
    yield started
    for service in started:
        service.stop()


def test_rust_web_runtime_starting_the_web_server_module_serves_from_recog_web(tmp_path, services):
    """given: [web] runtime = "rust", a built recog-web, a free port and no bridge
    when: `python -m recog.web.server --port <free>` starts
    then: the process becomes recog-web and GET /api/prompts lists the checkout's
          prompts, GET /api/meetings the empty data directory's meetings"""
    _require(rust_runtime.RUST_WEB, "cargo build --release -p recog-web --bin recog-web")
    port = _free_port()
    env = _env(
        tmp_path,
        RECOG_WEB_RUNTIME="rust",
        # The subscriber must not reach the installed bridge: nothing listens here.
        RECOG_BRIDGE_WS_URL=f"ws://127.0.0.1:{_free_port()}",
    )
    web = Service(tmp_path, "recog.web.server", ["--port", str(port)], env)
    services.append(web)

    def get(path):
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=2) as response:
            return response.status, json.loads(response.read())

    status, prompts = web.wait_for(lambda: get("/api/prompts"), "GET /api/prompts")

    assert status == 200
    assert {p["id"] for p in prompts} >= {
        p.stem for p in (REPO_ROOT / "recog/prompts").glob("*.md")
    }
    assert get("/api/meetings")[0] == 200
    assert web.program().startswith(str(rust_runtime.RUST_WEB))
    assert '[web] runtime = "rust": running' in web.stderr()


def test_rust_copilot_runtime_starting_the_channel_module_answers_mcp_initialize(
    tmp_path, services
):
    """given: [copilot] runtime = "rust", a built recog-copilot-channel, free ports and
          no bridge
    when: `python -m recog.copilot_channel --mcp-port <free> --bridge-url <nothing>`
          starts
    then: the process becomes the binary and answers an MCP initialize over HTTP with
          a session"""
    _require(
        rust_runtime.RUST_COPILOT_CHANNEL,
        "cargo build --release -p recog-copilot --bin recog-copilot-channel",
    )
    port = _free_port()
    args = ["--mcp-port", str(port), "--bridge-url", f"ws://127.0.0.1:{_free_port()}"]
    env = _env(tmp_path, RECOG_COPILOT_RUNTIME="rust")
    channel = Service(tmp_path, "recog.copilot_channel", args, env)
    services.append(channel)
    initialize = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "0"},
        },
    }

    def post():
        request = urllib.request.Request(
            f"http://127.0.0.1:{port}/mcp",
            data=json.dumps(initialize).encode(),
            headers={
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
            },
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            session = response.headers.get("mcp-session-id")
            for raw in response:
                line = raw.decode().strip()
                if line.startswith("data:"):
                    return session, json.loads(line[len("data:") :])
                if line.startswith("{"):
                    return session, json.loads(line)
        return None

    session, answer = channel.wait_for(post, "an answer to initialize")

    assert session
    assert answer["id"] == 1 and "serverInfo" in answer["result"]
    assert channel.program().startswith(str(rust_runtime.RUST_COPILOT_CHANNEL))
    assert '[copilot] runtime = "rust": running' in channel.stderr()


def _stub_commands(tmp_path: Path) -> Path:
    """tmux, systemd-run, systemctl and claude that only log and fail: the supervisor
    finds no session, and nothing reaches the user's `tmux -L recog-copilot`."""
    stubs = tmp_path / "stubs"
    stubs.mkdir()
    log = tmp_path / "stub.log"
    for name in ("tmux", "systemd-run", "systemctl", "claude"):
        stub = stubs / name
        stub.write_text(f"#!/bin/sh\necho \"{name} $*\" >> '{log}'\nexit 1\n")
        stub.chmod(0o755)
    return stubs


def test_rust_copilot_runtime_starting_the_supervisor_module_says_hello_to_the_bridge(
    tmp_path, services
):
    """given: [copilot] runtime = "rust", a built recog-copilot-supervisor, a fake
          bridge on a free port and a tmux that knows no session
    when: `python -m recog.copilot_supervisor --bridge-url <fake bridge>` starts
    then: the process becomes the binary, connects and sends supervisor_hello with no
          session to keep, and asked only the stub tmux"""
    _require(
        rust_runtime.RUST_COPILOT_SUPERVISOR,
        "cargo build --release -p recog-copilot --bin recog-copilot-supervisor",
    )
    from websockets.sync.server import serve

    frames: list[dict] = []

    def bridge(connection):
        for message in connection:
            frame = json.loads(message)
            frames.append(frame)
            if frame.get("type") == "supervisor_hello":
                connection.send(json.dumps({"type": "meeting_state", "active": False}))

    port = _free_port()
    stubs = _stub_commands(tmp_path)
    env = _env(
        tmp_path,
        RECOG_COPILOT_RUNTIME="rust",
        PATH=f"{stubs}{os.pathsep}{os.environ.get('PATH', '')}",
    )
    with serve(bridge, "127.0.0.1", port) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            supervisor = Service(
                tmp_path,
                "recog.copilot_supervisor",
                ["--bridge-url", f"ws://127.0.0.1:{port}"],
                env,
            )
            services.append(supervisor)

            hello = supervisor.wait_for(
                lambda: next((f for f in frames if f.get("type") == "supervisor_hello"), None),
                "supervisor_hello",
            )

            assert hello.get("current_meeting_id") is None
            assert supervisor.program().startswith(str(rust_runtime.RUST_COPILOT_SUPERVISOR))
            assert '[copilot] runtime = "rust": running' in supervisor.stderr()
            tmux_calls = [
                c for c in (tmp_path / "stub.log").read_text().splitlines() if c.startswith("tmux")
            ]
            assert tmux_calls and all("-L recog-copilot" in c for c in tmux_calls)
        finally:
            for service in services:
                service.stop()
            server.shutdown()
            thread.join(timeout=5)
