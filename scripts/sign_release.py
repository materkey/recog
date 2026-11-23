"""Sign a release's update feed and Linux repository on the signing Mac.

Both private keys live only in the login Keychain of one Mac, reached over ssh
(docs/sparkle-updates.md, docs/linux-packages.md). This release workstation
downloads the disk image and the Linux indexes, checks them against the digests
GitHub reports, and sends them with the signing scripts to that Mac. There a
one-shot launchd job in the user's graphical session (gui/<uid>) signs both:
the Keychain refuses an ssh session ("User interaction is not allowed"), and
`launchctl asuser` needs root. The results come back to OUT and are
verified here; with --upload they are attached to the draft and
`linux_repo.py ready` checks the whole draft.
"""

import argparse
import os
import shlex
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

import linux_repo

ROOT = Path(__file__).resolve().parents[1]
IMAGE = "Recog-macOS-arm64.dmg"
SIGNED = ("appcast.xml", f"linux-repo/{linux_repo.IN_RELEASE}", f"linux-repo/{linux_repo.DB_SIG}")
# Copied in the repository's layout, so each script finds its public key.
SHIPPED = (
    "scripts/sparkle_appcast.py",
    "scripts/verify_sparkle_signatures.swift",
    "scripts/linux_repo.py",
    "deploy/macos/dmg/sparkle-public-key.txt",
    "deploy/landing/recog.asc",
)
JOB = "com.recog.release-sign"
SCRIPT = """#!/bin/bash
set -u
umask 077
d=$(cd "$(dirname "$0")" && pwd -P)
cd "$d"
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
python3 scripts/sparkle_appcast.py sign {tag} input/{image} appcast.xml \\
  --sdk "$HOME/.local/share/recog-signing/Sparkle-2.10.1" --sha256 "$(cat input/github-digest)" >sparkle.log 2>&1
sparkle=$?
python3 scripts/linux_repo.py sign {tag} linux-repo >linux.log 2>&1
printf '%s %s\\n' "$sparkle" "$?" >done
"""


def ssh(host, command, **options):
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", host, command], check=True, text=True, **options
    )


def inputs(tag, work):
    """The draft's disk image and Linux indexes, as GitHub publishes them."""
    image_dir = work / "input"
    files, _ = linux_repo.download(tag, (IMAGE,), image_dir)
    (image_dir / "github-digest").write_text(files[IMAGE][1] + "\n")
    linux_repo.signing_input(tag, work / "linux-repo")


def ship(tag, work):
    for name in SHIPPED:
        (work / name).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / name, work / name)
    script = work / "sign.sh"
    script.write_text(SCRIPT.format(tag=tag, image=IMAGE))
    script.chmod(0o700)


def wait_for(probe, attempts, pause):
    """The first non-empty answer of probe, asked at most attempts times."""
    for _ in range(attempts):
        time.sleep(pause)
        answer = probe()
        if answer:
            return answer
    return ""


def remote_output(host, command):
    return ssh(host, command, capture_output=True).stdout.strip()


def require_clean(host, remote, done):
    """Both signings exited 0, or the end of their logs explains why not."""
    if done != "0 0":
        logs = remote_output(host, f"cd {shlex.quote(remote)} && tail -n 5 sparkle.log linux.log")
        raise SystemExit(
            f"Signing on {host} did not finish cleanly ({done or 'timed out'}):\n{logs}"
        )


def fetch_signed(host, remote, work):
    for name in SIGNED:
        subprocess.run(["scp", "-q", f"{host}:{remote}/{name}", str(work / name)], check=True)


# Run on the signing Mac with the remote directory as $1.
START = f"""set -eu
plist="$1/{JOB}.plist"
cat >"$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{JOB}</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>$1/sign.sh</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
PLIST
launchctl bootout "gui/$(id -u)/{JOB}" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$plist"
"""
STOP = f'launchctl bootout "gui/$(id -u)/{JOB}" 2>/dev/null || true; rm -rf "$1"'


def sign_remotely(host, work, timeout):
    """Run the signing job on the signing Mac; the job and the remote copy are removed afterwards."""
    remote = remote_output(host, "umask 077; mktemp -d /tmp/recog-release-sign.XXXXXXXX")
    quoted = shlex.quote(remote)
    try:
        subprocess.run(["scp", "-q", "-r", f"{work}/.", f"{host}:{remote}/"], check=True)
        ssh(host, f"bash -s -- {quoted}", input=START)
        done = wait_for(
            lambda: remote_output(host, f"cat {quoted}/done 2>/dev/null || true"), timeout // 3, 3
        )
        require_clean(host, remote, done)
        fetch_signed(host, remote, work)
    finally:
        ssh(host, f"bash -s -- {quoted}", input=STOP)


def check_here(tag, work):
    subprocess.run(
        [
            "python3",
            ROOT / "scripts/sparkle_appcast.py",
            "verify",
            tag,
            work / "input" / IMAGE,
            work / "appcast.xml",
        ],
        check=True,
    )
    repo = work / "linux-repo"
    indexes = {name: (repo / name).read_bytes() for name in linux_repo.INDEXES}
    signatures = [(repo / name).read_bytes() for name in (linux_repo.IN_RELEASE, linux_repo.DB_SIG)]
    linux_repo.check_signatures(indexes, *signatures, linux_repo.PUBLIC_KEY.read_bytes())


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("tag", type=linux_repo.stable_tag)
    parser.add_argument("out", type=Path, help="where the signed files are kept")
    parser.add_argument(
        "--mac", default=os.environ.get("RECOG_SIGNING_MAC"), help="ssh host of the signing Mac"
    )
    parser.add_argument(
        "--timeout", type=int, default=900, help="seconds to wait for the signing window"
    )
    parser.add_argument(
        "--upload", action="store_true", help="attach the signed files to the draft"
    )
    args = parser.parse_args()
    if not args.mac:
        parser.error(
            "name the signing Mac: --mac HOST or RECOG_SIGNING_MAC (~/.config/recog/release.env)"
        )
    with tempfile.TemporaryDirectory() as temp:
        work = Path(temp)
        inputs(args.tag, work)
        ship(args.tag, work)
        sign_remotely(args.mac, work, args.timeout)
        check_here(args.tag, work)
        signed = keep(work, args.out)
    print(f"Signed and verified: {', '.join(path.name for path in signed)} in {args.out}")
    if args.upload:
        upload(args.tag, signed)


def keep(work, out):
    out.mkdir(parents=True, exist_ok=True)
    return [Path(shutil.copy(work / name, out / Path(name).name)) for name in SIGNED]


def upload(tag, signed):
    subprocess.run(
        ["gh", "release", "upload", tag, *map(str, signed), "--repo", linux_repo.PROJECT],
        check=True,
    )
    linux_repo.ready(argparse.Namespace(tag=tag))


if __name__ == "__main__":
    main()
