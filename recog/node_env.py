"""
Node/nvm environment utilities for subprocess calls.

Under launchd, PATH is minimal and doesn't include nvm-managed node.
These utilities find the node binary and construct a proper environment.
"""

import os
import re
import shutil
from pathlib import Path


def version_key(version_dir: Path) -> tuple:
    """Extract (major, minor, patch) from nvm version directory name for correct sorting."""
    match = re.match(r"v?(\d+)\.(\d+)\.(\d+)", version_dir.name)
    if match:
        return (int(match.group(1)), int(match.group(2)), int(match.group(3)))
    return (0, 0, 0)


def get_node_env() -> dict:
    """Get environment with node/nvm paths for subprocess.

    Under launchd PATH is minimal and doesn't include nvm-managed node.
    This finds the nvm node binary and prepends it to PATH.
    """
    env = os.environ.copy()

    # If node already in PATH, use current env
    if shutil.which("node"):
        return env

    # Find nvm node directory and add to PATH
    nvm_locations = [
        Path.home() / ".local/share/nvm",
        Path.home() / ".nvm/versions/node",
    ]
    for nvm_base in nvm_locations:
        if nvm_base.exists():
            for version_dir in sorted(nvm_base.iterdir(), key=version_key, reverse=True):
                node_bin = version_dir / "bin"
                if (node_bin / "node").exists():
                    env["PATH"] = f"{node_bin}:{env.get('PATH', '')}"
                    return env
    return env
