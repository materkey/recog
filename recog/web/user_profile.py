"""Persist profile edits and track acknowledgement from the live process."""

import asyncio
import json
from typing import Awaitable, Callable

from recog.user_profile import UserProfileStore, resolve_identity


class ProfileCoordinator:
    def __init__(self, store: UserProfileStore, legacy_name: str,
                 broadcast: Callable[[str], Awaitable[None]]):
        self.store = store
        self.legacy_name = legacy_name
        self._broadcast = broadcast
        self._send = None
        self._applied_identity = None
        self._changed = asyncio.Event()
        self._write_lock = asyncio.Lock()
        self._error = None
        self._connection_generation = 0

    async def snapshot(self):
        profile = await asyncio.to_thread(self.store.read)
        applied = (self._send is not None and self._applied_identity is not None
                   and self._applied_identity["local_user_profile_revision"] == profile["revision"])
        suggested = self.legacy_name.strip()
        if suggested.casefold() == "me":
            suggested = ""
        result = {
            "profile": profile,
            "suggested_name": suggested,
            "identity": self._applied_identity or resolve_identity(profile, legacy_name=self.legacy_name),
            "application_state": "applied" if applied else "pending",
        }
        if self._error:
            result["error"] = self._error
        return result

    async def _publish(self):
        snapshot = await self.snapshot()
        await self._broadcast(json.dumps({"type": "user_profile_status", **snapshot}, ensure_ascii=False))

    async def connected(self, send):
        self._connection_generation += 1
        self._send = send
        self._applied_identity = None
        self._error = None
        # Do not wait for an acknowledgement here: the subscriber receive loop
        # starts after this callback returns.
        await self._refresh(await asyncio.to_thread(self.store.read))
        await self._publish()

    async def disconnected(self):
        self._connection_generation += 1
        self._send = None
        self._applied_identity = None
        self._changed.set()
        await self._publish()

    async def _refresh(self, profile):
        self._changed.clear()
        if self._send is None:
            return False
        sent = await self._send(json.dumps({"type": "user_profile_refresh", "revision": profile["revision"]}))
        if not sent:
            self._applied_identity = None
        return sent

    async def save(self, name, aliases):
        return await self._write(self.store.write, name, aliases)

    async def clear(self):
        return await self._write(self.store.clear)

    async def _write(self, operation, *args):
        async with self._write_lock:
            profile = await asyncio.to_thread(operation, *args)
            self._error = None
            self._applied_identity = None
        # A sender may deliver the acknowledgement immediately. Never hold the
        # persistence/acknowledgement lock while awaiting transport I/O.
        sent = await self._refresh(profile)
        await self._publish()
        if sent:
            try:
                await asyncio.wait_for(self._changed.wait(), timeout=1.0)
            except asyncio.TimeoutError:
                pass
        return await self.snapshot()

    async def handle_frame(self, frame):
        kind = frame.get("type")
        if kind == "user_profile_applied":
            identity = frame.get("identity")
            connection = self._connection_generation
            async with self._write_lock:
                profile = await asyncio.to_thread(self.store.read)
                if (connection != self._connection_generation or self._send is None
                        or not isinstance(identity, dict)
                        or frame.get("revision") != profile["revision"]
                        or identity.get("local_user_profile_revision") != profile["revision"]
                        or not isinstance(identity.get("local_user_name"), str)
                        or not isinstance(identity.get("local_user_aliases"), list)
                        or not all(isinstance(alias, str) for alias in identity["local_user_aliases"])
                        or not isinstance(identity.get("local_user_identity_known"), bool)):
                    return
                self._applied_identity = identity
                self._error = None
                self._changed.set()
            await self._publish()
        elif kind == "user_profile_error":
            self._applied_identity = None
            self._error = "Профиль сохранён, но пока не применён к записи."
            self._changed.set()
            await self._publish()
