#!/bin/bash
# Print the effective data root the programs use, as the recog binary resolves
# it; never source .env.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/deps.sh"
deps_recog config data-dir
