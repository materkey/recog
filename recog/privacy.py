"""Shared privacy preference and name masking at the Copilot MCP boundary."""

from collections import OrderedDict
import copy
import json
import os
from pathlib import Path
import tempfile

from recog.config import get_data_dir


class PrivacyStore:
    def __init__(self, data_dir: Path | None = None):
        self.path = (data_dir if data_dir is not None else get_data_dir()) / "privacy.json"

    def read(self) -> dict:
        try:
            with self.path.open() as stream:
                data = json.loads(stream.read(1024))
        except FileNotFoundError:
            return {"enabled": False, "configured": False}
        if not isinstance(data, dict) or type(data.get("enabled")) is not bool:
            raise ValueError("Invalid privacy setting")
        return {"enabled": data["enabled"], "configured": True}

    def write(self, enabled: bool) -> dict:
        if type(enabled) is not bool:
            raise ValueError("Privacy must be a boolean")
        self.read()
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        fd, temporary = tempfile.mkstemp(prefix=".privacy-", dir=self.path.parent)
        try:
            with os.fdopen(fd, "w") as stream:
                json.dump({"enabled": enabled}, stream)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        return {"enabled": enabled, "configured": True}


class CopilotPrivacy:
    def __init__(self, store: PrivacyStore | None = None):
        self.store = store if store is not None else PrivacyStore()
        self._meetings: OrderedDict[str | None, dict[str, str]] = OrderedDict()
        self._current_meeting = None

    def apply(self, payload: dict) -> dict:
        # Read at delivery, not enqueue time: enabling privacy must also mask
        # events that were queued before the user pressed the button.
        if not self.store.read()["enabled"]:
            return payload
        result = copy.deepcopy(payload)
        segments = result.get("segments") or []
        meeting = result.get("meeting_id", segments[0].get("meeting_id") if segments else self._current_meeting)
        self._current_meeting = meeting
        names = self._meetings.setdefault(meeting, {})
        self._meetings.move_to_end(meeting)
        if len(self._meetings) > 16:
            self._meetings.popitem(last=False)

        def alias(name):
            if not isinstance(name, str) or not name:
                return "speaker_?"
            return names.setdefault(name, f"speaker_{len(names)}")

        for key in ("recent_segments", "new_segments", "segments"):
            for segment in result.get(key) or []:
                if segment.get("event_type"):
                    continue
                masked = alias(segment.get("speaker"))
                segment["speaker"] = masked
                if "speaker_id" in segment:
                    segment["speaker_id"] = masked
        if "recent_context" in result:
            if "recent_segments" in result:
                result["recent_context"] = "\n".join(
                    f"{segment.get('speaker') or '?'}: {segment.get('text') or ''}"
                    for segment in result["recent_segments"]
                )
            else:
                # Compatibility with a bridge not yet restarted after upgrade.
                lines = []
                for line in result["recent_context"].splitlines():
                    name, separator, text = line.partition(": ")
                    lines.append(f"{alias(name)}: {text}" if separator else line)
                result["recent_context"] = "\n".join(lines)
        if "local_user_name" in result:
            result["local_user_name"] = alias(result["local_user_name"])
        if "local_user_aliases" in result:
            result["local_user_aliases"] = []
        if "local_user_identity_known" in result:
            result["local_user_identity_known"] = False
        if "local_user_profile_revision" in result:
            result["local_user_profile_revision"] = "privacy:" + str(result["local_user_profile_revision"])
        result["privacy_mode"] = True
        return result
