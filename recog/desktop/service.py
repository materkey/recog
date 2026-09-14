"""Low-cost observer hosted by the web server; inspector work lives in a child."""

import asyncio
import fcntl
import json
import logging
import os
import shutil
import sys
import time
from pathlib import Path

from recog.log_utils import log_rate_limited

from .processes import (
    Target,
    application_path,
    find_targets,
    inspector_supported,
    port_available,
    ready,
    same_process,
)
from .storage import read_json, write_json, write_status

log = logging.getLogger(__name__)
POLL_SECONDS = 2
RETRYABLE = {
    "inspector_port_busy",
    "application_not_ready_or_debugging",
    "attachment_in_progress",
}


def status(root: Path) -> dict:
    try:
        existing = read_json(root / "status.json")
        if existing.get("state") == "inspector_unclosed":
            try:
                config = read_json(root / "config.json")
                enabled = bool(config.get("enabled"))
            except (OSError, ValueError, TypeError, AttributeError):
                enabled = True
            return {
                **existing,
                "enabled": enabled,
                "app_version": existing.get("app_version"),
                "inspector_closed": False,
            }
        config = read_json(root / "config.json")
        if not config.get("enabled", False):
            return {
                "enabled": False,
                "state": "disabled",
                "reason": None,
                "app_version": None,
                "inspector_closed": None,
            }
        result = read_json(root / "status.json", {"state": "waiting"})
        heartbeat = read_json(root / "heartbeat.json")
        if not heartbeat.get("running") or time.time() - heartbeat.get("updated_at", 0) > 8:
            if result.get("state") != "inspector_unclosed":
                result = {
                    **result,
                    "state": "server_offline",
                    "reason": "web_server_not_running",
                }
        return {
            "enabled": True,
            "app_version": None,
            "reason": None,
            "inspector_closed": None,
            **result,
        }
    except (OSError, ValueError, TypeError, AttributeError):
        return {
            "enabled": True,
            "state": "error",
            "reason": "invalid_configuration",
            "app_version": None,
            "inspector_closed": None,
        }


async def launch_worker(root: Path, target: Target | None = None, recover=False) -> dict:
    args = [sys.executable, "-m", "recog.desktop.worker", "--root", str(root)]
    if recover:
        args.append("--recover")
    else:
        args += ["--target", json.dumps(target.to_dict())]
    started = time.time()
    child = await asyncio.create_subprocess_exec(
        *args,
        stdout=asyncio.subprocess.DEVNULL,
        stderr=asyncio.subprocess.DEVNULL,
        cwd=str(Path(__file__).resolve().parents[2]),
    )
    try:
        deadline = time.monotonic() + 17
        while child.returncode is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError
            try:
                await asyncio.wait_for(child.wait(), min(1, remaining))
            except TimeoutError:
                await asyncio.to_thread(
                    write_json,
                    root / "heartbeat.json",
                    {"running": True, "updated_at": time.time()},
                )
    except TimeoutError:
        child.kill()  # Only our worker, never KTalk.
        await child.wait()
    operation = await asyncio.to_thread(read_json, root / "operation.json")
    if not recover and operation.get("phase") not in (None, "closed"):
        return await launch_worker(root, recover=True)
    result = await asyncio.to_thread(read_json, root / "status.json")
    if result.get("state") != "inspector_unclosed" and (
        result.get("updated_at", 0) < started or result.get("state") == "attaching"
    ):
        result = {
            "state": "error",
            "reason": "worker_failed",
            "inspector_closed": None,
            "updated_at": time.time(),
        }
        if target:
            result["target"] = target.to_dict()
        await asyncio.to_thread(write_status, root, result)
    return result


class DesktopIntegration:
    def __init__(self, root: Path):
        self.root = root
        self._task = None
        self._stop = asyncio.Event()
        self._lock_fd = None
        self._attempted = set()
        self._recovered = set()
        self._notified = set()

    async def start(self):
        if self._task:
            return
        self._stop.clear()
        self._task = asyncio.create_task(self._run(), name="desktop-observer")

    async def stop(self):
        self._stop.set()
        if self._task:
            # Let the worker finish cleanup instead of cancelling an open
            # inspector operation at web-server shutdown.
            await self._task
            self._task = None

    def _claim(self):
        if self._lock_fd is not None:
            return True
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        fd = os.open(self.root / "observer.lock", os.O_CREAT | os.O_RDWR, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(fd)
            return False
        self._lock_fd = fd
        return True

    async def _save(self, result):
        await asyncio.to_thread(write_status, self.root, {**result, "updated_at": time.time()})

    async def _notify(self, result):
        if result.get("state") not in ("error", "incompatible", "inspector_unclosed"):
            return
        key = (result.get("reason"), json.dumps(result.get("target")))
        if key in self._notified:
            return
        self._notified.add(key)
        message = (
            "Не удалось закрыть отладочное подключение. Перезапустите Толк после встречи."
            if result["state"] == "inspector_unclosed"
            else "Не удалось подключить Recog к Толку. Подробности в настройках Recog."
        )
        if sys.platform == "darwin":
            args = [
                "osascript",
                "-e",
                "display notification "
                + json.dumps(message, ensure_ascii=False)
                + ' with title "Recog"',
            ]
        elif shutil.which("notify-send"):
            args = ["notify-send", "Recog", message]
        else:
            return
        try:
            child = await asyncio.create_subprocess_exec(
                *args,
                stdout=asyncio.subprocess.DEVNULL,
                stderr=asyncio.subprocess.DEVNULL,
            )
            try:
                await asyncio.wait_for(child.wait(), 2)
            except TimeoutError:
                child.terminate()
                await child.wait()
        except OSError:
            log.warning("Desktop integration notification unavailable")

    async def _tick(self):
        config = await asyncio.to_thread(read_json, self.root / "config.json")
        operation = await asyncio.to_thread(read_json, self.root / "operation.json")
        if not config and not operation:
            return
        if not await asyncio.to_thread(self._claim):
            return
        await asyncio.to_thread(
            write_json,
            self.root / "heartbeat.json",
            {"running": True, "updated_at": time.time()},
        )
        if operation.get("phase") not in (None, "closed"):
            await self._recover_operation(operation, config)
            return
        if not config.get("enabled", False):
            return
        app = await asyncio.to_thread(application_path, config["app"])
        targets = await asyncio.to_thread(find_targets, app)
        if len(targets) != 1:
            await self._save(
                {
                    "state": "waiting",
                    "reason": "multiple_processes" if targets else None,
                    "inspector_closed": None,
                }
            )
            return
        target = targets[0]
        await self._attach_target(target)

    async def _recover_operation(self, operation, config):
        if not await asyncio.to_thread(same_process, Target.from_dict(operation["target"])):
            await asyncio.to_thread(
                write_json,
                self.root / "operation.json",
                {**operation, "phase": "closed"},
            )
            await self._save(
                {
                    "state": "waiting" if config.get("enabled", False) else "disabled",
                    "reason": None,
                    "inspector_closed": True,
                }
            )
            return
        token = operation.get("token")
        if token not in self._recovered:
            result = await launch_worker(self.root, recover=True)
            if result.get("reason") != "attachment_in_progress":
                self._recovered.add(token)
            await self._notify(result)

    async def _attach_target(self, target):
        previous = await asyncio.to_thread(read_json, self.root / "status.json")
        if previous.get("target") == target.to_dict() and previous.get("state") in (
            "attached",
            "incompatible",
            "error",
            "inspector_unclosed",
        ):
            if previous.get("reason") not in RETRYABLE:
                self._attempted.add(target.key)
        runtime = await asyncio.to_thread(read_json, self.root / "runtime.json")
        if runtime.get("target") == target.to_dict() and runtime.get("state") == "incompatible":
            result = {
                **previous,
                **runtime,
                "inspector_closed": previous.get("inspector_closed"),
            }
            await self._save(result)
            await self._notify(result)
            self._attempted.add(target.key)
        elif (
            runtime.get("target") == target.to_dict()
            and runtime.get("state") == "ready"
            and previous.get("target") == target.to_dict()
            and previous.get("state") == "incompatible"
            and previous.get("reason") == "page_initialization_failed"
        ):
            # A later page initialized fine, so the persisted page failure is stale.
            await self._save({**previous, "state": "attached", "reason": None})
        if target.key in self._attempted:
            return
        if not await asyncio.to_thread(ready, target):
            await self._save(
                {
                    "state": "waiting",
                    "reason": "application_not_ready_or_debugging",
                    "target": target.to_dict(),
                    "inspector_closed": None,
                }
            )
            return
        if not await asyncio.to_thread(port_available):
            await self._save(
                {
                    "state": "error",
                    "reason": "inspector_port_busy",
                    "target": target.to_dict(),
                    "inspector_closed": None,
                }
            )
            return
        if not await asyncio.to_thread(inspector_supported, target):
            result = {
                "state": "incompatible",
                "reason": "inspector_disabled_or_unverified",
                "target": target.to_dict(),
                "inspector_closed": None,
            }
            await self._save(result)
            self._attempted.add(target.key)
            await self._notify(result)
            return
        await self._save(
            {"state": "attaching", "target": target.to_dict(), "inspector_closed": None}
        )
        result = await launch_worker(self.root, target)
        if result.get("reason") not in RETRYABLE:
            self._attempted.add(target.key)
        await self._notify(result)

    async def _run(self):
        try:
            while not self._stop.is_set():
                try:
                    await self._tick()
                except Exception as exc:
                    # A persistent cause (a renamed AppImage, say) fails every 2 s tick.
                    log_rate_limited(
                        f"desktop-observer:{type(exc).__name__}",
                        f"Desktop observer iteration failed: {type(exc).__name__}: {exc}",
                        interval_s=60.0,
                    )
                    if self._lock_fd is not None:
                        result = {
                            "state": "error",
                            "reason": "observer_failed",
                            "inspector_closed": None,
                        }
                        await self._save(result)
                        await self._notify(result)
                try:
                    await asyncio.wait_for(self._stop.wait(), POLL_SECONDS)
                except TimeoutError:
                    pass
        finally:
            if self._lock_fd is not None:
                await asyncio.to_thread(
                    write_json,
                    self.root / "heartbeat.json",
                    {"running": False, "updated_at": time.time()},
                )
                os.close(self._lock_fd)
                self._lock_fd = None
