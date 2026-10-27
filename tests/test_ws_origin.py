"""Tests for recog/ws_origin.py — the shared WS Origin allowlist predicate."""
import json
import sys
from pathlib import Path
from unittest import mock

import pytest

PROJECT_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT_ROOT))

from recog import ws_origin
from recog.ws_origin import (
    ALLOWED_ORIGINS,
    WILDCARD_DOMAINS,
    origin_allowed,
)


@pytest.mark.parametrize("origin", [
    "https://example.ktalk.ru",
    "https://ktalk.ru",
    "https://deep.sub.ktalk.ru",
    "https://ktalk.ru:443",
])
def test_given_ktalk_https_origin_when_checked_then_allowed(origin):
    """given: an https origin on ktalk.ru or one of its subdomains
    when: origin_allowed is evaluated
    then: the connection is allowed
    """
    assert origin_allowed(origin) is True


@pytest.mark.parametrize(
    "origin",
    [
        "https://app.zoom.us",
        "https://zoom.us",
        "https://us05web.zoom.us",
    ],
)
def test_given_zoom_https_origin_when_checked_then_allowed(origin):
    """given: an https origin on zoom.us or one of its subdomains — the web
    client runs on app.zoom.us and on regional hosts
    when: origin_allowed is evaluated
    then: the connection is allowed
    """
    assert origin_allowed(origin) is True


@pytest.mark.parametrize(
    "origin",
    [
        "https://evil-zoom.us",
        "https://evilzoom.us",
        "https://zoom.us.evil.example",
        "http://app.zoom.us",
        "https://app.zoom.us:8443",
    ],
)
def test_given_zoom_lookalike_or_insecure_origin_when_checked_then_rejected(origin):
    """given: a host that only resembles zoom.us, or zoom.us over http or on
    a non-default port
    when: origin_allowed is evaluated
    then: the connection is rejected
    """
    assert origin_allowed(origin) is False


@pytest.mark.parametrize("origin", [
    "https://meet.vas3k.cloud",
])
def test_given_exact_conference_origin_when_checked_then_allowed(origin):
    """given: an exact conference-page origin from the extension manifest
    when: origin_allowed is evaluated
    then: the connection is allowed
    """
    assert origin_allowed(origin) is True


@pytest.mark.parametrize("origin", [
    "http://127.0.0.1:9080",
    "http://localhost:9080",
])
def test_given_webapp_origin_when_checked_then_allowed(origin):
    """given: the webapp origin on port 9080
    when: origin_allowed is evaluated
    then: the connection is allowed
    """
    assert origin_allowed(origin) is True


def test_given_absent_origin_header_when_checked_then_allowed():
    """given: no Origin header at all (Python clients: supervisor,
    copilot-channel daemon, BridgeSubscriber)
    when: origin_allowed is evaluated with None
    then: the connection is allowed
    """
    assert origin_allowed(None) is True


def test_given_unlisted_origin_when_checked_then_rejected():
    """given: an origin not on the allowlist
    when: origin_allowed is evaluated
    then: the connection is rejected
    """
    assert origin_allowed("https://evil.example") is False


def test_given_suffix_spoof_origin_when_checked_then_rejected():
    """given: a hostname that merely ends with the ktalk.ru string
    when: origin_allowed is evaluated
    then: the connection is rejected (hostname parse, not suffix match)
    """
    assert origin_allowed("https://evil-ktalk.ru") is False


def test_given_http_ktalk_origin_when_checked_then_rejected():
    """given: a conference host over plain http
    when: origin_allowed is evaluated
    then: the connection is rejected (conference origins are https-only)
    """
    assert origin_allowed("http://example.ktalk.ru") is False


def test_given_null_origin_when_checked_then_rejected():
    """given: the literal string "null" (sandboxed iframe)
    when: origin_allowed is evaluated
    then: the connection is rejected — it is not an absent header
    """
    assert origin_allowed("null") is False


@pytest.mark.parametrize("origin", [
    "",
    "https://",
    "not a url",
    "https://ktalk.ru.evil.example",
    "https://[invalid",
    "https://ktalk.ru/path",
    "https://user@ktalk.ru",
    "https://ktalk.ru?query",
    "https://ktalk.ru#fragment",
    "https://ktalk.ru:444",
    "https://ktalk.ru:bad",
    "https://ktalk.ru:99999",
    "https://.ktalk.ru",
    "https://evil..ktalk.ru",
    "https://evil%00.ktalk.ru",
    "https://-evil.ktalk.ru",
    "https://evil-.ktalk.ru",
])
def test_given_malformed_or_lookalike_origin_when_checked_then_rejected(origin):
    """given: a malformed origin or a ktalk.ru-prefixed lookalike domain
    when: origin_allowed is evaluated
    then: the connection is rejected without raising
    """
    assert origin_allowed(origin) is False


def test_given_extension_manifest_when_compared_then_allowlist_matches_both_ways():
    """given: the extension manifest's content_scripts host patterns —
    the source of truth the allowlist must mirror
    when: each pattern is mapped to a representative page origin
    then: origin_allowed accepts every one, so a platform host added to
          the manifest without touching the allowlist fails here
    """
    manifest_path = (
        PROJECT_ROOT / "browser-extension" / "manifest.json"
    )
    manifest = json.loads(manifest_path.read_text())

    exact_origins: set[str] = set()
    wildcard_hosts: set[str] = set()
    for entry in manifest["content_scripts"]:
        for pattern in entry["matches"]:
            # Pattern shape: <scheme>://<host>/<path>, e.g.
            # https://*.ktalk.ru/* or https://meet.vas3k.cloud/*
            scheme, rest = pattern.split("://", 1)
            assert scheme == "https", f"unexpected scheme in {pattern!r}"
            host = rest.split("/", 1)[0]
            if host.startswith("*."):
                wildcard_hosts.add(host)
            else:
                exact_origins.add(f"https://{host}")

    webapp_origins = {
        "http://127.0.0.1:9080",
        "http://localhost:9080",
    }
    assert wildcard_hosts == {f"*.{domain}" for domain in WILDCARD_DOMAINS}
    assert ALLOWED_ORIGINS == exact_origins | webapp_origins


# -----------------------------------------------------------------------------
# RECOG_EXTRA_WS_ORIGINS — per-installation exact origins (step 7)
# -----------------------------------------------------------------------------


def test_given_extra_origin_when_checked_then_allowed():
    """given: an origin outside the built-in allowlist, passed via extra
    when: origin_allowed is evaluated
    then: the connection is allowed
    """
    extra = frozenset({"https://meet.example.com"})

    assert origin_allowed("https://meet.example.com", extra=extra) is True


def test_given_extra_origin_when_explicitly_empty_then_rejected():
    """given: the same origin, with the extra set explicitly empty
    when: origin_allowed is evaluated
    then: it is rejected — the knob is opt-in, never a silent widening
    """
    assert origin_allowed("https://meet.example.com", extra=frozenset()) is False


def test_given_configured_extra_origins_when_argument_omitted_then_still_applied():
    """given: RECOG_EXTRA_WS_ORIGINS configured for the installation
    when: origin_allowed is called without the extra argument
    then: the configured origins still apply — every production call site
          omits it, so a None default meaning "no extras" would have made the
          whole knob silently dead
    """
    configured = frozenset({"https://meet.example.com"})

    with mock.patch.object(ws_origin, "EXTRA_ORIGINS", configured):
        assert origin_allowed("https://meet.example.com") is True
        assert origin_allowed("https://evil.example.com") is False


def test_given_extra_origins_when_other_origin_checked_then_still_rejected():
    """given: an extra allowlist configured
    when: some other origin connects
    then: extra entries match exactly and widen nothing else
    """
    extra = frozenset({"https://meet.example.com"})

    assert origin_allowed("https://evil.example.com", extra=extra) is False
    assert origin_allowed("https://meet.example.com.evil.tld", extra=extra) is False


def test_given_extra_origin_when_null_origin_checked_then_only_an_explicit_entry_allows_it():
    """given: an extra allowlist configured
    when: a sandboxed iframe sends the literal "null" Origin
    then: it stays rejected unless the operator listed "null" itself — extra
          matches exactly and does not touch the null/None contract
    """
    extra = frozenset({"https://meet.example.com", "null"})

    assert origin_allowed("null", extra=frozenset({"https://meet.example.com"})) is False
    # even a literal "null" entry only matches the exact-set branch, which is
    # what an operator explicitly asked for
    assert origin_allowed("null", extra=extra) is True


def test_given_extra_origins_when_manifest_compared_then_builtin_set_unchanged():
    """given: the extra-origins knob exists
    when: the built-in ALLOWED_ORIGINS set is inspected
    then: it still contains only manifest hosts + webapp origins, so the
          manifest-equality test above keeps guarding the extension mirror
    """
    assert "https://meet.example.com" not in ALLOWED_ORIGINS
