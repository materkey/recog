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
