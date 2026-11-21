"""Only the requested release and its ordinary files can enter a Finder application."""

import base64
import importlib
import io
import subprocess
import sys
import tarfile
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
dmg = importlib.import_module("build_macos_dmg")


@pytest.mark.parametrize("invalid", ["version", "parent", "link", None])
def test_release_archive_when_unpacked_then_only_the_expected_payload_is_extracted(
    tmp_path, invalid
):
    archive = tmp_path / "archive.tgz"
    resources = tmp_path / "Resources"
    resources.mkdir()
    with tarfile.open(archive, "w:gz") as tar:
        release = tarfile.TarInfo("recog/recog-release")
        contents = b"v0.1.5" if invalid == "version" else b"v0.2.0"
        release.size = len(contents)
        tar.addfile(release, io.BytesIO(contents))
        entry = tarfile.TarInfo("recog/../outside" if invalid == "parent" else "recog/bin/recog")
        if invalid == "link":
            entry.type = tarfile.SYMTYPE
            entry.linkname = "/outside"
        tar.addfile(entry)
    if invalid:
        with pytest.raises(ValueError):
            dmg.unpack(archive, resources, "v0.2.0")
        assert list(resources.iterdir()) == []
    else:
        root = dmg.unpack(archive, resources, "v0.2.0")
        assert (root / "bin/recog").is_file()


def test_stable_archive_when_packaged_then_appcast_uses_both_exact_bundle_versions(tmp_path):
    key = base64.b64encode(bytes(range(32))).decode()
    key_file = tmp_path / "public-key"
    key_file.write_text(key + "\n")
    settings = dmg.update_settings("v0.10.20", key_file, None)
    assert settings == {
        "CFBundleVersion": "0.10.20",
        "CFBundleShortVersionString": "0.10.20",
        "SUPublicEDKey": key,
        "SUFeedURL": "https://github.com/materkey/recog/releases/latest/download/appcast.xml",
        "SURequireSignedFeed": True,
        "SUVerifyUpdateBeforeExtraction": True,
        "SUEnableAutomaticChecks": True,
    }


@pytest.mark.parametrize("tag", ["v0.2.1-beta.1", "v0.2.1-rc", "v0.2.01", "v0.2.1+test"])
def test_unstable_or_ambiguous_version_when_packaged_then_no_stable_number_is_assigned(
    tmp_path, tag
):
    with pytest.raises(ValueError, match="stable release"):
        dmg.update_settings(tag, tmp_path / "missing", None)


@pytest.mark.parametrize("key", ["", "invalid!", "AA==", "A" * 43 + "B", "A" * 42 + "B="])
def test_invalid_key_when_packaged_then_rejected(tmp_path, key):
    key_file = tmp_path / "key"
    key_file.write_text(key)
    with pytest.raises(ValueError, match="SUPublicEDKey"):
        dmg.update_settings("v0.2.0", key_file, None)


def test_loopback_feed_when_requested_then_allowed_only_for_local_builds(monkeypatch):
    feed = "http://[::1]:8123/appcast.xml"
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    assert dmg.update_feed(feed) == feed
    for invalid in (
        "https://example.org/appcast.xml",
        "file:///tmp/appcast.xml",
        "http://127.0.0.1@example.org/feed",
        "http://user@localhost/feed",
    ):
        with pytest.raises(ValueError):
            dmg.update_feed(invalid)
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    with pytest.raises(ValueError):
        dmg.update_feed(feed)
    assert dmg.update_feed(None) == dmg.FEED


@pytest.mark.parametrize(
    "platform,minimum,allowed",
    [
        ("MACOS", "12.0", True),
        ("MACOS", "13.0.0", True),
        ("MACOS", "13.0.1", False),
        ("IOS", "12.0", False),
        ("MACOS", "unknown", False),
    ],
)
def test_framework_helper_when_checked_then_must_run_on_macos_13(
    monkeypatch, platform, minimum, allowed
):
    def vtool(args, text):
        assert args[1:4] == ["-arch", "arm64", "-show-build"]
        return f" platform {platform}\n    minos {minimum}\n"

    monkeypatch.setattr(dmg.subprocess, "check_output", vtool)
    if allowed:
        dmg.check_macos13("Sparkle")
    else:
        with pytest.raises(ValueError, match="macOS 13"):
            dmg.check_macos13("Sparkle")


def test_cached_sdk_when_archive_checksum_is_wrong_then_fetch_preserves_existing_directory(
    tmp_path,
):
    destination = tmp_path / "sdk"
    destination.mkdir()
    sentinel = destination / "existing"
    sentinel.write_text("keep")
    (tmp_path / "Sparkle-2.10.1.tar.xz").write_bytes(b"incorrect archive")
    result = subprocess.run(
        [dmg.ROOT / "scripts/fetch_sparkle.sh", destination], capture_output=True
    )
    assert result.returncode != 0
    assert b"FAILED" in result.stdout
    assert list(destination.iterdir()) == [sentinel]
    assert sentinel.read_text() == "keep"
