#!/bin/bash
#
# scripts/check.sh - the automatable part of the repo's verification.
#
# Runs on macOS and Linux against a plain checkout with the Python
# dependencies installed; .github/workflows/ci.yml runs exactly this.
# Deliberately out of scope: starting services, capturing audio, loading model
# weights — all of that needs a host with real audio devices and is part of the
# manual acceptance pass.
#
# RECOG_CHECK_PYTHON overrides the interpreter (default: the repo venv when it
# exists, else python3), the same way DEPS_PYTHON does for scripts/deps.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_DIR"

if [ -n "${RECOG_CHECK_PYTHON:-}" ]; then
    PYTHON="$RECOG_CHECK_PYTHON"
elif [ -x "$REPO_DIR/venv-gigaam/bin/python" ]; then
    PYTHON="$REPO_DIR/venv-gigaam/bin/python"
elif command -v python3 > /dev/null 2>&1; then
    PYTHON="$(command -v python3)"
else
    echo "check: no python3 found (set RECOG_CHECK_PYTHON)" >&2
    exit 1
fi

step() { printf '\n=== %s ===\n' "$1"; }

# Tracked plus not-yet-added scripts, minus the ignored ones: a checkout can
# carry ignored worktrees and vendored trees whose scripts are none of our
# business, but a brand-new script must be checked before it is committed.
list_shell_scripts() {
    if git rev-parse --is-inside-work-tree > /dev/null 2>&1; then
        git ls-files --cached --others --exclude-standard -z -- '*.sh'
    else
        find . -name '*.sh' \
            -not -path './venv*' \
            -not -path '*/node_modules/*' \
            -print0
    fi
}

echo "check: python is $PYTHON"

step "compileall"
"$PYTHON" -m compileall -q recog scripts

step "shell syntax"
while IFS= read -r -d '' script; do
    echo "  bash -n $script"
    bash -n "$script"
done < <(list_shell_scripts)

step "CLI help"
"$PYTHON" -m recog --help > /dev/null

step "service units render"
RENDER_DIR="$(mktemp -d)"
trap 'rm -rf "$RENDER_DIR"' EXIT
./recog-service.sh render --dry-run "$RENDER_DIR" > /dev/null
echo "  rendered $(find "$RENDER_DIR" -type f | wc -l | tr -d ' ') $(uname -s) units"

if [ -n "${RECOG_QUALITY_BASE:-}" ]; then
    step "quality gates (baseline and current pytest with branch coverage)"
    "$PYTHON" scripts/quality.py --base "$RECOG_QUALITY_BASE" \
        --output "${RECOG_QUALITY_OUTPUT:-.local/quality}"
else
    step "pytest"
    "$PYTHON" -m pytest -q -m "not e2e"
fi

printf '\ncheck: OK\n'
