"""Recog.app: the application the live service runs under on macOS.

macOS attributes Screen Recording and the microphone to the responsible
process, which for a launchd job is the program the job starts. That used to
be the Homebrew interpreter, so the permission dialogs named "python3.12" and
every interpreter upgrade replaced its ad-hoc identity: a running service lost
the grant it was started with (ScreenCaptureKit answered -3801) and the new
interpreter had to be allowed again. The launchd job now starts
`Recog.app/Contents/MacOS/recog_launcher <python> -m recog live ...`; the
launcher runs the interpreter as its child, so the permissions belong to
"Recog" and survive interpreter upgrades.

Built by `setup.sh` (`python -m recog.macos_launcher`), checked by the
readiness gate in `scripts/deps.sh` before the service is started.
"""

from __future__ import annotations

import sys
from pathlib import Path

from recog.macos_bundle import build_app, find_or_build_app

REPO_ROOT = Path(__file__).resolve().parent.parent

# The permissions are recorded under the bundle identifier, so it must not change.
LAUNCHER_APP = "Recog.app"
LAUNCHER_BUNDLE_ID = "com.recog.live"
LAUNCHER_NAME = "Recog"
LAUNCHER_EXECUTABLE = "recog_launcher"
LAUNCHER_SOURCE = "recog/native/macos/recog_launcher.swift"
# Beside the bundle, not in it, for the reason given in recog/macos_bundle.py.
LAUNCHER_STAMP = "Recog.source-sha256"


def _launcher_info() -> dict:
    return {
        "CFBundleIdentifier": LAUNCHER_BUNDLE_ID,
        "CFBundleName": LAUNCHER_NAME,
        "CFBundleDisplayName": LAUNCHER_NAME,
        "CFBundleExecutable": LAUNCHER_EXECUTABLE,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "1.0",
        # No Dock icon, no menu bar: it only runs the service.
        "LSBackgroundOnly": True,
        # Shown in the microphone dialog, which macOS refuses to show without it.
        "NSMicrophoneUsageDescription": "Recog transcribes your side of the conversation.",
    }


def _build_launcher(app: Path, source: Path) -> None:
    print(f"Building {app.name} from {source}...", file=sys.stderr)
    build_app(app, source, info=_launcher_info())
    print(
        f'Built {app.name}: macOS asks again for Screen Recording and the microphone, now for "{LAUNCHER_NAME}"',
        file=sys.stderr,
    )


def find_or_build_launcher(repo_root: Path = REPO_ROOT) -> Path:
    """The launcher application; built when missing or when its source changed."""
    return find_or_build_app(
        repo_root / LAUNCHER_APP,
        repo_root / LAUNCHER_SOURCE,
        repo_root / LAUNCHER_STAMP,
        _build_launcher,
    )


def main() -> int:
    try:
        app = find_or_build_launcher()
    except RuntimeError as exc:
        print(exc, file=sys.stderr)
        return 1
    print(app / "Contents/MacOS" / LAUNCHER_EXECUTABLE)
    return 0


if __name__ == "__main__":
    sys.exit(main())
