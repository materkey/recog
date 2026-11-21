"""Sign a release appcast on the trusted Mac or verify it with the public key."""

import argparse
import base64
import hashlib
import os
import plistlib
import re
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

PROJECT = "https://github.com/materkey/recog"
IMAGE = "Recog-macOS-arm64.dmg"
FEED = f"{PROJECT}/releases/latest/download/appcast.xml"
NS = "{http://www.andymatuschak.org/xml-namespaces/sparkle}"
ACCOUNT = "com.recog.manager.sparkle"
SCRIPTS = Path(__file__).resolve().parent
PUBLIC_KEY = SCRIPTS.parent / "deploy/macos/dmg/sparkle-public-key.txt"


def stable_tag(text):
    if not re.fullmatch(r"v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", text):
        raise argparse.ArgumentTypeError("expected a stable release tag such as v0.3.0")
    return text


def public_key(path):
    key = path.read_text().strip()
    if len(base64.b64decode(key, validate=True)) != 32:
        raise ValueError("Expected an Ed25519 public key")
    return key


def signed_feed_data(feed):
    content, marker, footer = feed.rpartition(b"<!-- sparkle-signatures:\n")
    match = re.fullmatch(rb"edSignature: ([A-Za-z0-9+/=]+)\nlength: (\d+)\n-->\s*", footer)
    if not marker or not match or int(match[2]) != len(content):
        raise ValueError("Missing or invalid Sparkle signed-feed trailer")
    return content, match[1].decode()


def image_signature(content, tag, length):
    channel = ET.fromstring(content).find("channel")
    items = channel.findall("item") if channel is not None else []
    if len(items) != 1:
        raise ValueError("Expected one complete update in the stable feed")
    item = items[0]
    versions = [item.findtext(NS + k) for k in ("version", "shortVersionString")]
    if versions != [tag[1:], tag[1:]] or item.findtext(NS + "minimumSystemVersion") != "13.0":
        raise ValueError("The feed describes a different app version or system requirement")
    entries = item.findall("enclosure")
    if len(entries) != 1 or item.find(NS + "deltas") is not None:
        raise ValueError("Expected one full disk image")
    enclosure = entries[0]
    if enclosure.get("url") != f"{PROJECT}/releases/download/{tag}/{IMAGE}":
        raise ValueError("The enclosure must point to the immutable versioned release URL")
    if enclosure.get("length") != str(length):
        raise ValueError("The feed length differs from the disk image")
    signature = enclosure.attrib[NS + "edSignature"]
    if len(base64.b64decode(signature, validate=True)) != 64:
        raise ValueError("Invalid disk image signature")
    return signature


def verify(tag, image, feed, key):
    public_key(key)
    content, feed_signature = signed_feed_data(feed.read_bytes())
    signature = image_signature(content, tag, image.stat().st_size)
    with tempfile.TemporaryDirectory() as temp:
        data = Path(temp) / "signed-feed.xml"
        data.write_bytes(content)
        subprocess.run(
            [
                "swift",
                SCRIPTS / "verify_sparkle_signatures.swift",
                key,
                data,
                feed_signature,
                image,
                signature,
            ],
            check=True,
        )


def check_bundle(mount, tag, key):
    app = mount / "Recog.app"
    subprocess.run(["/usr/bin/codesign", "--verify", "--strict", "--deep", app], check=True)
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    expected = {
        "CFBundleIdentifier": "com.recog.manager",
        "CFBundleVersion": tag[1:],
        "CFBundleShortVersionString": tag[1:],
        "SUFeedURL": FEED,
        "SUPublicEDKey": key,
        "SURequireSignedFeed": True,
        "SUVerifyUpdateBeforeExtraction": True,
    }
    for name, value in expected.items():
        if info.get(name) != value:
            raise ValueError(f"Unexpected {name} in the disk image")
    if (app / "Contents/Resources/recog/recog-release").read_text().strip() != tag:
        raise ValueError("Bundled runtime does not match the release")


def sign(args):
    # Supply the digest of the actual GitHub draft asset, not a digest
    # calculated from an arbitrary file submitted to the signing machine.
    if args.image.name != IMAGE:
        raise ValueError("Expected the Recog disk image")
    tools = args.sdk / "bin"
    key = public_key(args.public_key)
    with tempfile.TemporaryDirectory(dir=args.feed.parent) as temp:
        staging = Path(temp)
        updates = staging / "updates"
        updates.mkdir()
        image = updates / IMAGE
        shutil.copyfile(args.image, image)
        if hashlib.sha256(image.read_bytes()).hexdigest() != args.sha256:
            raise ValueError("The staged disk image differs from the GitHub release digest")
        mount = staging / "mount"
        mount.mkdir()
        subprocess.run(
            ["hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", mount, image], check=True
        )
        try:
            check_bundle(mount, args.tag, key)
        finally:
            subprocess.run(["hdiutil", "detach", mount], check=True)
        signing = subprocess.run(
            [
                tools / "generate_appcast",
                "--account",
                ACCOUNT,
                "--maximum-deltas",
                "0",
                "--maximum-versions",
                "1",
                "--download-url-prefix",
                f"{PROJECT}/releases/download/{args.tag}/",
                "--link",
                "https://recog.vkkovalev.work/",
                "--full-release-notes-url",
                f"{PROJECT}/releases/tag/{args.tag}",
                updates,
            ],
            capture_output=True,
            check=False,
        )
        if signing.returncode:
            raise RuntimeError(
                f"Sparkle signing failed ({signing.returncode}); check access to the {ACCOUNT} Keychain item"
            )
        feed = updates / "appcast.xml"
        verify(args.tag, image, feed, args.public_key)
        os.replace(feed, args.feed)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="action", required=True)
    for action in ("sign", "verify"):
        sub = subs.add_parser(action)
        sub.add_argument("tag", type=stable_tag)
        sub.add_argument("image", type=Path)
        sub.add_argument("feed", type=Path)
        sub.add_argument("--public-key", type=Path, default=PUBLIC_KEY)
        if action == "sign":
            sub.add_argument("--sdk", type=Path, required=True)
            sub.add_argument("--sha256", required=True)
    options = parser.parse_args()
    if options.action == "sign":
        sign(options)
    else:
        verify(options.tag, options.image, options.feed, options.public_key)
