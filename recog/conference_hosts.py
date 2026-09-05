"""Installation-specific conference origins shared with the extension build."""

import json
import ipaddress
import os
from pathlib import Path
import re
from urllib.parse import urlsplit


PLATFORMS = frozenset({"ktalk", "vas3k", "jitsi", "matrix"})


def load_conference_hosts(path: Path | None = None) -> dict:
    if path is None:
        configured = os.environ.get("RECOG_CONFERENCE_HOSTS_FILE")
        path = (Path(configured).expanduser() if configured else
                Path(__file__).resolve().parent.parent / ".local/conference-hosts.json")
        if not path.is_absolute():
            path = Path(__file__).resolve().parent.parent / path
    if not path.exists():
        return {"jitsi": [], "matrix": [], "source_aliases": {}}
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or set(value) - {"jitsi", "matrix", "source_aliases"}:
        raise ValueError("Conference hosts must contain jitsi/matrix arrays and optional source_aliases")
    result = {"jitsi": [], "matrix": [], "source_aliases": value.get("source_aliases", {})}
    seen = set()
    for platform in ("jitsi", "matrix"):
        origins = value.get(platform, [])
        if not isinstance(origins, list):
            raise ValueError(f"{platform} must be an array of HTTPS origins")
        for origin in origins:
            if not isinstance(origin, str):
                raise ValueError("Conference origins must be strings")
            parsed = urlsplit(origin)
            hostname = parsed.hostname or ""
            if (not hostname or len(hostname) > 253
                    or any(not re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
                           for label in hostname.split("."))
                    or origin != f"https://{hostname}"
                    or hostname == "ktalk.ru" or hostname.endswith(".ktalk.ru")
                    or hostname == "meet.vas3k.cloud"):
                raise ValueError("Use canonical HTTPS origins without ports, paths or built-in hosts")
            if origin in seen:
                raise ValueError("Conference origins must be unique across platforms")
            # Browser URL parsing treats numeric last labels as IPv4. Require
            # the canonical dotted address rather than accepting its shorthands.
            last_label = hostname.rsplit(".", 1)[-1]
            if last_label.isdigit() or re.fullmatch(r"0x[0-9a-f]+", last_label):
                if str(ipaddress.IPv4Address(hostname)) != hostname:
                    raise ValueError("Use a canonical IPv4 address")
            seen.add(origin)
            result[platform].append(origin)
    aliases = result["source_aliases"]
    if not isinstance(aliases, dict) or any(
        not isinstance(key, str) or not key or key in PLATFORMS
        or not isinstance(target, str) or target not in {"jitsi", "matrix"}
        for key, target in aliases.items()
    ):
        raise ValueError("source_aliases must map legacy names to known conference platforms")
    return result


CONFERENCE_HOSTS = load_conference_hosts()


def canonical_source(source: str | None) -> str | None:
    """Resolve legacy installation labels without rewriting stored meetings."""
    return CONFERENCE_HOSTS["source_aliases"].get(source, source)
