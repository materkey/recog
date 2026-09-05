"""Shared WS Origin allowlist for the bridge and frontend live-stream endpoints.

WebSockets bypass CORS: any webpage can open a socket to a localhost port
and speak the protocol. Both WS servers (`ktalk_bridge._handle_client`,
`recog/web/server.live_stream`) call `origin_allowed()` at accept-time to
reject browser pages that are not the Chrome extension's conference hosts
(mirroring `browser-extension/manifest.json`) or the
webapp itself.

Absent Origin header (``None``) is allowed — non-browser clients (the
supervisor, the copilot-channel daemon, `BridgeSubscriber`) send none.
The literal string ``"null"`` (sandboxed iframe) is NOT an absent header
and is rejected.

An installation whose conferences live on other hosts extends the list via
``RECOG_EXTRA_WS_ORIGINS`` (comma-separated exact origins) — kept as a
separate set so ``ALLOWED_ORIGINS`` stays a literal mirror of the
extension's `manifest.json` (tests assert that equality). The same host
must be added to `manifest.json` and the extension rebuilt, otherwise no
page ever opens the socket in the first place.
"""

from typing import Iterable, Optional
from urllib.parse import urlsplit

from .config import RECOG_EXTRA_WS_ORIGINS

# Exact-match origins: Jitsi/Element conference hosts used by the extension
# (https only) plus the webapp's own origin on port 9080.
ALLOWED_ORIGINS = frozenset({
    "https://meet.vas3k.cloud",
    "https://meet.vkkovalev.work",
    "https://element.vkkovalev.work",
    "http://127.0.0.1:9080",
    "http://localhost:9080",
})

# Wildcard rule: https://ktalk.ru and any https://*.ktalk.ru subdomain.
KTALK_WILDCARD_DOMAIN = "ktalk.ru"


def parse_extra_origins(raw: str | None) -> frozenset[str]:
    """Split ``RECOG_EXTRA_WS_ORIGINS`` into a set of exact origins."""
    if not raw:
        return frozenset()
    return frozenset(item.strip() for item in raw.split(",") if item.strip())


# Installation-specific exact origins from the environment (empty by default).
EXTRA_ORIGINS = parse_extra_origins(RECOG_EXTRA_WS_ORIGINS)


def origin_allowed(
    origin: str | None,
    extra: Optional[Iterable[str]] = None,
) -> bool:
    """Return True if a WS connection with this Origin header may proceed.

    ``extra`` — additional exact-match origins; they never widen the ktalk.ru
    wildcard rule. Omitted means the configured ``EXTRA_ORIGINS``: threading
    the module's own constant through every call site made forgetting the
    argument silently drop the operator's configured hosts. Pass an explicit
    (possibly empty) iterable to override.
    """
    if origin is None:
        return True
    if origin in ALLOWED_ORIGINS:
        return True
    if extra is None:
        extra = EXTRA_ORIGINS
    if origin in extra:
        return True
    if any(char.isspace() for char in origin):
        return False
    # ktalk.ru wildcard: parse the hostname — a raw suffix check on the
    # origin string would admit https://evil-ktalk.ru.
    try:
        parts = urlsplit(origin)
        hostname = parts.hostname
        port = parts.port
    except ValueError:
        return False
    if parts.scheme != "https":
        return False
    if (
        parts.username is not None
        or parts.password is not None
        or parts.path
        or parts.query
        or parts.fragment
        or port not in (None, 443)
    ):
        return False
    if not hostname or len(hostname) > 253:
        return False
    labels = hostname.split(".")
    if any(
        not label
        or len(label) > 63
        or not label.isascii()
        or not label[0].isalnum()
        or not label[-1].isalnum()
        or any(not (char.isalnum() or char == "-") for char in label)
        for label in labels
    ):
        return False
    return (
        hostname == KTALK_WILDCARD_DOMAIN
        or hostname.endswith("." + KTALK_WILDCARD_DOMAIN)
    )
