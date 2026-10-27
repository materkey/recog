#!/bin/bash
# Print the installation config path the services use (recog.install_config).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/deps.sh"
"$(deps_python)" -c 'import sys; sys.path.insert(0, sys.argv[1]); from recog.install_config import config_path; print(config_path())' "$DEPS_REPO_DIR"
