"""AI configuration persistence and acknowledgement of live Copilot policy."""

import asyncio
import json

from recog.ai_settings import AiSettingsStore


class AiSettingsCoordinator:
    def __init__(self, store: AiSettingsStore, broadcast):
        self.store = store
        self._broadcast = broadcast
        self._send = None
        self._applied_revision = None
        self._connection_generation = 0
        self._write_lock = asyncio.Lock()
        self._changed = asyncio.Event()
        self._error = None

    async def view(self):
        result = await asyncio.to_thread(self.store.view)
        return self._with_application(result)

    def _with_application(self, result):
        result["copilot_application"] = {
            "state": "applied" if self._send is not None and self._applied_revision == result["revision"] else "pending",
            "model_application": "next_session",
        }
        if self._error:
            result["copilot_application"]["error"] = self._error
        return result

    async def _publish(self):
        result = await self.view()
        await self._broadcast(json.dumps({"type": "ai_settings_status", "revision": result["revision"],
                                          "copilot_application": result["copilot_application"]}))

    async def connected(self, send):
        self._connection_generation += 1
        self._send = send
        self._applied_revision = None
        self._error = None
        snapshot = await asyncio.to_thread(self.store.read)
        await self._refresh(snapshot["revision"])
        await self._publish()

    async def disconnected(self):
        self._connection_generation += 1
        self._send = None
        self._applied_revision = None
        self._changed.set()
        await self._publish()

    async def _refresh(self, revision):
        self._changed.clear()
        if self._send is None:
            return False
        sent = await self._send(json.dumps({"type": "ai_settings_refresh", "revision": revision}))
        if not sent:
            self._applied_revision = None
        return sent

    async def write(self, operation, *args, expected_revision=None):
        async with self._write_lock:
            result = await asyncio.to_thread(operation, *args, expected_revision=expected_revision)
            self._applied_revision = None
            self._error = None
        sent = await self._refresh(result["revision"])
        await self._publish()
        if sent:
            try:
                await asyncio.wait_for(self._changed.wait(), timeout=1)
            except asyncio.TimeoutError:
                pass
        # Keep the revision of this write: another tab can save while we await
        # the bridge, and callers may retain an unsaved draft of another section.
        return self._with_application(result)

    async def handle_frame(self, frame):
        if frame.get("type") == "ai_settings_applied":
            generation = self._connection_generation
            async with self._write_lock:
                snapshot = await asyncio.to_thread(self.store.read)
                if generation != self._connection_generation or self._send is None or frame.get("revision") != snapshot["revision"]:
                    return
                self._applied_revision = snapshot["revision"]
                self._error = None
                self._changed.set()
        else:
            self._applied_revision = None
            self._error = "Настройки сохранены; Copilot пока не получил новые инструкции."
            self._changed.set()
        await self._publish()
