#!/bin/bash
# Print the installation config path the services use (recog.install_config).
# Without Python ([install] runtime = "rust") the recog binary resolves it.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/deps.sh"
if deps_rust_mode; then
    deps_recog config config-path
    exit
fi
"$(deps_python)" -c 'import sys; sys.path.insert(0, sys.argv[1]); from recog.install_config import config_path; print(config_path())' "$DEPS_REPO_DIR"
