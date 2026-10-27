"""Installation settings stay local and cannot widen origin permissions."""

import dataclasses
import json
import os
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

from recog import install_config
from recog.install_config import (
    SETTINGS,
    InstallConfig,
    config_path,
    load_install_config,
    parse_install_config,
)

ROOT = Path(__file__).resolve().parent.parent
# Subprocesses must not see the developer's own legacy hosts file.
NO_LEGACY = {"RECOG_CONFERENCE_HOSTS_FILE": "/nonexistent/conference-hosts.json"}
HOSTS = """
[conferences]
jitsi = ["https://meet.example.org"]
matrix = ["https://element.example.org"]
"""


def with_aliases(monkeypatch, aliases):
    config = dataclasses.replace(install_config.INSTALL_CONFIG, source_aliases=aliases)
    monkeypatch.setattr(install_config, "INSTALL_CONFIG", config)


def test_no_files_when_loading_then_only_builtin_hosts_remain(tmp_path):
    config = load_install_config(tmp_path / "config.toml", tmp_path / "hosts.json")
    assert config == InstallConfig()


def test_config_file_when_loading_then_its_settings_are_returned(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text(
        HOSTS
        + """
[sources.aliases]
old-meet = "jitsi"
"""
    )
    config = load_install_config(path, tmp_path / "absent.json")
    assert config.jitsi == ("https://meet.example.org",)
    assert config.matrix == ("https://element.example.org",)
    assert config.source_aliases == {"old-meet": "jitsi"}


def test_source_labels_when_loading_then_importer_sources_are_named(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text("""
[sources.labels]
importer = "Phone Importer"
""")
    config = load_install_config(path, tmp_path / "absent.json")
    assert config.source_labels == {"importer": "Phone Importer"}


def test_only_legacy_hosts_file_when_loading_then_it_is_read_with_a_warning(tmp_path, capsys):
    legacy = tmp_path / "conference-hosts.json"
    legacy.write_text(
        json.dumps(
            {
                "jitsi": ["https://meet.example.org"],
                "source_aliases": {"old-meet": "jitsi"},
            }
        )
    )
    config = load_install_config(tmp_path / "config.toml", legacy)
    assert config.jitsi == ("https://meet.example.org",)
    assert config.source_aliases == {"old-meet": "jitsi"}
    assert "config.toml" in capsys.readouterr().err


def test_both_files_when_loading_then_rejected(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text(HOSTS)
    legacy = tmp_path / "conference-hosts.json"
    legacy.write_text("{}")
    with pytest.raises(ValueError, match="conference-hosts.json"):
        load_install_config(path, legacy)


def test_malformed_toml_when_loading_then_rejected(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text("[conferences\n")
    with pytest.raises(ValueError, match="config.toml"):
        load_install_config(path, tmp_path / "absent.json")


def test_default_location_when_resolving_then_xdg_config_home_is_used(monkeypatch, tmp_path):
    monkeypatch.delenv("RECOG_CONFIG_FILE", raising=False)
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    assert config_path() == tmp_path / "recog" / "config.toml"
    monkeypatch.delenv("XDG_CONFIG_HOME")
    assert config_path() == Path.home() / ".config" / "recog" / "config.toml"


def test_explicit_location_when_resolving_then_it_wins(monkeypatch, tmp_path):
    monkeypatch.setenv("RECOG_CONFIG_FILE", "~/elsewhere.toml")
    assert config_path() == Path.home() / "elsewhere.toml"
    monkeypatch.setenv("RECOG_CONFIG_FILE", "relative.toml")
    assert config_path() == ROOT / "relative.toml"


def test_settings_when_parsing_then_each_becomes_its_environment_value():
    config = parse_install_config(
        {
            "asr": {"backend": "remote", "remote_timeout_s": 20, "circuit_fails": 4},
            "live": {"dedup_max_gap_s": 2.5, "zoom_app": True},
            "copilot": {"autospawn": False},
            "web": {"external_meetings_paths": ["/a/meetings:a", "/b/meetings"]},
        }
    )
    assert config.settings == {
        "RECOG_ASR_BACKEND": "remote",
        "RECOG_ASR_REMOTE_TIMEOUT_S": "20",
        "RECOG_ASR_CIRCUIT_FAILS": "4",
        "DEDUP_MAX_GAP_S": "2.5",
        "RECOG_ZOOM_APP": "1",
        "RECOG_COPILOT_AUTOSPAWN": "0",
        "EXTERNAL_MEETINGS_PATHS": "/a/meetings:a,/b/meetings",
    }


def test_settings_when_applied_then_environment_values_win():
    config = parse_install_config({"asr": {"backend": "remote", "remote_model": "big"}})
    environ = {"RECOG_ASR_BACKEND": "local"}

    config.apply_settings(environ)

    assert environ == {"RECOG_ASR_BACKEND": "local", "RECOG_ASR_REMOTE_MODEL": "big"}


def test_config_file_when_recog_starts_then_its_settings_reach_the_code(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text('[live]\ndedup_max_gap_s = 3.5\n[asr]\nremote_model = "big"\n')
    code = "import recog.config as c; print(c.DEDUP_MAX_GAP_S, c.RECOG_ASR_REMOTE_MODEL)"
    env = {**os.environ, "RECOG_CONFIG_FILE": str(path), **NO_LEGACY}
    env.pop("DEDUP_MAX_GAP_S", None)
    env["RECOG_ASR_REMOTE_MODEL"] = "small"

    result = subprocess.run(
        [sys.executable, "-c", code],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.split() == ["3.5", "small"]


@pytest.mark.parametrize(
    "value",
    [
        {"other": {}},
        {"conferences": {"other": []}},
        {"conferences": {"jitsi": "https://meet.example.org"}},
        {"conferences": {"jitsi": [False]}},
        {"conferences": {"jitsi": ["http://meet.example.org"]}},
        {"conferences": {"matrix": ["https://element.example.org/"]}},
        {"conferences": {"matrix": ["https://element.example.org:443"]}},
        {"conferences": {"matrix": ["https://element.example.org/path"]}},
        {"conferences": {"matrix": ["https://element.example.org?query"]}},
        {"conferences": {"matrix": ["https://element.example.org#room"]}},
        {"conferences": {"matrix": ["https://user@element.example.org"]}},
        {"conferences": {"matrix": ["https://*.example.org"]}},
        {"conferences": {"matrix": ["https://MEET.example.org"]}},
        {"conferences": {"matrix": ["https://m\u0435et.example.org"]}},
        {"conferences": {"matrix": ["https://meet..example.org"]}},
        {"conferences": {"matrix": ["https://meet.example.org."]}},
        {"conferences": {"matrix": ["https://-meet.example.org"]}},
        {"conferences": {"matrix": [f"https://{'a' * 64}.example.org"]}},
        {"conferences": {"matrix": ["https://[::1]"]}},
        {"conferences": {"matrix": ["https://127.1"]}},
        {"conferences": {"matrix": ["https://123"]}},
        {"conferences": {"matrix": ["https://foo.123"]}},
        {"conferences": {"matrix": ["https://ktalk.ru"]}},
        {"conferences": {"jitsi": ["https://meet.vas3k.cloud"]}},
        {"conferences": {"jitsi": ["https://meet.google.com"]}},
        {"conferences": {"jitsi": ["https://zoom.us"]}},
        {"conferences": {"jitsi": ["https://app.zoom.us"]}},
        {
            "conferences": {
                "jitsi": ["https://same.example.org"],
                "matrix": ["https://same.example.org"],
            }
        },
        {"sources": {"other": {}}},
        {"sources": {"aliases": {"old": "unknown"}}},
        {"sources": {"aliases": {"ktalk": "matrix"}}},
        {"sources": {"aliases": {"gmeet": "jitsi"}}},
        {"sources": {"aliases": {"zoom": "jitsi"}}},
        {"sources": {"labels": {"importer": 1}}},
        {"sources": {"labels": {"importer": ""}}},
        {"sources": {"labels": {"": "Label"}}},
        {"sources": {"labels": ["importer"]}},
        {"asr": {"other": "x"}},
        {"asr": {"backend": 1}},
        {"asr": {"remote_timeout_s": "15"}},
        {"asr": {"remote_timeout_s": True}},
        {"asr": {"circuit_fails": 3.5}},
        {"live": {"zoom_app": "1"}},
        {"web": {"extra_ws_origins": "https://a.example.org"}},
        {"web": {"extra_ws_origins": [1]}},
    ],
)
def test_invalid_config_when_parsing_then_rejected(value):
    with pytest.raises((TypeError, ValueError)):
        parse_install_config(value)


@pytest.mark.parametrize(
    "origin",
    [
        "https://evilzoom.us",
        "https://evilktalk.ru",
        "https://meet-1.xn--e1afmkfd.xn--p1ai",
    ],
)
def test_canonical_hostname_when_parsing_then_accepted(origin):
    assert parse_install_config({"conferences": {"jitsi": [origin]}}).jitsi == (origin,)


def test_canonical_ipv4_when_parsing_then_exact_origin_is_preserved():
    config = parse_install_config({"conferences": {"jitsi": ["https://127.0.0.1"]}})
    assert config.jitsi == ("https://127.0.0.1",)


def test_configured_origins_when_process_starts_then_only_exact_hosts_are_allowed(
    tmp_path,
):
    path = tmp_path / "config.toml"
    path.write_text(HOSTS)
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            """
from recog.ws_origin import origin_allowed
assert origin_allowed('https://meet.example.org')
assert origin_allowed('https://element.example.org')
assert not origin_allowed('https://element.example.org.evil.test')
assert not origin_allowed('http://element.example.org')
assert not origin_allowed('https://unlisted.example.org')
""",
        ],
        cwd=ROOT,
        env={
            **os.environ,
            "RECOG_CONFIG_FILE": str(path),
            **NO_LEGACY,
            "RECOG_EXTRA_WS_ORIGINS": "",
        },
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_extension_build_when_asking_for_hosts_then_module_prints_them_as_json(
    tmp_path,
):
    path = tmp_path / "config.toml"
    path.write_text(
        HOSTS
        + """
[sources.aliases]
old-meet = "jitsi"
"""
    )
    result = subprocess.run(
        [sys.executable, "-m", "recog.install_config"],
        cwd=ROOT,
        env={**os.environ, "RECOG_CONFIG_FILE": str(path), **NO_LEGACY},
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {
        "jitsi": ["https://meet.example.org"],
        "matrix": ["https://element.example.org"],
    }


def test_invalid_config_when_extension_build_asks_then_it_fails(tmp_path):
    path = tmp_path / "config.toml"
    path.write_text('[conferences]\njitsi = ["https://*.example.org"]\n')
    result = subprocess.run(
        [sys.executable, "-m", "recog.install_config"],
        cwd=ROOT,
        env={**os.environ, "RECOG_CONFIG_FILE": str(path), **NO_LEGACY},
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode != 0
    assert "config.toml" in result.stderr


def test_legacy_source_when_reading_then_canonical_without_changing_recording(
    tmp_path, monkeypatch
):
    from recog.meeting_storage import MeetingStorage, is_conference_source
    from recog.web.meeting_catalog import MeetingCatalog

    with_aliases(monkeypatch, {"old-meet": "jitsi"})
    storage = MeetingStorage(tmp_path / "meetings")
    meeting_id = storage.start_session()
    path = storage.base_path / meeting_id / "meta.json"
    meta = json.loads(path.read_text())
    meta["source"] = "old-meet"
    raw = json.dumps(meta)
    path.write_text(raw)

    assert is_conference_source("old-meet")
    assert storage.get_meeting(meeting_id)["meta"]["source"] == "jitsi"
    assert storage.list_active_meetings()[0]["source"] == "jitsi"
    catalog = MeetingCatalog(storage.base_path)
    assert catalog._metadata_entry(path, meta)["source"] == "jitsi"
    assert path.read_text() == raw


@pytest.mark.parametrize("leave_first", [False, True])
def test_same_room_on_different_hosts_when_joining_then_recordings_stay_separate(
    tmp_path, leave_first
):
    from recog.meeting_storage import MeetingStorage

    storage = MeetingStorage(tmp_path / "meetings")
    storage.start_session(ktalk=True)
    first = storage.on_meeting_join("https://a.example.org/daily", platform="jitsi")
    storage.append_segment("Tester", "test", "hello")
    if leave_first:
        storage.on_ktalk_leave()
    second = storage.on_meeting_join("https://b.example.org/daily", platform="jitsi")
    assert second != first
    assert storage.get_meeting(first)["meta"]["conference"]["host"] == "a.example.org"


def test_legacy_same_host_when_rejoining_then_existing_recording_is_reused(tmp_path, monkeypatch):
    from recog.meeting_storage import MeetingStorage

    with_aliases(monkeypatch, {"old-meet": "jitsi"})
    storage = MeetingStorage(tmp_path / "meetings")
    storage.start_session(ktalk=True)
    first = storage.on_meeting_join("https://a.example.org/daily", platform="jitsi")
    storage.append_segment("Tester", "test", "hello")
    storage.on_ktalk_leave()
    path = storage.base_path / first / "meta.json"
    meta = json.loads(path.read_text())
    meta["source"] = "old-meet"
    path.write_text(json.dumps(meta))
    assert storage.on_meeting_join("https://a.example.org/daily", platform="jitsi") == first


def test_invalid_host_config_when_readiness_is_checked_then_activation_is_rejected(
    tmp_path,
):
    config = tmp_path / "config.toml"
    config.write_text('[conferences]\njitsi = ["https://*.example.org"]\n')
    result = subprocess.run(
        [
            "bash",
            "-c",
            'source "$1"; deps_asr_config_check',
            "--",
            str(ROOT / "scripts/deps.sh"),
        ],
        env={
            **os.environ,
            "DEPS_PYTHON": sys.executable,
            "RECOG_ASR_BACKEND": "local",
            "RECOG_CONFIG_FILE": str(config),
            **NO_LEGACY,
        },
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode != 0
    assert "Invalid installation configuration" in result.stdout


def test_legacy_index_when_listing_then_canonical_without_rewriting_cache(tmp_path, monkeypatch):
    from recog.meeting_storage import MeetingStorage

    with_aliases(monkeypatch, {"old-meet": "jitsi"})
    storage = MeetingStorage(tmp_path / "meetings")
    path = storage.base_path / "index.json"
    raw = json.dumps({"meetings": [{"id": "old", "source": "old-meet"}]})
    path.write_text(raw)
    assert storage.list_meetings(include_active=False)[0]["source"] == "jitsi"
    assert path.read_text() == raw


def test_shipped_example_when_every_setting_is_uncommented_then_it_parses_and_covers_all():
    lines = (ROOT / "deploy/config.example.toml").read_text().splitlines()
    uncommented = [
        line[2:] if line.startswith("# ") and " = " in line and "RECOG" not in line else line
        for line in lines
    ]
    document = tomllib.loads("\n".join(uncommented))

    config = parse_install_config(document)

    expected = {variable for keys in SETTINGS.values() for variable, _ in keys.values()}
    assert set(config.settings) == expected
    assert config.jitsi and config.matrix and config.source_aliases and config.source_labels
