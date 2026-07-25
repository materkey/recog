"""Shared WS Origin allowlist for the bridge and frontend live-stream endpoints.

WebSockets bypass CORS: any webpage can open a socket to a localhost port
and speak the protocol. Both WS servers (`ktalk_bridge._handle_client`,
`frontend/server.live_stream`) call `origin_allowed()` at accept-time to
reject browser pages that are not the Chrome extension's conference hosts
(mirroring `kontur-diarize/ktalk-speaker-extension/manifest.json`) or the
webapp itself.

Absent Origin header (``None``) is allowed — non-browser clients (the
supervisor, the copilot-channel daemon, `BridgeSubscriber`) send none.
The literal string ``"null"`` (sandboxed iframe) is NOT an absent header
and is rejected.
"""

from urllib.parse import urlsplit

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


def origin_allowed(origin: str | None) -> bool:
    """Return True if a WS connection with this Origin header may proceed."""
    if origin is None:
        return True
    if origin in ALLOWED_ORIGINS:
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
