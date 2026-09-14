"""Private installation state and immutable extension snapshots."""

import hashlib
import json
import os
import shutil
import tempfile
from pathlib import Path


def read_json(path: Path, default=None):
    try:
        return json.loads(path.read_text())
    except FileNotFoundError:
        return {} if default is None else default


def write_json(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, name = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(value, f, ensure_ascii=False)
            f.flush()
            os.fsync(f.fileno())
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


def write_status(root: Path, result: dict) -> dict:
    """Keep an unresolved inspector warning until closure is established."""
    existing = read_json(root / "status.json")
    if (
        existing.get("state") == "inspector_unclosed"
        and result.get("state") != "inspector_unclosed"
        and result.get("inspector_closed") is not True
        and read_json(root / "operation.json").get("phase") != "closed"
    ):
        return existing
    write_json(root / "status.json", result)
    return result


def bundle_digest(directory: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(directory.rglob("*")):
        if path.is_symlink():
            raise ValueError("Extension snapshot cannot contain symbolic links")
        if path.is_file():
            digest.update(path.relative_to(directory).as_posix().encode() + b"\0")
            digest.update(path.read_bytes())
    return digest.hexdigest()


def prepare_bundle(source: Path, root: Path) -> tuple[str, Path]:
    for name in ("manifest.json", "injected.js", "src/audio-worklet/downsampler.js"):
        if not (source / name).is_file():
            raise ValueError("Build browser-extension before enabling desktop integration")
    digest = bundle_digest(source)
    parent = root / "extensions"
    parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    target = parent / digest
    if not target.exists():
        temporary = Path(tempfile.mkdtemp(prefix="stage-", dir=parent))
        try:
            shutil.copytree(source, temporary, dirs_exist_ok=True)
            if bundle_digest(temporary) != digest:
                raise ValueError("Extension build changed while taking snapshot")
            os.rename(temporary, target)
        finally:
            if temporary.exists():
                shutil.rmtree(temporary)
    if bundle_digest(target) != digest:
        raise ValueError("Extension snapshot integrity check failed")
    return digest, target


def validate_bundle(root: Path, config: dict) -> Path:
    digest = config.get("bundle", "")
    if len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
        raise ValueError("Invalid extension snapshot identifier")
    path = root / "extensions" / digest
    if not path.is_dir() or path.is_symlink() or bundle_digest(path) != digest:
        raise ValueError("Extension snapshot integrity check failed")
    return path
