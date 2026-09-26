"""Custom conference hosts stay local and cannot widen origin permissions."""

import json
import os
from pathlib import Path
import subprocess
import sys

import pytest

from recog.conference_hosts import CONFERENCE_HOSTS, load_conference_hosts


def test_missing_config_when_loading_then_only_builtin_hosts_remain(tmp_path):
    assert load_conference_hosts(tmp_path / "absent.json") == {
        "jitsi": [], "matrix": [], "source_aliases": {},
    }


@pytest.mark.parametrize(
    "value",
    [
        [],
        {"other": []},
        {"jitsi": "https://meet.example.org"},
        {"jitsi": [False]},
        {"jitsi": ["http://meet.example.org"]},
        {"matrix": ["https://element.example.org/"]},
        {"matrix": ["https://element.example.org:443"]},
        {"matrix": ["https://element.example.org/path"]},
        {"matrix": ["https://element.example.org?query"]},
        {"matrix": ["https://user@element.example.org"]},
        {"matrix": ["https://*.example.org"]},
        {"matrix": ["https://127.1"]},
        {"matrix": ["https://123"]},
        {"matrix": ["https://foo.123"]},
        {"matrix": ["https://ktalk.ru"]},
        {"jitsi": ["https://meet.vas3k.cloud"]},
        {"jitsi": ["https://meet.google.com"]},
        {"jitsi": ["https://same.example.org"], "matrix": ["https://same.example.org"]},
        {"source_aliases": {"old": "unknown"}},
        {"source_aliases": {"ktalk": "matrix"}},
        {"source_aliases": {"gmeet": "jitsi"}},
    ],
)
def test_invalid_config_when_loading_then_rejected(tmp_path, value):
    path = tmp_path / "hosts.json"
    path.write_text(json.dumps(value))
    with pytest.raises(ValueError):
        load_conference_hosts(path)


def test_configured_origins_when_process_starts_then_only_exact_hosts_are_allowed(tmp_path):
    path = tmp_path / "hosts.json"
    path.write_text(json.dumps({"jitsi": ["https://meet.example.org"],
                                "matrix": ["https://element.example.org"]}))
    result = subprocess.run([
        sys.executable, "-c", """
from recog.ws_origin import origin_allowed
assert origin_allowed('https://meet.example.org')
assert origin_allowed('https://element.example.org')
assert not origin_allowed('https://element.example.org.evil.test')
assert not origin_allowed('http://element.example.org')
assert not origin_allowed('https://unlisted.example.org')
"""], cwd=Path(__file__).resolve().parent.parent,
        env={**os.environ, "RECOG_CONFERENCE_HOSTS_FILE": str(path),
             "RECOG_EXTRA_WS_ORIGINS": ""}, capture_output=True, text=True)
    assert result.returncode == 0, result.stderr


def test_canonical_ipv4_when_loading_then_exact_origin_is_preserved(tmp_path):
    path = tmp_path / "hosts.json"
    path.write_text(json.dumps({"jitsi": ["https://127.0.0.1"]}))
    assert load_conference_hosts(path)["jitsi"] == ["https://127.0.0.1"]


def test_legacy_source_when_reading_then_canonical_without_changing_recording(tmp_path, monkeypatch):
    from recog.meeting_storage import MeetingStorage, is_conference_source
    from recog.web.meeting_catalog import MeetingCatalog

    monkeypatch.setitem(CONFERENCE_HOSTS, "source_aliases", {"old-meet": "jitsi"})
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
def test_same_room_on_different_hosts_when_joining_then_recordings_stay_separate(tmp_path, leave_first):
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

    monkeypatch.setitem(CONFERENCE_HOSTS, "source_aliases", {"old-meet": "jitsi"})
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


def test_invalid_host_config_when_readiness_is_checked_then_activation_is_rejected(tmp_path):
    root = Path(__file__).resolve().parent.parent
    config = tmp_path / "hosts.json"
    config.write_text('{"jitsi": ["https://*.example.org"]}')
    result = subprocess.run(
        ["bash", "-c", 'source "$1"; deps_asr_config_check', "--", str(root / "scripts/deps.sh")],
        env={**os.environ, "DEPS_PYTHON": sys.executable,
             "RECOG_ASR_BACKEND": "local", "RECOG_CONFERENCE_HOSTS_FILE": str(config)},
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "Invalid conference host configuration" in result.stdout


def test_legacy_index_when_listing_then_canonical_without_rewriting_cache(tmp_path, monkeypatch):
    from recog.meeting_storage import MeetingStorage

    monkeypatch.setitem(CONFERENCE_HOSTS, "source_aliases", {"old-meet": "jitsi"})
    storage = MeetingStorage(tmp_path / "meetings")
    path = storage.base_path / "index.json"
    raw = json.dumps({"meetings": [{"id": "old", "source": "old-meet"}]})
    path.write_text(raw)
    assert storage.list_meetings(include_active=False)[0]["source"] == "jitsi"
    assert path.read_text() == raw
