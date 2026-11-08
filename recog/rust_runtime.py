"""The Rust programs of the services besides live (`[web] runtime`).

With "rust", `python -m recog.web.server` execs the matching binary of rust/ (built by
setup.sh) with equivalent arguments, so the service units, recog-service.sh and the readiness
gate stay as they are: the exec keeps the unit's pid, environment and working
directory, and the binary resolves the same `.env`, `config.toml` and data directory
from the checkout it is given. Without the binary the service stays in Python and the
log says so in one line. The live process has its own hand-over in recog/cli.py.
"""

import os
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RELEASE_DIR = REPO_ROOT / "rust" / "target" / "release"
RUST_WEB = RELEASE_DIR / "recog-web"


def rust_program(key: str, runtime: str, binary: Path, prefix: str, service: str):
    """The binary when the setting `key` (e.g. `[web] runtime`) asks for "rust" and it
    is built; None keeps `service` in Python, with one log line when "rust" was asked.

    Any other value than "python" or "rust" stops startup naming the key.
    """
    if runtime not in ("python", "rust"):
        raise ValueError(f'{key} must be "python" or "rust", not {runtime!r}')
    if runtime == "python":
        return None
    if not (binary.is_file() and os.access(binary, os.X_OK)):
        print(
            f"{prefix} {binary} is not built (./setup.sh builds it when cargo is installed): "
            f"{service} runs in Python",
            file=sys.stderr,
            flush=True,
        )
        return None
    return binary


def exec_program(program: Path, argv: list[str], key: str) -> None:
    """Become `program` with `argv`; does not return.

    An exec, not a child: the service manager keeps watching the same pid, its
    signals reach the Rust process, and the environment (including what `.env` added
    on import) and the working directory carry over unchanged.
    """
    print(f'{key} = "rust": running {program}', file=sys.stderr, flush=True)
    sys.stdout.flush()
    os.execv(program, [str(program), *argv])


def web_argv(host: str, port: int, argv: list[str]) -> list[str]:
    """recog-web's arguments for the server Python would run on host:port.

    The checkout is passed explicitly (the binary would otherwise use the one it was
    built from); arguments given to `python -m recog.web.server`, which Python
    ignores, come last and so override the defaults (`--port` for a test server).
    """
    return ["--host", host, "--port", str(port), "--repo", str(REPO_ROOT), *argv]


def hand_over_web(host: str, port: int, argv: list[str]) -> None:
    """`python -m recog.web.server` under `[web] runtime = "rust"`; returns only when
    the server stays in Python."""
    from recog.install_config import INSTALL_CONFIG

    key = "[web] runtime"
    program = rust_program(key, INSTALL_CONFIG.web.runtime, RUST_WEB, "[web]", "the web server")
    if program is not None:
        exec_program(program, web_argv(host, port, argv), key)
