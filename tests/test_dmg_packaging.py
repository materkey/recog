"""Only the requested release and its ordinary files can enter a Finder application."""

import importlib
import io
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
