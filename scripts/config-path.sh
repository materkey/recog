#!/bin/bash
# Print the installation config path the programs use, as the recog binary
# resolves it.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/deps.sh"
deps_recog config config-path
