#!/bin/bash
# Print the same effective data root Python consumers use; never source .env.
# Without Python ([install] runtime = "rust") the recog binary resolves it.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/deps.sh"
if deps_rust_mode; then
    deps_recog config data-dir
    exit
fi
"$(deps_python)" -c 'import sys; sys.path.insert(0, sys.argv[1]); from recog.config import get_data_dir; print(get_data_dir())' "$DEPS_REPO_DIR"
