"""Build a tagged Recog archive, or render a Homebrew formula for those archives."""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROJECT = "https://github.com/materkey/recog"


def release_tag(value):
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?", value):
        raise argparse.ArgumentTypeError("expected a version tag like v1.0.0")
    return value


def asset(tag, platform_name):
    return f"recog-{tag}-{platform_name}.tar.gz"


def archive(tag, output, binaries):
    target = {("Darwin", "arm64"): "macos-arm64", ("Linux", "x86_64"): "linux-x86_64"}[
        (platform.system(), platform.machine())
    ]
    names = [
        "recog",
        "recog-web",
        "recog-desktop",
        "recog-copilot-supervisor",
        "recog-copilot-channel",
    ]
    if target == "macos-arm64":
        names.append("recog-mic")
    if binaries is None:
        subprocess.run(
            ["cargo", "build", "--locked", "--release", *[x for n in names for x in ("--bin", n)]],
            cwd=ROOT,
            check=True,
        )
        metadata = subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT
        )
        binaries = Path(json.loads(metadata)["target_directory"]) / "release"

    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as directory:
        tree = Path(directory) / "recog"
        (tree / "bin").mkdir(parents=True)
        for name in names:
            shutil.copy2(binaries / name, tree / "bin" / name)
        # Package only tracked runtime resources. Local profiles, configuration,
        # conference hosts and meeting recordings never enter the archive.
        resources = [
            "LICENSE",
            "CHANGELOG.md",
            ".env.example",
            ".claude/agents/copilot-session.md",
            ".claude/skills/copilot-mode",
            ".claude/skills/recog-meetings",
            "crates/recog-web/static",
            "crates/recog-ai/prompts",
            "crates/recog-live/macos",
            "crates/recog-zoom/macos",
        ]
        files = subprocess.check_output(["git", "ls-files", "-z", "--", *resources], cwd=ROOT)
        for raw in files.split(b"\0"):
            if raw:
                name = raw.decode()
                (tree / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / name, tree / name)
        if target == "macos-arm64":
            mac = tree / "crates/recog-live/macos"
            subprocess.run(
                [
                    "swiftc",
                    mac / "sck_audio_capture.swift",
                    "-o",
                    tree / "sck_audio_capture",
                    "-framework",
                    "ScreenCaptureKit",
                    "-framework",
                    "CoreMedia",
                    "-framework",
                    "AVFoundation",
                ],
                check=True,
            )
            subprocess.run(
                [
                    "swiftc",
                    mac / "recog_permission_probe.swift",
                    "-o",
                    tree / "bin/recog-permission-probe",
                ],
                check=True,
            )
        # Build the extension outside the checkout. The temporary build has no
        # installation binary or configuration to inject developer-specific hosts.
        ext = Path(directory) / "source/browser-extension"
        shutil.copytree(
            ROOT / "browser-extension", ext, ignore=shutil.ignore_patterns("dist", "node_modules")
        )
        env = {
            **os.environ,
            "RECOG_CONFIG_FILE": str(ext / "absent.toml"),
            "RECOG_CONFERENCE_HOSTS_FILE": str(ext / "absent.json"),
        }
        subprocess.run(["npm", "ci"], cwd=ext, env=env, check=True)
        subprocess.run(["npm", "run", "build"], cwd=ext, env=env, check=True)
        shutil.copytree(ext / "dist", tree / "browser-extension/dist")
        (tree / "recog-release").write_text(tag + "\n")
        result = output / asset(tag, target)
        with tarfile.open(result, "w:gz") as tar:
            tar.add(tree, arcname="recog")
        print(result)


def formula(tag, directory):
    name = asset(tag, "macos-arm64")
    checksum = hashlib.sha256((directory / name).read_bytes()).hexdigest()
    return f'''class Recog < Formula
  desc "Live Russian conference transcription"
  homepage "{PROJECT}"
  url "{PROJECT}/releases/download/{tag}/{name}"
  version "{tag[1:]}"
  sha256 "{checksum}"
  license "MIT"
  depends_on arch: :arm64
  depends_on :macos
  depends_on "ffmpeg"

  def install
    libexec.install Dir["*", ".env.example", ".claude"]
    (libexec/"recog-homebrew").write("1\\n")
    bin.install_symlink libexec/"bin/recog"
  end

  test do
    assert_match "recog", shell_output("#{{bin}}/recog --help")
  end
end
'''


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("archive", "formula"))
    parser.add_argument("tag", type=release_tag)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--binaries", type=Path)
    args = parser.parse_args()
    if args.action == "archive":
        archive(args.tag, args.directory.resolve(), args.binaries)
    else:
        print(formula(args.tag, args.directory), end="")
