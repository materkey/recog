#!/usr/bin/env python3
"""Prepare the GigaAM weight cache without ever building a model.

`setup.sh` runs this so the first meeting does not stall on a ~450 MB
download, and so a re-run after `git pull` is a no-op when the cache is
already valid.

Three constraints shape the implementation:

* **No model in RAM.** `gigaam.load_model` and `torch.load` are never called —
  this may run next to a live service on the same machine. Upstream's
  `hash_path` is avoided for the same reason: it does
  ``open(path, "rb").read()``, i.e. the whole checkpoint in memory. Everything
  here streams in fixed-size blocks.
* **Atomic publication.** A download lands in a temp file in the destination
  directory and is renamed only after its checksum matches, so a live pipeline
  can never observe a partial checkpoint, and a failure leaves the previous
  cache untouched.
* **Checksums come from the pinned GigaAM.** The URL and per-model MD5s are
  parsed out of the installed package's ``__init__.py`` with `ast` instead of
  imported, because importing `gigaam` pulls in torch.

Exit code 0 means "cache is ready"; 1 means "cache unchanged, weights not
ready" — setup.sh turns that into a warning, not a failed install.
"""

import argparse
import ast
import hashlib
import importlib.util
import os
import shutil
import sys
import tempfile
import urllib.request
from pathlib import Path
from typing import Dict, Optional, Tuple

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))

# Upstream default (gigaam/__init__.py:_CACHE_DIR). Not parsed from the source
# like the other two constants: it is an os.path.expanduser() call, not a
# literal.
DEFAULT_CACHE_DIR = Path("~/.cache/gigaam").expanduser()
BLOCK_SIZE = 1 << 20  # 1 MiB — bounds the working buffer for hash + download
DEFAULT_TIMEOUT_S = 60.0


class PrefetchError(RuntimeError):
    """Cache could not be brought up to date; the existing cache is intact."""


# ---------------------------------------------------------------------------
# Constants from the installed (pinned) GigaAM
# ---------------------------------------------------------------------------


def gigaam_init_path() -> Path:
    """Path of the installed gigaam/__init__.py, without importing it."""
    spec = importlib.util.find_spec("gigaam")
    if spec is None or not spec.origin:
        raise PrefetchError(
            "gigaam is not installed in this interpreter — run ./setup.sh first"
        )
    return Path(spec.origin)


def parse_gigaam_constants(source: str) -> Tuple[str, Dict[str, str]]:
    """Extract (_URL_DIR, _MODEL_HASHES) from gigaam/__init__.py source."""
    url_dir: Optional[str] = None
    hashes: Optional[Dict[str, str]] = None

    for node in ast.parse(source).body:
        if not isinstance(node, ast.Assign):
            continue
        names = [t.id for t in node.targets if isinstance(t, ast.Name)]
        if "_URL_DIR" in names:
            url_dir = ast.literal_eval(node.value)
        elif "_MODEL_HASHES" in names:
            hashes = ast.literal_eval(node.value)

    if not url_dir or not hashes:
        raise PrefetchError(
            "could not read _URL_DIR / _MODEL_HASHES from the installed gigaam "
            "— the pinned version may have moved them (RECOG_GIGAAM_REF)"
        )
    return url_dir, hashes


# ---------------------------------------------------------------------------
# Cache primitives
# ---------------------------------------------------------------------------


def file_md5(path: Path, block_size: int = BLOCK_SIZE) -> str:
    """MD5 of a file read in blocks — a checkpoint never lands in RAM whole."""
    digest = hashlib.md5()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(block_size), b""):
            digest.update(block)
    return digest.hexdigest()


def is_cached(path: Path, md5: Optional[str]) -> bool:
    """True when `path` can be reused as-is.

    Without a published checksum (the tokenizer) existence is all we can
    check — re-downloading a valid file every setup run would be worse.
    """
    if not path.is_file():
        return False
    if md5 is None:
        return True
    return file_md5(path) == md5


def expected_length(response) -> Optional[int]:
    """Content-Length of `response`, or None when the server did not send one.

    urllib never decompresses: it asks for `Accept-Encoding: identity` and
    hands the body over exactly as it came off the wire, so Content-Length
    always describes the bytes `read()` returns — including a compressed body.
    A server that ignores `identity` anyway is therefore not a reason to skip
    the truncation check; it is a reason to fail, because the bytes we would
    write are the compressed ones and nothing downstream decompresses them
    (for the tokenizer there is no checksum to catch that later).
    """
    headers = getattr(response, "headers", None)
    if headers is None:
        return None
    encoding = (headers.get("Content-Encoding") or "identity").strip().lower()
    if encoding not in ("identity", ""):
        raise PrefetchError(
            f"server answered with Content-Encoding: {encoding} — urllib does "
            f"not decompress, so the cached file would be unreadable"
        )
    raw = headers.get("Content-Length")
    if raw is None:
        return None
    try:
        return int(raw)
    except ValueError:
        return None


def download_to(url: str, dst: Path, md5: Optional[str],
                timeout: float = DEFAULT_TIMEOUT_S,
                block_size: int = BLOCK_SIZE) -> None:
    """Stream `url` into `dst`, verifying before publishing it atomically."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.md5()
    written = 0
    fd, tmp_name = tempfile.mkstemp(dir=str(dst.parent), prefix=f".{dst.name}.")
    tmp = Path(tmp_name)
    try:
        with os.fdopen(fd, "wb") as out:
            with urllib.request.urlopen(url, timeout=timeout) as source:
                declared = expected_length(source)
                while True:
                    block = source.read(block_size)
                    if not block:
                        break
                    digest.update(block)
                    written += len(block)
                    out.write(block)
        # A body cut short mid-stream is NOT an exception: http.client
        # deliberately returns b"" instead of raising IncompleteRead ("it
        # might break compatibility"), so a premature EOF is indistinguishable
        # from a finished download by the read loop alone. For the tokenizer
        # there is no published checksum, so the declared length is the only
        # thing standing between a truncated file and atomic publication —
        # after which `is_cached` accepts it by existence, forever.
        if declared is not None and written != declared:
            raise PrefetchError(
                f"truncated download for {url}: got {written} of {declared} bytes"
            )
        if md5 is not None and digest.hexdigest() != md5:
            raise PrefetchError(
                f"checksum mismatch for {url}: expected {md5}, "
                f"got {digest.hexdigest()}"
            )
        os.replace(tmp, dst)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise


def ensure_file(url: str, dst: Path, md5: Optional[str],
                timeout: float = DEFAULT_TIMEOUT_S) -> str:
    """Make `dst` a valid cached copy of `url`. Returns "cached"|"downloaded"."""
    if is_cached(dst, md5):
        return "cached"
    download_to(url, dst, md5, timeout=timeout)
    return "downloaded"


def needs_tokenizer(model_name: str) -> bool:
    """Mirrors gigaam._download_tokenizer: only e2e and v1_rnnt ship one."""
    return model_name == "v1_rnnt" or "e2e" in model_name


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def prefetch(model_name: str, cache_dir: Path, url_dir: str,
             hashes: Dict[str, str], timeout: float = DEFAULT_TIMEOUT_S) -> Dict[str, str]:
    """Bring the cache for one model up to date. Returns {filename: state}."""
    if model_name not in hashes:
        raise PrefetchError(
            f"unknown model {model_name!r}; the pinned GigaAM publishes: "
            f"{', '.join(sorted(hashes))}"
        )

    results = {}
    ckpt = f"{model_name}.ckpt"
    results[ckpt] = ensure_file(
        f"{url_dir}/{ckpt}", cache_dir / ckpt, hashes[model_name], timeout
    )

    if needs_tokenizer(model_name):
        tokenizer = f"{model_name}_tokenizer.model"
        results[tokenizer] = ensure_file(
            f"{url_dir}/{tokenizer}", cache_dir / tokenizer, None, timeout
        )
    return results


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--model", default=None,
        help="model name (default: recog.config.DEFAULT_MODEL)",
    )
    parser.add_argument(
        "--cache-dir", default=None, type=Path,
        help=f"download root (default: {DEFAULT_CACHE_DIR})",
    )
    parser.add_argument(
        "--url-dir", default=None,
        help="base URL of the checkpoints (default: the pinned GigaAM's)",
    )
    parser.add_argument(
        "--timeout", type=float, default=DEFAULT_TIMEOUT_S,
        help="per-read network timeout in seconds",
    )
    args = parser.parse_args(argv)

    model_name = args.model
    if model_name is None:
        from recog.config import DEFAULT_MODEL
        model_name = DEFAULT_MODEL
    cache_dir = args.cache_dir or DEFAULT_CACHE_DIR

    try:
        url_dir, hashes = parse_gigaam_constants(gigaam_init_path().read_text())
        if args.url_dir:
            url_dir = args.url_dir
        results = prefetch(model_name, cache_dir, url_dir, hashes, args.timeout)
    except PrefetchError as exc:
        print(f"[prefetch] {exc}", file=sys.stderr)
        return 1
    except Exception as exc:  # network, disk, malformed response
        print(f"[prefetch] {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1

    free = shutil.disk_usage(cache_dir).free // (1 << 20)
    for name, state in results.items():
        print(f"[prefetch] {name}: {state}")
    print(f"[prefetch] cache: {cache_dir} ({free} MiB free)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
