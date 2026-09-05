#!/bin/bash
# Print the same effective data root Python consumers use; never source .env.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/deps.sh"
"$(deps_python)" -c 'import sys; sys.path.insert(0, sys.argv[1]); from recog.config import get_data_dir; print(get_data_dir())' "$DEPS_REPO_DIR"
