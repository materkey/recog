"""Wrap a prebuilt Recog release in a Finder application and read-only disk image."""

import argparse
import hashlib
import os
import plistlib
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path

from check_dmg_runtime import check_install
from release import ROOT, asset, release_tag

IMAGE = "Recog-macOS-arm64.dmg"


def run(*args):
    subprocess.run(args, check=True)


def digest(path):
    with path.open("rb") as stream:
        checksum = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(block)
        return checksum.hexdigest()


def unpack(archive, resources, tag):
    with tarfile.open(archive, "r:gz") as tar:
        entries = tar.getmembers()
        for entry in entries:
            parts = Path(entry.name).parts
            if parts[:1] != ("recog",) or ".." in parts or not (entry.isdir() or entry.isfile()):
                raise ValueError(f"Unexpected entry in the release archive: {entry.name}")
        if tar.extractfile("recog/recog-release").read().decode().strip() != tag:
            raise ValueError("Archive release does not match the requested disk image")
        options = {"filter": "data"} if hasattr(tarfile, "data_filter") else {}
        tar.extractall(resources, members=entries, **options)
    return resources / "recog"


def make_app(app, archive, tag, ffmpeg):
    contents = app / "Contents"
    macos = contents / "MacOS"
    resources = contents / "Resources"
    macos.mkdir(parents=True)
    resources.mkdir()
    payload = unpack(archive, resources, tag)
    for bundle in ["Recog.app", "RecogZoomHelper.app"]:
        run("/usr/bin/codesign", "--verify", "--strict", payload / bundle)
    run("/usr/bin/codesign", "--verify", "--strict", ffmpeg / "ffmpeg")
    shutil.copy2(ffmpeg / "ffmpeg", payload / "bin/ffmpeg")
    web = payload / "bin/recog-web"
    web.rename(payload / "bin/recog-web-release")
    shutil.copyfile(ROOT / "deploy/macos/dmg/recog-web.sh", web)
    web.chmod(0o755)
    shutil.copytree(ffmpeg / "licenses", resources / "Licenses/FFmpeg")
    shutil.copy2(payload / "Recog.app/Contents/Resources/Recog.icns", resources / "Recog.icns")
    version = tag[1:].split("-")[0]
    with (contents / "Info.plist").open("wb") as stream:
        plistlib.dump(
            {
                "CFBundlePackageType": "APPL",
                "CFBundleIdentifier": "com.recog.manager",
                "CFBundleExecutable": "Recog",
                "CFBundleName": "Recog",
                "CFBundleIconFile": "Recog.icns",
                "CFBundleShortVersionString": version,
                "CFBundleVersion": version,
                "LSMinimumSystemVersion": "13.0",
                "NSHighResolutionCapable": True,
                "RecogReleaseArchiveSHA256": digest(archive),
            },
            stream,
        )
    run(
        "swiftc",
        "-O",
        "-target",
        "arm64-apple-macosx13.0",
        ROOT / "deploy/macos/dmg/Recog.swift",
        "-o",
        macos / "Recog",
    )
    run("/usr/bin/codesign", "--force", "--sign", "-", app)
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", app)


def verify_image(image, mount, tag):
    mount.mkdir()
    run(
        "/usr/bin/hdiutil",
        "attach",
        "-readonly",
        "-nobrowse",
        "-mountpoint",
        mount,
        image,
    )
    try:
        if os.readlink(mount / "Applications") != "/Applications":
            raise ValueError("Missing Finder installation target")
        app = mount / "Recog.app"
        run("/usr/bin/codesign", "--verify", "--deep", "--strict", app)
        payload = app / "Contents/Resources/recog"
        if (payload / "recog-release").read_text().strip() != tag:
            raise ValueError("Mounted image has the wrong release")
        run(payload / "bin/recog", "--help")
        run(payload / "bin/ffmpeg", "-version")
        check_install(payload, mount.parent / "test-user")
    finally:
        run("/usr/bin/hdiutil", "detach", mount)


def build(tag, directory, ffmpeg):
    archive = directory / asset(tag, "macos-arm64")
    with tempfile.TemporaryDirectory(dir=directory) as staging:
        stage = Path(staging)
        source = stage / "volume"
        make_app(source / "Recog.app", archive, tag, ffmpeg)
        (source / "Applications").symlink_to("/Applications")
        image = stage / IMAGE
        run(
            "/usr/bin/hdiutil",
            "create",
            "-srcfolder",
            source,
            "-volname",
            "Recog",
            "-format",
            "UDZO",
            image,
        )
        verify_image(image, stage / "mounted", tag)
        image.replace(directory / IMAGE)
    (directory / (IMAGE + ".sha256")).write_text(f"{digest(directory / IMAGE)}  {IMAGE}\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag", type=release_tag)
    parser.add_argument(
        "directory",
        type=Path,
        help="release archive directory, also receives the disk image",
    )
    parser.add_argument("ffmpeg", type=Path, help="output directory from build_dmg_ffmpeg.sh")
    args = parser.parse_args()
    build(args.tag, args.directory.resolve(), args.ffmpeg.resolve())
