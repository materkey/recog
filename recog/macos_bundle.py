"""Signed application bundles built from a single Swift source (macOS).

macOS records a privacy permission (Accessibility, Screen Recording, the
microphone) under the signature of the application responsible for the
process, so a helper that needs one is built as its own bundle with a stable
identifier. The bundle is rebuilt only when its source changes: an ad-hoc
signature is the hash of the bundle, and any change of it costs the user the
permission.
"""

from __future__ import annotations

import hashlib
import plistlib
import shutil
import subprocess
from collections.abc import Callable, Sequence
from pathlib import Path

from recog.install_config import INSTALL_CONFIG


def build_app(app: Path, source: Path, *, info: dict, frameworks: Sequence[str] = ()) -> None:
    """Compile `source` into the executable of `app`, write `info` and sign the bundle."""
    # Built aside and swapped in on success: a failed build must not take a
    # working application away.
    staging = app.with_name(app.name + ".build")
    shutil.rmtree(staging, ignore_errors=True)
    contents = staging / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    executable = contents / "MacOS" / info["CFBundleExecutable"]
    compile_command = ["swiftc", "-O", "-o", str(executable), str(source)]
    for framework in frameworks:
        compile_command += ["-framework", framework]
    with (contents / "Info.plist").open("wb") as handle:
        plistlib.dump(info, handle)
    # macOS recognizes the permission holder by its signature. "-" is an ad-hoc
    # signature, which is the hash of the bundle: it stays the same while the
    # code does, and the permission must be granted again once the code
    # changes. A real identity keeps the grant across such changes too.
    identity = INSTALL_CONFIG.macos.codesign_identity or "-"
    sign_command = [
        "codesign",
        "--force",
        "--sign",
        identity,
        "--identifier",
        info["CFBundleIdentifier"],
        str(staging),
    ]
    for command in (compile_command, sign_command):
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        if result.returncode != 0:
            raise RuntimeError(
                f"Failed to build {app.name} ({command[0]}): {result.stderr.strip()}"
            )
    shutil.rmtree(app, ignore_errors=True)
    staging.rename(app)


def find_or_build_app(
    app: Path, source: Path, stamp: Path, build: Callable[[Path, Path], None]
) -> Path:
    """The application; `build(app, source)` runs when it is missing or its source changed.

    Freshness is the source's content hash, not its mtime, so a checkout that
    merely touches the file does not rebuild. The hash is kept in `stamp`
    beside the bundle, not in it: everything inside is sealed by the
    signature. The stamp is written last: a build that failed half-way is
    repeated on the next call.
    """
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    built = (app / "Contents/_CodeSignature/CodeResources").exists()
    if built and stamp.exists() and stamp.read_text().strip() == digest:
        return app
    if not shutil.which("swiftc"):
        if built:
            return app  # a stale application is better than none
        raise RuntimeError(f"swiftc not found — cannot build {app.name} (xcode-select --install)")
    build(app, source)
    stamp.write_text(digest)
    return app
