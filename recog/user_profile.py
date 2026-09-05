"""Private local addressing profile shared by the frontend and live pipeline."""

import json
import os
from pathlib import Path
import tempfile
import threading
import unicodedata
import uuid


MAX_NAME_LENGTH = 80
MAX_ALIASES = 16
_MAX_FILE_BYTES = 16384
_locks_guard = threading.Lock()
_writer_locks: dict[Path, threading.Lock] = {}


def _label(value: str) -> str:
    if not isinstance(value, str):
        raise ValueError("Profile names must be strings")
    if any(unicodedata.category(char).startswith("C") for char in value):
        raise ValueError("Profile names cannot contain control characters")
    value = value.strip()
    if len(value) > MAX_NAME_LENGTH:
        raise ValueError(f"Profile names must be at most {MAX_NAME_LENGTH} characters")
    return value


def _is_name(value: str) -> bool:
    return bool(value) and value.casefold() != "me"


def _aliases(values: list[str]) -> list[str]:
    if not isinstance(values, list) or len(values) > MAX_ALIASES:
        raise ValueError(f"Profile aliases must be a list of at most {MAX_ALIASES} names")
    return _unique_names(_label(value) for value in values)


def _unique_names(values) -> list[str]:
    result = []
    seen = set()
    for value in values:
        key = value.casefold()
        if _is_name(value) and key not in seen:
            result.append(value)
            seen.add(key)
    return result


def _profile(name: str, aliases: list[str], revision: str) -> dict:
    return {"version": 1, "revision": revision, "name": name,
            "aliases": aliases, "configured": bool(name)}


class UserProfileStore:
    def __init__(self, data_dir: Path):
        self.path = Path(data_dir) / "user-profile.json"
        with _locks_guard:
            self._writer_lock = _writer_locks.setdefault(self.path.resolve(), threading.Lock())

    def read(self) -> dict:
        try:
            with self.path.open("rb") as stream:
                raw = stream.read(_MAX_FILE_BYTES + 1)
        except FileNotFoundError:
            return _profile("", [], "unconfigured")
        if len(raw) > _MAX_FILE_BYTES:
            raise ValueError("Profile file is too large")
        try:
            saved = json.loads(raw)
        except (ValueError, UnicodeError) as error:
            raise ValueError("Profile file is not valid JSON") from error
        if not isinstance(saved, dict) or type(saved.get("version")) is not int or saved["version"] != 1:
            raise ValueError("Unsupported profile version")
        revision = saved.get("revision")
        if (not isinstance(revision, str) or not revision.strip() or len(revision) > 128
                or any(unicodedata.category(char).startswith("C") for char in revision)):
            raise ValueError("Invalid profile revision")
        name = _label(saved.get("name"))
        aliases = _aliases(saved.get("aliases"))
        if name and not _is_name(name):
            raise ValueError("Profile name cannot be Me")
        if not name and aliases:
            raise ValueError("An empty profile cannot have aliases")
        return _profile(name, aliases, revision)

    def write(self, name: str, aliases: list[str]) -> dict:
        name = _label(name)
        if not _is_name(name):
            raise ValueError("Profile name must be nonempty and cannot be Me")
        return self._save(name, _aliases(aliases))

    def clear(self) -> dict:
        return self._save("", [])

    def _save(self, name: str, aliases: list[str]) -> dict:
        with self._writer_lock:
            # A broken existing profile needs an explicit repair, never silent replacement.
            self.read()
            result = _profile(name, aliases, str(uuid.uuid4()))
            saved = {key: value for key, value in result.items() if key != "configured"}
            self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            fd, temporary = tempfile.mkstemp(prefix=".user-profile-", suffix=".tmp", dir=self.path.parent)
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as stream:
                    os.fchmod(stream.fileno(), 0o600)
                    json.dump(saved, stream, ensure_ascii=False)
                    stream.write("\n")
                    stream.flush()
                    os.fsync(stream.fileno())
                os.replace(temporary, self.path)
            finally:
                if os.path.exists(temporary):
                    os.unlink(temporary)
            return result


def resolve_identity(profile: dict, *, legacy_name: str = "Me", override: str | None = None) -> dict:
    """CLI labels win for this run; saving a profile retires the legacy alias."""
    canonical = profile["name"]
    explicit = override.strip() if isinstance(override, str) else ""
    legacy = legacy_name.strip() if isinstance(legacy_name, str) else ""
    effective = explicit or canonical or legacy or "Me"
    aliases = _unique_names([canonical, effective, *profile["aliases"]])
    return {
        "local_user_name": effective,
        "local_user_aliases": aliases,
        "local_user_profile_revision": profile["revision"],
        "local_user_identity_known": bool(aliases),
    }
