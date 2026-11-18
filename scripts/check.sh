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

# A quality tool from the interpreter's environment (the pins in
# deploy/python/requirements-quality.txt install next to it), else from PATH.
quality_tool() {
    local candidate
    candidate="$(dirname "$PYTHON")/$1"
    if [ -x "$candidate" ]; then
        echo "$candidate"
    elif command -v "$1"; then
        :
    else
        echo "check: $1 not found ($2)" >&2
        return 1
    fi
}

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

step "shellcheck"
SHELLCHECK="$(quality_tool shellcheck "install deploy/python/requirements-quality.txt")"
# -x with SCRIPTDIR follows the `source "$SCRIPT_DIR/…"` of each script.
list_shell_scripts | xargs -0 "$SHELLCHECK" -x --source-path=SCRIPTDIR

if [ -d .github/workflows ]; then
    step "workflow lint"
    ACTIONLINT="$(quality_tool actionlint "install deploy/python/requirements-quality.txt")"
    # Without -shellcheck actionlint skips the run: scripts when shellcheck is
    # not on PATH, and the venv is not.
    "$ACTIONLINT" -shellcheck "$SHELLCHECK"
fi

# Committed history plus the files a commit would take (tracked and new, not
# ignored), copied out so that venvs and node_modules are not scanned. The
# config path is relative: the copy carries the working tree's config too.
step "secrets"
GITLEAKS="$(quality_tool gitleaks "install gitleaks 8.30.1, e.g. brew install gitleaks")"
GITLEAKS_ARGS=(--no-banner --redact --config scripts/quality-gitleaks.toml)
if git rev-parse --is-inside-work-tree > /dev/null 2>&1; then
    "$GITLEAKS" git "${GITLEAKS_ARGS[@]}" .
    TREE_COPY="$(mktemp -d)"
    trap 'rm -rf "$TREE_COPY"' EXIT
    git ls-files --cached --others --exclude-standard -z \
        | while IFS= read -r -d '' name; do
            if [ -f "$name" ]; then printf '%s\0' "$name"; fi
        done \
        | tar --null -T - -cf - | tar -xf - -C "$TREE_COPY"
    (cd "$TREE_COPY" && "$GITLEAKS" dir "${GITLEAKS_ARGS[@]}" .)
    rm -rf "$TREE_COPY"
else
    "$GITLEAKS" dir "${GITLEAKS_ARGS[@]}" .
fi

# The Rust workspace (Cargo.toml): format, lint, the tests that need no data.
# The parity tests against pyaec need the port's golden data, named by
# SPEEXDSP_AEC_GOLDEN (crates/speexdsp-aec/README.md).
if [ -f Cargo.toml ]; then
    step "rust"
    if ! command -v cargo > /dev/null 2>&1; then
        echo "check: cargo not found (install Rust with rustup, https://rustup.rs)" >&2
        exit 1
    fi
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test -p speexdsp-aec --lib --test properties --test cli --test adapted_golden
    cargo test -p speexdsp-aec --doc
    cargo test -p recog-mic
    cargo test -p recog-copilot
    cargo test -p recog-zoom
    cargo test -p recog-core
    cargo test -p recog-install
    cargo test -p recog-ai
    cargo test -p recog-live
    cargo test -p recog-bridge
    cargo test -p recog-web
    cargo test -p recog-desktop
    cargo test -p recog-asr-server
    if [ -n "${SPEEXDSP_AEC_GOLDEN:-}" ]; then
        cargo test -p speexdsp-aec --test parity --test fft_parity
    fi
fi

step "CLI help"
if [ -f Cargo.toml ]; then
    cargo run -q -p recog-live --bin recog -- --help > /dev/null
fi

step "service units render"
RENDER_DIR="$(mktemp -d)"
trap 'rm -rf "$RENDER_DIR"' EXIT
# Rendering asks recog for the data directory: the installed one, else the debug
# build the Rust tests above made (a checkout setup.sh has not installed, CI).
if [ -x bin/recog ] || [ ! -x "${CARGO_TARGET_DIR:-target}/debug/recog" ]; then
    ./recog-service.sh render --dry-run "$RENDER_DIR" > /dev/null
else
    DEPS_BIN_DIR="$(cd "${CARGO_TARGET_DIR:-target}/debug" && pwd)" \
        ./recog-service.sh render --dry-run "$RENDER_DIR" > /dev/null
fi
echo "  rendered $(find "$RENDER_DIR" -type f | wc -l | tr -d ' ') $(uname -s) units"

# RECOG_QUALITY_BASE runs the full quality gate (docs/quality.md), which
# replaces plain pytest; CI sets it. Otherwise only the gate's fast Ruff check
# of changed lines runs, against the local origin/main as last fetched (a stale
# ref only widens the compared range), and is skipped without that ref.
if [ -n "${RECOG_QUALITY_BASE:-}" ]; then
    step "quality gates (baseline and current coverage of Rust and JavaScript, then pytest)"
    "$PYTHON" scripts/quality.py --base "$RECOG_QUALITY_BASE" \
        --output "${RECOG_QUALITY_OUTPUT:-.local/quality}"
else
    if git rev-parse -q --verify 'origin/main^{commit}' > /dev/null 2>&1; then
        step "ruff on changed lines"
        "$PYTHON" scripts/quality.py --style-only --base origin/main \
            --output "${RECOG_QUALITY_OUTPUT:-.local/quality}"
    fi
    step "pytest"
    "$PYTHON" -m pytest -q -m "not e2e"
fi

printf '\ncheck: OK\n'
