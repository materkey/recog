"""Bounded attachment worker. Never closes an inspector it did not open."""

import argparse
import asyncio
import fcntl
import json
import os
import signal
import sys
import time
import uuid
from contextlib import contextmanager
from pathlib import Path
from urllib.parse import urlsplit

import httpx
import websockets

from .processes import (
    INSPECTOR_PORT,
    Target,
    inspector_supported,
    owns_inspector,
    port_available,
    ready,
    same_process,
)
from .storage import read_json, validate_bundle, write_json, write_status

ATTACH_TIMEOUT = 10
CLEANUP_TIMEOUT = 5

REQUIRE = """(typeof require === 'function' ? require :
(process.mainModule?.require.bind(process.mainModule) ||
process.getBuiltinModule('module').createRequire(process.execPath)))"""
CLOSE = f"""(() => {{const inspector = {REQUIRE}('node:inspector');
const guard = globalThis.__recogInspectorGuard;
if (guard) {{ clearTimeout(guard.timer); delete globalThis.__recogInspectorGuard; }}
const endpoint = inspector.url();
setTimeout(() => {{ if (inspector.url() === endpoint) inspector.close(); }}, 100); return true;}})()"""


class AttachError(Exception):
    pass


@contextmanager
def attachment_lock():
    directory = Path.home() / ".cache" / "recog"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(directory / "desktop-attach.lock", os.O_CREAT | os.O_RDWR, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield
    finally:
        os.close(fd)


def inspector_url(targets) -> str:
    if not isinstance(targets, list) or len(targets) != 1:
        raise AttachError("ambiguous_inspector")
    url = targets[0].get("webSocketDebuggerUrl", "")
    parsed = urlsplit(url)
    if (
        parsed.scheme != "ws"
        or parsed.hostname != "127.0.0.1"
        or parsed.port != INSPECTOR_PORT
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
    ):
        raise AttachError("invalid_inspector_endpoint")
    return url


async def endpoint(target: Target, expected: str | None = None) -> str:
    if not await asyncio.to_thread(owns_inspector, target):
        raise AttachError("inspector_owner_mismatch")
    async with httpx.AsyncClient(trust_env=False, timeout=1) as client:
        response = await client.get(f"http://127.0.0.1:{INSPECTOR_PORT}/json/list")
        response.raise_for_status()
        if len(response.content) > 65536:
            raise AttachError("invalid_inspector_response")
        targets = response.json()
    url = inspector_url(targets)
    if expected and url != expected:
        raise AttachError("inspector_session_changed")
    if not await asyncio.to_thread(owns_inspector, target):
        raise AttachError("inspector_owner_mismatch")
    return url


async def evaluate(url: str, expression: str):
    # Node inspector is local. Never use an HTTP proxy or follow redirections.
    options = dict(open_timeout=1, close_timeout=1, max_size=131072)
    if int(websockets.__version__.split(".")[0]) >= 15:
        options["proxy"] = None
    async with websockets.connect(url, **options) as ws:
        await ws.send(
            json.dumps(
                {
                    "id": 1,
                    "method": "Runtime.evaluate",
                    "params": {
                        "expression": expression,
                        "awaitPromise": True,
                        "returnByValue": True,
                    },
                }
            )
        )
        while True:
            reply = json.loads(await ws.recv())
            if reply.get("id") != 1:
                continue
            result = reply.get("result", {})
            if reply.get("error") or result.get("exceptionDetails"):
                # Do not copy exception descriptions: they can contain page data.
                raise AttachError("inspector_evaluation_failed")
            return result.get("result", {}).get("value")


async def close_owned(operation: dict) -> bool:
    target = Target.from_dict(operation["target"])
    if not await asyncio.to_thread(same_process, target):
        return True
    if not operation.get("endpoint"):
        # A pre-signal journal entry cannot prove ownership of a later user
        # debugger. Never discover a new endpoint during recovery.
        return False
    deadline = time.monotonic() + CLEANUP_TIMEOUT
    while time.monotonic() < deadline:
        try:
            if not await asyncio.to_thread(owns_inspector, target):
                # A disappeared listener is success; an unexpected listener
                # on this PID must not be treated as closed.
                from .processes import listeners

                return not any(
                    port == INSPECTOR_PORT for _, port in await asyncio.to_thread(listeners, target)
                )
            url = await endpoint(target, operation.get("endpoint"))
            await asyncio.wait_for(evaluate(url, CLOSE), 1.5)
        except Exception:
            pass
        await asyncio.sleep(0.2)
    return False


async def bootstrap(url: str, root: Path, target: Target, bundle: Path, digest: str):
    identity = await evaluate(
        url,
        f"""(() => {{
const inspector = {REQUIRE}('node:inspector');
const previous = globalThis.__recogInspectorGuard;
if (previous) clearTimeout(previous.timer);
const guard = globalThis.__recogInspectorGuard = {{endpoint: inspector.url()}};
guard.timer = setTimeout(() => {{
  if (globalThis.__recogInspectorGuard === guard) {{
    delete globalThis.__recogInspectorGuard;
    if (inspector.url() === guard.endpoint) inspector.close();
  }}
}}, 8000).unref();
return {{pid: process.pid}};}})()""",
    )
    if identity != {"pid": target.pid}:
        raise AttachError("inspector_identity_mismatch")
    source = Path(__file__).with_name("bootstrap.js").read_text()
    response = await evaluate(
        url,
        source
        + "\nattachDesktop("
        + json.dumps(str(bundle))
        + ","
        + json.dumps(digest)
        + ","
        + json.dumps(str(root / "runtime.json"))
        + ","
        + json.dumps(target.to_dict())
        + ")",
    )
    if not isinstance(response, dict) or not response.get("app_version"):
        raise AttachError("invalid_bootstrap_response")
    return response


async def check_attachment_ready(target: Target):
    if not await asyncio.to_thread(port_available):
        raise AttachError("inspector_port_busy")
    if not await asyncio.to_thread(ready, target):
        raise AttachError("application_not_ready_or_debugging")
    if not await asyncio.to_thread(inspector_supported, target):
        raise AttachError("inspector_disabled_or_unverified")


async def attach(root: Path, target: Target) -> dict:
    config = read_json(root / "config.json")
    bundle = validate_bundle(root, config)
    app = Path(config["app"])
    if not await asyncio.to_thread(same_process, target, app):
        raise AttachError("process_changed")
    previous = read_json(root / "operation.json")
    if previous.get("phase") not in (None, "closed"):
        if not await close_owned(previous):
            return {
                "state": "inspector_unclosed",
                "reason": "cleanup_failed",
                "inspector_closed": False,
                "target": previous["target"],
            }
        write_json(root / "operation.json", {**previous, "phase": "closed"})
    await check_attachment_ready(target)
    operation = {
        "target": target.to_dict(),
        "phase": "opening",
        "token": uuid.uuid4().hex,
        "opened_at": time.time(),
    }
    write_json(root / "operation.json", operation)
    result = {
        "state": "error",
        "reason": "attachment_failed",
        "target": target.to_dict(),
    }
    signalled = False
    try:
        async with asyncio.timeout(ATTACH_TIMEOUT):
            if not await asyncio.to_thread(same_process, target, app):
                raise AttachError("process_changed")
            os.kill(target.pid, signal.SIGUSR1)
            signalled = True
            while not await asyncio.to_thread(owns_inspector, target):
                if not await asyncio.to_thread(same_process, target):
                    raise AttachError("process_exited")
                await asyncio.sleep(0.05)
            url = await endpoint(target)
            operation.update(endpoint=url, phase="opened")
            write_json(root / "operation.json", operation)
            response = await bootstrap(url, root, target, bundle, config["bundle"])
            result.update(state="attached", reason=None, **response)
    except TimeoutError:
        result["reason"] = "attachment_timeout"
    except AttachError as exc:
        result["reason"] = str(exc)
    except Exception:
        result["reason"] = "attachment_failed"
    finally:
        closed = await close_owned(operation) if signalled else True
        result["inspector_closed"] = closed
        if closed:
            write_json(root / "operation.json", {**operation, "phase": "closed"})
        else:
            result.update(state="inspector_unclosed", reason="cleanup_failed")
    return result


def run(root: Path, target: Target | None = None, recover=False) -> dict:
    try:
        with attachment_lock():
            if recover:
                operation = read_json(root / "operation.json")
                if not operation or operation.get("phase") == "closed":
                    return {"state": "waiting", "inspector_closed": True}
                closed = asyncio.run(close_owned(operation))
                if closed:
                    write_json(root / "operation.json", {**operation, "phase": "closed"})
                result = {
                    "state": "error" if closed else "inspector_unclosed",
                    "reason": "worker_interrupted" if closed else "cleanup_failed",
                    "inspector_closed": closed,
                    "target": operation["target"],
                }
            else:
                result = asyncio.run(attach(root, target))
    except BlockingIOError:
        existing = read_json(root / "status.json")
        if existing.get("state") == "inspector_unclosed":
            return existing
        result = {
            "state": "error",
            "reason": "attachment_in_progress",
            "inspector_closed": None,
        }
    except AttachError as exc:
        result = {
            "state": "incompatible" if str(exc) == "inspector_disabled_or_unverified" else "error",
            "reason": str(exc),
            "inspector_closed": None,
        }
    except Exception:
        result = {
            "state": "error",
            "reason": "invalid_installation",
            "inspector_closed": None,
        }
    result["updated_at"] = time.time()
    if target:
        result.setdefault("target", target.to_dict())
    return write_status(root, result)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--target", type=json.loads)
    parser.add_argument("--recover", action="store_true")
    args = parser.parse_args()
    result = run(args.root, Target.from_dict(args.target) if args.target else None, args.recover)
    sys.exit(0 if result["state"] in ("attached", "waiting") else 1)
