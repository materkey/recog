"""Profile persistence and identity precedence without touching installed data."""

from concurrent.futures import ThreadPoolExecutor
import json
import os
import threading

import pytest

from recog.user_profile import UserProfileStore, resolve_identity


def test_missing_profile_when_read_then_unconfigured_without_creating_directory(tmp_path):
    store = UserProfileStore(tmp_path / "data")
    assert store.read() == {"version": 1, "revision": "unconfigured", "name": "",
                            "aliases": [], "configured": False}
    assert not store.path.parent.exists()
    assert resolve_identity(store.read())["local_user_identity_known"] is False


def test_unicode_names_when_saved_then_normalized_private_and_round_trip(tmp_path):
    store = UserProfileStore(tmp_path)
    saved = store.write("  Александр\u2003", [" Саша ", "сАША", "", "  ", "Alex", "Me", "mE"])
    assert saved["name"] == "Александр"
    assert saved["aliases"] == ["Саша", "Alex"]
    assert saved["configured"] is True
    assert store.read() == saved
    assert store.path.stat().st_mode & 0o777 == 0o600
    assert "configured" not in json.loads(store.path.read_text())
    assert resolve_identity(saved)["local_user_aliases"] == ["Александр", "Саша", "Alex"]


@pytest.mark.parametrize("name,aliases", [
    ("", []), ("  ", []), ("mE", []), (None, []), (3, []),
    ("a" * 81, []), ("Alex\n", []), ("Alex\x00", []),
    ("Alex", "Al"), ("Alex", [None]), ("Alex", ["a" * 81]),
    ("Alex", ["a"] * 17), ("Alex", ["Al\t"]), ("Alex", ["A\u200bl"]),
])
def test_invalid_input_when_written_then_rejected_without_file(tmp_path, name, aliases):
    store = UserProfileStore(tmp_path)
    with pytest.raises(ValueError):
        store.write(name, aliases)
    assert not store.path.exists()


def test_configured_profile_when_cleared_then_revision_tombstone_persists(tmp_path):
    store = UserProfileStore(tmp_path)
    first = store.write("Alex", ["Al"])
    cleared = store.clear()
    assert cleared == {"version": 1, "revision": cleared["revision"], "name": "",
                       "aliases": [], "configured": False}
    assert cleared["revision"] not in (first["revision"], "unconfigured")
    assert UserProfileStore(tmp_path).read() == cleared
    again = store.clear()
    assert again["revision"] != cleared["revision"]


@pytest.mark.parametrize("payload", [
    b"not json", b"\xff", b"[]", b"{}", b"x" * 16385,
    json.dumps({"version": True, "revision": "a", "name": "Alex", "aliases": []}).encode(),
    json.dumps({"version": 2, "revision": "a", "name": "Alex", "aliases": []}).encode(),
    json.dumps({"version": 1, "revision": "", "name": "Alex", "aliases": []}).encode(),
    json.dumps({"version": 1, "revision": None, "name": "Alex", "aliases": []}).encode(),
    json.dumps({"version": 1, "revision": "r", "name": "Me", "aliases": []}).encode(),
    json.dumps({"version": 1, "revision": "r", "name": "", "aliases": ["Alex"]}).encode(),
])
def test_corrupt_profile_when_read_or_saved_then_error_preserves_bytes(tmp_path, payload):
    store = UserProfileStore(tmp_path)
    store.path.write_bytes(payload)
    for action in (store.read, lambda: store.write("Alex", []), store.clear):
        with pytest.raises(ValueError):
            action()
        assert store.path.read_bytes() == payload


def test_replace_failure_when_saved_then_previous_profile_preserved_and_temp_removed(tmp_path, monkeypatch):
    store = UserProfileStore(tmp_path)
    old = store.write("Alex", [])
    before = store.path.read_bytes()

    def fail_replace(source, destination):
        assert os.stat(source).st_mode & 0o777 == 0o600
        assert store.read() == old
        raise OSError("simulated replace failure")

    monkeypatch.setattr("recog.user_profile.os.replace", fail_replace)
    with pytest.raises(OSError, match="replace failure"):
        store.write("New name", [])
    assert store.path.read_bytes() == before
    assert list(tmp_path.iterdir()) == [store.path]


def test_fsync_failure_when_saved_then_previous_profile_preserved(tmp_path, monkeypatch):
    store = UserProfileStore(tmp_path)
    old = store.write("Alex", [])

    def fail_fsync(fd):
        raise OSError("simulated fsync failure")

    monkeypatch.setattr("recog.user_profile.os.fsync", fail_fsync)
    with pytest.raises(OSError, match="fsync failure"):
        store.clear()
    assert store.read() == old
    assert list(tmp_path.iterdir()) == [store.path]


def test_concurrent_stores_when_saving_then_unique_revisions_and_complete_reads(tmp_path):
    store = UserProfileStore(tmp_path)
    store.write("Initial", [])
    stop = threading.Event()
    observed = []

    def read_until_done():
        while not stop.is_set():
            observed.append(store.read())

    with ThreadPoolExecutor(max_workers=5) as pool:
        reader = pool.submit(read_until_done)
        try:
            saved = list(pool.map(lambda n: UserProfileStore(tmp_path).write(f"Name {n}", []), range(20)))
        finally:
            stop.set()
        reader.result()
    assert observed
    assert len({item["revision"] for item in saved}) == 20
    assert store.read() in saved
    assert all(item["configured"] and item["name"] for item in observed)
    assert list(tmp_path.iterdir()) == [store.path]


@pytest.mark.parametrize("configured,legacy,override,name,aliases", [
    (False, "Me", None, "Me", []),
    (False, "  ", None, "Me", []),
    (False, "Legacy", None, "Legacy", ["Legacy"]),
    (False, "Legacy", "CLI", "CLI", ["CLI"]),
    (True, "Legacy", None, "Alex", ["Alex", "Al"]),
    (True, "Legacy", "CLI", "CLI", ["Alex", "CLI", "Al"]),
    (True, "Legacy", "alex", "alex", ["Alex", "Al"]),
    (True, "Legacy", "Me", "Me", ["Alex", "Al"]),
    (False, "Legacy", "Me", "Me", []),
])
def test_identity_inputs_when_resolved_then_precedence_and_aliases_match(tmp_path, configured, legacy, override, name, aliases):
    store = UserProfileStore(tmp_path)
    profile = store.write("Alex", ["Al"]) if configured else store.read()
    assert resolve_identity(profile, legacy_name=legacy, override=override) == {
        "local_user_name": name, "local_user_aliases": aliases,
        "local_user_profile_revision": profile["revision"],
        "local_user_identity_known": bool(aliases),
    }


def test_saved_profile_when_cleared_then_legacy_returns_without_old_aliases(tmp_path):
    store = UserProfileStore(tmp_path)
    store.write("Alex", ["Al"])
    identity = resolve_identity(store.clear(), legacy_name="Legacy")
    assert identity["local_user_name"] == "Legacy"
    assert identity["local_user_aliases"] == ["Legacy"]
