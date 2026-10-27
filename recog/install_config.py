"""Installation settings: one TOML file per user, read once at startup.

`~/.config/recog/config.toml` (or `$XDG_CONFIG_HOME/recog/config.toml`, or
`RECOG_CONFIG_FILE`) holds what differs between installations of the same
repository and is not a secret; secrets stay in `.env`. Python services and the
extension build (`python -m recog.install_config`) share this one parser.
"""

import ipaddress
import json
import os
import re
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlsplit

REPO_ROOT = Path(__file__).resolve().parent.parent
PLATFORMS = frozenset({"ktalk", "vas3k", "jitsi", "matrix", "gmeet", "zoom"})
CONFIGURABLE_PLATFORMS = ("jitsi", "matrix")

# Hosts the extension manifest already covers; configuring one again would
# register a second platform for the same pages.
_BUILTIN_HOSTS = frozenset({"meet.vas3k.cloud", "meet.google.com"})
_BUILTIN_WILDCARD_DOMAINS = ("ktalk.ru", "zoom.us")


@dataclass(frozen=True)
class InstallConfig:
    jitsi: tuple[str, ...] = ()
    matrix: tuple[str, ...] = ()
    # Nonstandard source names of old recordings -> the platform they were.
    source_aliases: dict[str, str] = field(default_factory=dict)
    # Web UI names of the sources the installation's own importers write.
    source_labels: dict[str, str] = field(default_factory=dict)


def _resolve(configured: str) -> Path:
    path = Path(configured).expanduser()
    return path if path.is_absolute() else REPO_ROOT / path


def config_path() -> Path:
    configured = os.environ.get("RECOG_CONFIG_FILE")
    if configured:
        return _resolve(configured)
    base = os.environ.get("XDG_CONFIG_HOME") or "~/.config"
    return Path(base).expanduser() / "recog" / "config.toml"


def legacy_hosts_path() -> Path:
    """Where installations kept conference hosts before config.toml."""
    return _resolve(os.environ.get("RECOG_CONFERENCE_HOSTS_FILE") or ".local/conference-hosts.json")


def _is_builtin_host(hostname: str) -> bool:
    return hostname in _BUILTIN_HOSTS or any(
        hostname == domain or hostname.endswith("." + domain)
        for domain in _BUILTIN_WILDCARD_DOMAINS
    )


def _table(value, name: str, keys: set[str]) -> dict:
    if not isinstance(value, dict) or set(value) - keys:
        raise ValueError(f"{name} must be a table with only {', '.join(sorted(keys))}")
    return value


def _canonical_hostname(hostname: str) -> bool:
    return (
        bool(hostname)
        and len(hostname) <= 253
        and all(
            re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
            for label in hostname.split(".")
        )
    )


def _ipv4_shorthand(hostname: str) -> bool:
    # Browser URL parsing treats numeric last labels as IPv4. Require
    # the canonical dotted address rather than accepting its shorthands.
    last_label = hostname.rsplit(".", 1)[-1]
    numeric = last_label.isdigit() or re.fullmatch(r"0x[0-9a-f]+", last_label)
    return bool(numeric) and str(ipaddress.IPv4Address(hostname)) != hostname


def _origin(origin) -> str:
    if not isinstance(origin, str):
        raise TypeError("Conference origins must be strings")
    hostname = urlsplit(origin).hostname or ""
    if (
        not _canonical_hostname(hostname)
        or origin != f"https://{hostname}"
        or _is_builtin_host(hostname)
    ):
        raise ValueError("Use canonical HTTPS origins without ports, paths or built-in hosts")
    if _ipv4_shorthand(hostname):
        raise ValueError("Use a canonical IPv4 address")
    return origin


def _conference_hosts(conferences: dict) -> dict[str, tuple[str, ...]]:
    hosts = {}
    seen = set()
    for platform in CONFIGURABLE_PLATFORMS:
        origins = conferences.get(platform, [])
        if not isinstance(origins, list):
            raise TypeError(f"{platform} must be an array of HTTPS origins")
        hosts[platform] = tuple(_origin(origin) for origin in origins)
        if seen & set(hosts[platform]) or len(set(hosts[platform])) != len(origins):
            raise ValueError("Conference origins must be unique across platforms")
        seen |= set(hosts[platform])
    return hosts


def _source_map(value, message: str, valid) -> dict[str, str]:
    """A table from source names to strings that `valid(name, value)` accepts."""
    if not isinstance(value, dict) or not all(
        name and valid(name, target) for name, target in value.items()
    ):
        raise ValueError(message)
    return dict(value)


def _alias(name: str, platform) -> bool:
    return name not in PLATFORMS and platform in CONFIGURABLE_PLATFORMS


def _label(name: str, label) -> bool:
    return isinstance(label, str) and bool(label)


def parse_install_config(value: dict) -> InstallConfig:
    """Validate the parsed TOML document; nothing unknown is ignored."""
    value = _table(value, "The configuration", {"conferences", "sources"})
    conferences = _table(value.get("conferences", {}), "[conferences]", set(CONFIGURABLE_PLATFORMS))
    sources = _table(value.get("sources", {}), "[sources]", {"aliases", "labels"})
    return InstallConfig(
        source_aliases=_source_map(
            sources.get("aliases", {}),
            "[sources.aliases] must map legacy names to jitsi or matrix",
            _alias,
        ),
        source_labels=_source_map(
            sources.get("labels", {}),
            "[sources.labels] must map source names to nonempty names",
            _label,
        ),
        **_conference_hosts(conferences),
    )


def _from_legacy(value) -> dict:
    """The conference-hosts.json shape, as the config.toml document."""
    value = _table(value, "Conference hosts", {"jitsi", "matrix", "source_aliases"})
    document = {"conferences": {key: value[key] for key in CONFIGURABLE_PLATFORMS if key in value}}
    if "source_aliases" in value:
        document["sources"] = {"aliases": value["source_aliases"]}
    return document


def load_install_config(path: Path | None = None, legacy: Path | None = None) -> InstallConfig:
    path = config_path() if path is None else path
    legacy = legacy_hosts_path() if legacy is None else legacy
    if path.exists() and legacy.exists():
        raise ValueError(
            f"Both {path} and {legacy} exist: move the hosts into the first "
            f"and delete {legacy.name}"
        )
    try:
        if path.exists():
            return parse_install_config(tomllib.loads(path.read_text(encoding="utf-8")))
        if legacy.exists():
            print(
                f"recog: {legacy} is deprecated, move its settings to {path} "
                "(docs/configuration.md)",
                file=sys.stderr,
            )
            return parse_install_config(
                _from_legacy(json.loads(legacy.read_text(encoding="utf-8")))
            )
    except (TypeError, ValueError) as error:
        raise ValueError(f"{path if path.exists() else legacy}: {error}") from error
    return InstallConfig()


INSTALL_CONFIG = load_install_config()


def canonical_source(source: str | None) -> str | None:
    """Resolve legacy installation labels without rewriting stored meetings."""
    return INSTALL_CONFIG.source_aliases.get(source, source)


if __name__ == "__main__":
    # The extension build takes the configured hosts from here, so the file
    # has one parser and one set of rules.
    print(
        json.dumps(
            {
                platform: list(getattr(INSTALL_CONFIG, platform))
                for platform in CONFIGURABLE_PLATFORMS
            }
        )
    )
