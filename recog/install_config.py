"""Installation settings: one TOML file per user, read once at startup.

`~/.config/recog/config.toml` (or `$XDG_CONFIG_HOME/recog/config.toml`, or
`RECOG_CONFIG_FILE`) holds what differs between installations of the same
repository and is not a secret; secrets stay in `.env`. Python services and the
extension build (`python -m recog.install_config`) share this one parser.

Every setting is also an environment variable, which wins over the file: the
process environment and the repository's `.env` (loaded here) override it.
Modules read the typed result, `INSTALL_CONFIG`, never the environment.
"""

import ipaddress
import json
import os
import re
import sys
import tomllib
from dataclasses import dataclass, field, fields, replace
from pathlib import Path
from urllib.parse import urlsplit

REPO_ROOT = Path(__file__).resolve().parent.parent
PLATFORMS = frozenset({"ktalk", "vas3k", "jitsi", "matrix", "gmeet", "zoom"})
CONFIGURABLE_PLATFORMS = ("jitsi", "matrix")

# Hosts the extension manifest already covers; configuring one again would
# register a second platform for the same pages.
_BUILTIN_HOSTS = frozenset({"meet.vas3k.cloud", "meet.google.com"})
_BUILTIN_WILDCARD_DOMAINS = ("ktalk.ru", "zoom.us")


def _setting(variable: str, kind: str, default=None):
    """A settings field: its environment variable and how values are read."""
    return field(default=default, metadata={"env": variable, "kind": kind})


# Sections are mutable only so that tests can monkeypatch one field; code
# treats INSTALL_CONFIG as read-only.
@dataclass
class AsrSettings:
    # "auto" | "local" | "remote" | "wavo": auto is wavo on a Mac where setup.sh built
    # it, else local (PyTorch); remote offloads GigaAM to a gateway (ADR-0020).
    backend: str = _setting("RECOG_ASR_BACKEND", "text", "auto")
    # auto (cuda > mps > cpu) | cuda | mps | cpu; explicit + unavailable fails.
    device: str = _setting("RECOG_ASR_DEVICE", "text", "auto")
    # torch.set_num_threads() cap for CPU inference; None leaves torch alone.
    torch_threads: int | None = _setting("RECOG_TORCH_THREADS", "integer")
    # Gateway base URL; no default, so remote without it fails at startup.
    remote_url: str = _setting("RECOG_ASR_REMOTE_URL", "text", "")
    remote_model: str = _setting("RECOG_ASR_REMOTE_MODEL", "text", "gigaam")
    remote_timeout_s: float = _setting("RECOG_ASR_REMOTE_TIMEOUT_S", "number", 15.0)
    # Shorter timeout of live chunk/batch calls; refine/offline keep the long one.
    live_timeout_s: float = _setting("RECOG_ASR_LIVE_TIMEOUT_S", "number", 5.0)
    circuit_fails: int = _setting("RECOG_ASR_CIRCUIT_FAILS", "integer", 3)
    circuit_open_s: float = _setting("RECOG_ASR_CIRCUIT_OPEN_S", "number", 60.0)
    # Audio kept in memory while no ASR can serve; the oldest is dropped beyond it.
    pending_buffer_cap_s: float = _setting("ASR_PENDING_BUFFER_CAP_S", "number", 600.0)


@dataclass
class LiveSettings:
    # Last-words dedup applies only within this gap from the previous segment.
    dedup_max_gap_s: float = _setting("DEDUP_MAX_GAP_S", "number", 2.0)
    # Pinned PulseAudio/PipeWire monitor (Linux); empty follows the default sink.
    pulse_monitor_source: str = _setting("RECOG_PULSE_MONITOR_SOURCE", "text", "")
    # Native Zoom application source on macOS, same as `live --zoom-app`.
    zoom_app: bool = _setting("RECOG_ZOOM_APP", "flag", False)
    # Cancel what the built-in speakers play out of the microphone (macOS).
    echo_cancel: bool = _setting("RECOG_ECHO_CANCEL", "flag", False)
    # The microphone's child process on macOS: "python" (recog/mic_worker.py) or
    # "rust" (rust/recog-mic, built by setup.sh when cargo is present).
    mic_worker: str = _setting("RECOG_MIC_WORKER", "text", "python")
    # Record golden sessions under <data dir>/golden for replay (recog/golden.py).
    golden_record: bool = _setting("RECOG_GOLDEN_RECORD", "flag", False)


@dataclass
class CopilotSettings:
    autospawn: bool = _setting("RECOG_COPILOT_AUTOSPAWN", "flag", True)
    grace_s: float = _setting("RECOG_COPILOT_GRACE_S", "number", 90.0)
    mcp_url: str = _setting("RECOG_COPILOT_MCP_URL", "text", "http://127.0.0.1:8767/mcp")


@dataclass
class WebSettings:
    bridge_ws_url: str = _setting("RECOG_BRIDGE_WS_URL", "text", "ws://localhost:8766")
    # Exact origins allowed besides the conference hosts, backend only.
    extra_ws_origins: tuple[str, ...] = _setting("RECOG_EXTRA_WS_ORIGINS", "list", ())
    # Read-only meeting directories of other sources: "path" or "path:source".
    external_meetings_paths: tuple[str, ...] = _setting("EXTERNAL_MEETINGS_PATHS", "list", ())


@dataclass
class MacosSettings:
    # Signing identity of RecogZoomHelper.app; empty signs ad hoc.
    codesign_identity: str = _setting("RECOG_CODESIGN_IDENTITY", "text", "")


SECTIONS = {
    "asr": AsrSettings,
    "live": LiveSettings,
    "copilot": CopilotSettings,
    "web": WebSettings,
    "macos": MacosSettings,
}

# Environment variable -> its config.toml key, for messages.
SETTING_KEYS = {
    f.metadata["env"]: f"[{name}] {f.name}" for name, cls in SECTIONS.items() for f in fields(cls)
}


@dataclass(frozen=True)
class InstallConfig:
    jitsi: tuple[str, ...] = ()
    matrix: tuple[str, ...] = ()
    # Nonstandard source names of old recordings -> the platform they were.
    source_aliases: dict[str, str] = field(default_factory=dict)
    # Web UI names of the sources the installation's own importers write.
    source_labels: dict[str, str] = field(default_factory=dict)
    asr: AsrSettings = field(default_factory=AsrSettings)
    live: LiveSettings = field(default_factory=LiveSettings)
    copilot: CopilotSettings = field(default_factory=CopilotSettings)
    web: WebSettings = field(default_factory=WebSettings)
    macos: MacosSettings = field(default_factory=MacosSettings)
    # Environment variables whose setting the file gave, to report conflicts.
    from_file: frozenset[str] = frozenset()


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


def _expect(value, kinds, name: str) -> None:
    # bool is an int in Python, but `true` is never a number in the file.
    if isinstance(value, bool) != (kinds is bool) or not isinstance(value, kinds):
        raise TypeError(f"{name} has the wrong type")


def _positive(value: int, name: str) -> int:
    if value <= 0:
        raise ValueError(f"{name} must be a positive integer")
    return value


def _file_flag(value, name: str) -> bool:
    _expect(value, bool, name)
    return value


def _file_integer(value, name: str) -> int:
    _expect(value, int, name)
    return _positive(value, name)


def _file_number(value, name: str) -> float:
    _expect(value, (int, float), name)
    return float(value)


def _file_text(value, name: str) -> str:
    _expect(value, str, name)
    return value


def _file_list(value, name: str) -> tuple[str, ...]:
    _expect(value, list, name)
    return tuple(_file_text(item, name) for item in value)


_TRUE = frozenset({"1", "true", "yes", "on"})
_FALSE = frozenset({"0", "false", "no", "off"})


def _env_flag(raw: str, name: str) -> bool:
    if raw.lower() not in _TRUE | _FALSE:
        raise ValueError(f"{name}={raw!r} is not one of 1/0, true/false, yes/no, on/off")
    return raw.lower() in _TRUE


def _env_integer(raw: str, name: str) -> int:
    return _positive(int(raw), name)


def _env_list(raw: str, name: str) -> tuple[str, ...]:
    return tuple(item.strip() for item in raw.split(",") if item.strip())


# kind -> (value from the file, value from an environment string).
_KINDS = {
    "flag": (_file_flag, _env_flag),
    "integer": (_file_integer, _env_integer),
    "number": (_file_number, lambda raw, name: float(raw)),
    "text": (_file_text, lambda raw, name: raw),
    "list": (_file_list, _env_list),
}


def _file_section(cls, document: dict, name: str):
    table = _table(document.get(name, {}), f"[{name}]", {f.name for f in fields(cls)})
    return cls(
        **{
            f.name: _KINDS[f.metadata["kind"]][0](table[f.name], f"{name}.{f.name}")
            for f in fields(cls)
            if f.name in table
        }
    )


def _set_in_file(document: dict) -> frozenset[str]:
    return frozenset(
        f.metadata["env"]
        for name, cls in SECTIONS.items()
        for f in fields(cls)
        if f.name in document.get(name, {})
    )


def parse_install_config(value: dict) -> InstallConfig:
    """Validate the parsed TOML document; nothing unknown is ignored."""
    value = _table(value, "The configuration", {"conferences", "sources", *SECTIONS})
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
        **{name: _file_section(cls, value, name) for name, cls in SECTIONS.items()},
        from_file=_set_in_file(value),
        **_conference_hosts(conferences),
    )


def _from_legacy(value) -> dict:
    """The conference-hosts.json shape, as the config.toml document."""
    value = _table(value, "Conference hosts", {"jitsi", "matrix", "source_aliases"})
    document = {"conferences": {key: value[key] for key in CONFIGURABLE_PLATFORMS if key in value}}
    if "source_aliases" in value:
        document["sources"] = {"aliases": value["source_aliases"]}
    return document


def _environment_value(f, environ):
    """The setting's value from the environment, or None when it is unset."""
    raw = environ.get(f.metadata["env"], "").strip()
    if not raw:
        return None
    try:
        return _KINDS[f.metadata["kind"]][1](raw, f.metadata["env"])
    except ValueError as error:
        raise ValueError(f"{f.metadata['env']}: {error}") from error


def with_environment(config: InstallConfig, environ) -> tuple[InstallConfig, list[str]]:
    """The config with environment values over the file's.

    Also returns the variables that override a different value the file gave:
    the file's value does nothing, which the caller reports instead of hiding.
    """
    sections = {}
    overridden = []
    for name in SECTIONS:
        section = getattr(config, name)
        changes = {}
        for f in fields(section):
            value = _environment_value(f, environ)
            if value is None:
                continue
            changes[f.name] = value
            if f.metadata["env"] in config.from_file and value != getattr(section, f.name):
                overridden.append(f.metadata["env"])
        sections[name] = replace(section, **changes)
    return replace(config, **sections), overridden


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


def load_dotenv(env_path: Path | None = None) -> None:
    """Load the repository's .env into the environment without overriding it."""
    if env_path is None:
        env_path = REPO_ROOT / ".env"

    if not env_path.exists():
        return

    with open(env_path, "r") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" in line:
                key, value = line.split("=", 1)
                key = key.strip()
                value = value.strip().strip('"').strip("'")
                if key and key not in os.environ:
                    os.environ[key] = value


def _load() -> InstallConfig:
    config, overridden = with_environment(load_install_config(), os.environ)
    for variable in overridden:
        print(
            f"recog: {variable} from the environment or .env overrides "
            f"{SETTING_KEYS[variable]} of config.toml; keep only one of them",
            file=sys.stderr,
        )
    return config


# .env first: like the process environment, it wins over config.toml.
load_dotenv()
INSTALL_CONFIG = _load()


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
