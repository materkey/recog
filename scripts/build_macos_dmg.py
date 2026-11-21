"""Wrap a prebuilt Recog release in a Finder application and read-only disk image."""

import argparse
import base64
import binascii
import hashlib
import os
import plistlib
import re
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

from check_dmg_runtime import check_install
from release import ROOT, asset, release_tag

IMAGE = "Recog-macOS-arm64.dmg"
PUBLIC_KEY = ROOT / "deploy/macos/dmg/sparkle-public-key.txt"
FEED = "https://github.com/materkey/recog/releases/latest/download/appcast.xml"
# Universal binary from the full release archive pinned in fetch_sparkle.sh.
SPARKLE_BINARY_SHA256 = "9c78b2046e7637efeb8ca505d63170bc1557651925dbbfd45ac236845732b05a"
SPARKLE_LICENSE_SHA256 = "389a4e4e9a32f059775b13a06e25a591445ba229d2838d26dd3e7c0c45127cfe"


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


def update_feed(test_url):
    if test_url is None:
        return FEED
    url = urlsplit(test_url)
    if (
        os.environ.get("GITHUB_ACTIONS") == "true"
        or url.scheme not in ("http", "https")
        or url.hostname not in ("127.0.0.1", "::1", "localhost")
        or url.username is not None
        or url.password is not None
        or url.fragment
        or url.port == 0
        or any(char.isspace() or ord(char) < 32 for char in test_url)
    ):
        raise ValueError("--test-feed-url requires a local build and a loopback feed")
    return test_url


def update_settings(tag, public_key_file, test_url):
    part = r"(?:0|[1-9][0-9]*)"
    if not re.fullmatch(rf"v{part}\.{part}\.{part}", tag):
        raise ValueError("Sparkle disk images require a stable release tag vX.Y.Z")
    key = public_key_file.read_text(encoding="ascii").strip()
    try:
        raw = base64.b64decode(key, validate=True)
    except binascii.Error as error:
        raise ValueError("SUPublicEDKey is not valid base64") from error
    if len(raw) != 32 or base64.b64encode(raw).decode("ascii") != key:
        raise ValueError("SUPublicEDKey must be a base64-encoded 32-byte public key")
    return {
        "CFBundleShortVersionString": tag[1:],
        "CFBundleVersion": tag[1:],
        "SUFeedURL": update_feed(test_url),
        "SUPublicEDKey": key,
        "SUVerifyUpdateBeforeExtraction": True,
        "SURequireSignedFeed": True,
        "SUEnableAutomaticChecks": True,
    }


def check_macos13(binary):
    build = subprocess.check_output(
        ["/usr/bin/vtool", "-arch", "arm64", "-show-build", binary], text=True
    )
    minimum = re.search(r"^\s*minos (\d+(?:\.\d+){0,2})$", build, re.MULTILINE)
    if (
        not re.search(r"^\s*platform MACOS$", build, re.MULTILINE)
        or not minimum
        or tuple(map(int, minimum[1].split("."))) > (13, 0, 0)
    ):
        raise ValueError(f"Sparkle executable is incompatible with macOS 13: {binary}")


def embed_sparkle(contents, sdk):
    frameworks = contents / "Frameworks"
    frameworks.mkdir()
    framework = frameworks / "Sparkle.framework"
    run("/usr/bin/ditto", sdk / framework.name, framework)
    if digest(framework / "Sparkle") != SPARKLE_BINARY_SHA256:
        raise ValueError("Expected the official Sparkle 2.10.1 framework")
    run("/usr/bin/codesign", "--verify", "--all-architectures", "--deep", "--strict", framework)
    current = framework / "Versions/Current"
    for binary in (
        current / "Sparkle",
        current / "Autoupdate",
        current / "Updater.app/Contents/MacOS/Updater",
        current / "XPCServices/Downloader.xpc/Contents/MacOS/Downloader",
        current / "XPCServices/Installer.xpc/Contents/MacOS/Installer",
    ):
        check_macos13(binary)
    licenses = contents / "Resources/Licenses/Sparkle"
    licenses.mkdir(parents=True)
    shutil.copy2(sdk / "LICENSE", licenses / "LICENSE")
    if digest(licenses / "LICENSE") != SPARKLE_LICENSE_SHA256:
        raise ValueError("Expected the license from the full Sparkle 2.10.1 SDK")
    return frameworks


def make_app(app, archive, tag, ffmpeg, sdk, settings):
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
    frameworks = embed_sparkle(contents, sdk)
    with (contents / "Info.plist").open("wb") as stream:
        plistlib.dump(
            {
                "CFBundlePackageType": "APPL",
                "CFBundleIdentifier": "com.recog.manager",
                "CFBundleExecutable": "Recog",
                "CFBundleName": "Recog",
                "CFBundleIconFile": "Recog.icns",
                **settings,
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
        "-F",
        frameworks,
        "-framework",
        "Sparkle",
        "-Xlinker",
        "-rpath",
        "-Xlinker",
        "@executable_path/../Frameworks",
        ROOT / "deploy/macos/dmg/Recog.swift",
        "-o",
        macos / "Recog",
    )
    run("/usr/bin/codesign", "--force", "--sign", "-", app)
    run("/usr/bin/codesign", "--verify", "--deep", "--strict", app)


def verify_updater(app, settings):
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    if any(info.get(key) != value for key, value in settings.items()):
        raise ValueError("Mounted image has unexpected update settings or version")
    binary = app / "Contents/MacOS/Recog"
    libraries = subprocess.check_output(["/usr/bin/otool", "-L", binary], text=True)
    commands = subprocess.check_output(["/usr/bin/otool", "-l", binary], text=True)
    if (
        "@rpath/Sparkle.framework/Versions/B/Sparkle (" not in libraries
        or "path @executable_path/../Frameworks (" not in commands
    ):
        raise ValueError("The manager does not link to its embedded Sparkle framework")


def verify_image(image, mount, tag, settings):
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
        verify_updater(app, settings)
        payload = app / "Contents/Resources/recog"
        if (payload / "recog-release").read_text().strip() != tag:
            raise ValueError("Mounted image has the wrong release")
        run(payload / "bin/recog", "--help")
        run(payload / "bin/ffmpeg", "-version")
        check_install(payload, mount.parent / "test-user")
    finally:
        run("/usr/bin/hdiutil", "detach", mount)


def build(tag, directory, ffmpeg, sdk=None, public_key_file=PUBLIC_KEY, test_url=None):
    settings = update_settings(tag, public_key_file, test_url)
    archive = directory / asset(tag, "macos-arm64")
    with tempfile.TemporaryDirectory(dir=directory) as staging:
        stage = Path(staging)
        if sdk is None:
            sdk = stage / "sparkle-sdk"
            run(ROOT / "scripts/fetch_sparkle.sh", sdk)
        source = stage / "volume"
        make_app(source / "Recog.app", archive, tag, ffmpeg, sdk, settings)
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
        verify_image(image, stage / "mounted", tag, settings)
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
    parser.add_argument(
        "--sparkle-sdk",
        type=Path,
        help="use a directory from fetch_sparkle.sh (default: fetch during build)",
    )
    parser.add_argument("--public-key-file", type=Path, default=PUBLIC_KEY)
    parser.add_argument("--test-feed-url", help="loopback appcast URL for a local test image")
    args = parser.parse_args()
    build(
        args.tag,
        args.directory.resolve(),
        args.ffmpeg.resolve(),
        args.sparkle_sdk,
        args.public_key_file,
        args.test_feed_url,
    )
