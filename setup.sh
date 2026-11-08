#!/usr/bin/env bash
#
# setup.sh - install (or update) the whole recog pipeline with one command.
#
#   git clone <stash-url>/recog.git && cd recog
#   ./setup.sh
#
# Works on macOS (launchd, ScreenCaptureKit) and Linux (systemd --user,
# PulseAudio/PipeWire). Idempotent: re-running it is how you update after a
# `git pull` — the venv and the weight cache are reused, previously running
# services are explicitly restarted so they pick up new code and units.
#
# Flags:
#   --dry-run        show the installation plan without changing anything
#   --no-services    prepare everything, do not touch launchd / systemd
#   --no-prefetch    skip the GigaAM weight-cache preparation
#   --no-extension   skip the Chrome extension build
#   --no-env         do not create environment files from examples
#
# Environment files are created only on a first install. Once the service
# units exist, a missing .env is left missing: the examples are not neutral
# (they switch meeting auto-start off and set models).
#
# Environment (read from the calling shell, NOT from .env — .env is created by
# this script, later than these are needed):
#   RECOG_TORCH_INDEX_URL   pip index for torch/torchaudio. Unset on Linux
#                           without an NVIDIA GPU defaults to the CPU index
#                           (plain PyPI torch drags in ~2-3 GB of CUDA
#                           runtime). Set it empty to force plain PyPI.
#   RECOG_GIGAAM_URL/REF    git source of GigaAM (defaults: upstream @ pin)
#   RECOG_GIGAAM_EDITABLE   path to a local GigaAM checkout to install -e
#   RECOG_HOST_OS           override the detected OS (tests)
#   RECOG_HOST_ARCH         override the detected CPU architecture (tests)
#   RECOG_SERVICE_OS        override the service backend (tests)
#   RECOG_SKIP_COPILOT_CHECK=1 skip tmux/claude checks when Copilot is not needed

# No `set -u`: empty-array expansions ("${PIP_CONSTRAINTS[@]}") are an
# unbound-variable error under bash 3.2, which is what /bin/bash still is on
# macOS.
set -eo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_DIR"

DEPS_REPO_DIR="$REPO_DIR"
source "$REPO_DIR/scripts/deps.sh"

GIGAAM_URL="${RECOG_GIGAAM_URL:-https://github.com/salute-developers/GigaAM.git}"
# Pinned: upstream has moved past this commit and recog calls the decoder /
# word-timestamp API directly (recog/asr.py, recog/refine.py).
GIGAAM_REF="${RECOG_GIGAAM_REF:-6e4b027}"
CPU_TORCH_INDEX="https://download.pytorch.org/whl/cpu"

VENV_DIR="$REPO_DIR/venv-gigaam"
HOST_OS="$(deps_host_os)"

DO_SERVICES=1
DO_PREFETCH=1
DO_EXTENSION=1
DO_ENV=1
DRY_RUN=0

# Collected as we go, reported in the summary.
MISSING_REQUIRED=()
WARNINGS=()

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

step() { echo ""; echo -e "${CYAN}=== $1 ===${NC}"; }
info() { echo -e "${GREEN}[ok]${NC} $1"; }
warn() { echo -e "${YELLOW}[warn]${NC} $1"; WARNINGS+=("$1"); }
fail() { echo -e "${RED}[error]${NC} $1" >&2; exit 1; }

usage() {
    sed -n '2,33p' "$REPO_DIR/setup.sh" | sed 's/^#\{0,1\} \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)      DRY_RUN=1 ;;
        --no-services)  DO_SERVICES=0 ;;
        --no-prefetch)  DO_PREFETCH=0 ;;
        --no-extension) DO_EXTENSION=0 ;;
        --no-env)       DO_ENV=0 ;;
        -h|--help)      usage; exit 0 ;;
        *)              echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

case "$HOST_OS" in
    darwin|linux) ;;
    *) fail "Unsupported OS: $HOST_OS (recog runs on macOS and Linux)" ;;
esac

# Upstream torch publishes no macOS x86_64 wheels past 2.2.2, and
# deploy/python/constraints/macos.txt pins the verified 2.8.0 — pip on an Intel Mac cannot
# resolve the install at all. Say so in the first second instead of after a
# venv and a failed resolve.
HOST_ARCH="${RECOG_HOST_ARCH:-$(uname -m)}"
if [ "$HOST_OS" = "darwin" ] && [ "$HOST_ARCH" != "arm64" ]; then
    fail "macOS on $HOST_ARCH is not supported: the pinned torch/torchaudio (deploy/python/constraints/macos.txt) have macOS wheels for Apple Silicon only. Use an Apple Silicon Mac or Linux. (A Rosetta shell on Apple Silicon also reports x86_64 — rerun in a native arm64 terminal.)"
fi

# The service backend follows the host unless a test pins it.
if [ -z "${RECOG_SERVICE_OS:-}" ]; then
    case "$HOST_OS" in
        darwin) export RECOG_SERVICE_OS="Darwin" ;;
        linux)  export RECOG_SERVICE_OS="Linux" ;;
    esac
fi

# Service units already on disk mean this run updates a working installation.
# A missing environment file is then part of its configuration (settings may
# live in unit drop-ins), so the examples must not be copied in.
services_already_installed() {
    (
        SVC_REPO_DIR="$REPO_DIR"
        SVC_DATA_DIR="$REPO_DIR" # only unit paths are read here
        source "$REPO_DIR/scripts/service/common.sh"
        case "$RECOG_SERVICE_OS" in
            Darwin) source "$REPO_DIR/scripts/service/launchd.sh" ;;
            Linux)  source "$REPO_DIR/scripts/service/systemd.sh" ;;
            *)      exit 1 ;;
        esac
        [ -n "$(svc_installed_services)" ]
    )
}

echo "=== recog setup ($HOST_OS) ==="

if [ "$DRY_RUN" -eq 1 ]; then
    echo "Dry run — installation plan only; dependencies and readiness are not verified."
    echo "Would check Python >= 3.12, system dependencies and ASR configuration."
    if [ -x "$VENV_DIR/bin/python" ]; then
        echo "Would reuse venv: $VENV_DIR"
    elif [ -e "$VENV_DIR" ]; then
        fail "$VENV_DIR exists but has no bin/python — remove it and rerun ./setup.sh"
    else
        echo "Would create venv: $VENV_DIR"
    fi
    torch_index="${RECOG_TORCH_INDEX_URL:-default PyPI index}"
    if [ "${RECOG_TORCH_INDEX_URL+set}" != "set" ] && [ "$HOST_OS" = "linux" ] && ! deps_have nvidia-smi; then
        torch_index="$CPU_TORCH_INDEX"
    fi
    echo "Would install torch / torchaudio from $torch_index"
    echo "Would install GigaAM from ${RECOG_GIGAAM_EDITABLE:-${GIGAAM_URL}@${GIGAAM_REF}}"
    echo "Would install deploy/python/requirements.txt and deploy/python/requirements-web.txt"
    if [ "$HOST_OS" = "darwin" ]; then
        echo "Would also install deploy/python/requirements-macos.txt; all pip steps use deploy/python/constraints/macos.txt"
        echo "Would build the echo canceller from rust/speexdsp-aec-py with pip and cargo, if cargo is available."
        echo "Would build the Rust microphone worker rust/recog-mic with cargo, if cargo is available."
        echo "Would build the Rust live process rust/recog-live with cargo, if cargo is available."
        echo "Would build the Rust web server rust/recog-web with cargo, if cargo is available."
        echo "Would build the Rust Copilot supervisor and channel rust/recog-copilot with cargo, if cargo is available."
    else
        echo "Would build the Rust live process rust/recog-live with cargo, if cargo is available and [live] runtime is rust."
        echo "Would build the Rust web server rust/recog-web with cargo, if cargo is available and [web] runtime is rust."
        echo "Would build the Rust Copilot supervisor and channel rust/recog-copilot with cargo, if cargo is available and [copilot] runtime is rust."
    fi
    echo "Would build the ASR backend wavo from rust/wavo-asr-py with pip and cargo, if [asr] backend is wavo, or auto on macOS."
    if [ "$DO_ENV" -eq 1 ]; then
        for config in "$REPO_DIR/.env" "$REPO_DIR/recog/web/.env"; do
            if [ -f "$config" ]; then
                echo "Would keep existing $config"
            elif services_already_installed; then
                echo "Would leave $config absent — services are already installed"
            else
                echo "Would copy $config.example to $config if the example exists"
            fi
        done
    else
        echo "Skipped environment files (--no-env)"
    fi
    echo "Would resolve the data directory and create it if missing (existing data is not migrated)."
    if [ "$DO_EXTENSION" -eq 1 ]; then
        echo "Would install npm dependencies and build browser-extension/dist if npm and the extension directory are available."
    else
        echo "Skipped Chrome extension (--no-extension)"
    fi
    if [ "$HOST_OS" = "darwin" ]; then
        echo "Would build sck_audio_capture with swiftc, or reuse the existing helper."
        echo "Would build Recog.app with swiftc when its source changed, or reuse the existing one."
    fi
    if [ "$DO_PREFETCH" -eq 1 ]; then
        echo "Would prepare the GigaAM weight cache, downloading missing or corrupt files."
    else
        echo "Skipped GigaAM weights (--no-prefetch)"
    fi
    if [ "$DO_SERVICES" -eq 1 ]; then
        echo "Would install $RECOG_SERVICE_OS service units, check readiness, then restart running services and start the others."
    else
        echo "Skipped services (--no-services)"
    fi
    echo "Dry run complete — nothing installed, written, downloaded or restarted."
    exit 0
fi

# ---------------------------------------------------------------------------
# 1. Python interpreter
# ---------------------------------------------------------------------------

step "Python"

python_minor() {
    "$1" -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>/dev/null
}

PYTHON_BIN=""
PYTHON_VERSION=""
for candidate in python3.12 python3.13 python3; do
    deps_have "$candidate" || continue
    version="$(python_minor "$candidate")" || continue
    [ -n "$version" ] || continue
    major="${version%%.*}"
    minor="${version##*.}"
    [ "$major" = "3" ] || continue
    [ "$minor" -ge 12 ] 2>/dev/null || continue
    PYTHON_BIN="$(command -v "$candidate")"
    PYTHON_VERSION="$version"
    break
done

if [ -z "$PYTHON_BIN" ]; then
    echo -e "${RED}[error]${NC} No Python >= 3.12 found (tried python3.12, python3.13, python3)." >&2
    echo "  Install it: $(deps_hint python)" >&2
    echo "  Distro-independent alternative: uv python install 3.12" >&2
    exit 1
fi

info "Python $PYTHON_VERSION at $PYTHON_BIN"
if [ "${PYTHON_VERSION##*.}" -gt 13 ]; then
    warn "Python $PYTHON_VERSION is newer than 3.13 — torch/GigaAM wheels may not exist for it yet. If pip fails: uv python install 3.12 (or python312 from AUR / deadsnakes on Ubuntu)."
fi

# Library probes run on this interpreter until the venv exists. Deliberately
# not RECOG_PYTHON — that one selects the interpreter baked into the units.
DEPS_PYTHON="$PYTHON_BIN"

# ---------------------------------------------------------------------------
# 2. System dependencies
# ---------------------------------------------------------------------------

step "System dependencies"

report_dep() {
    # report_dep <name> <found:0|1> <kind: required|optional> <note>
    local name="$1" found="$2" kind="$3" note="${4:-}"
    if [ "$found" = "1" ]; then
        printf "  ${GREEN}%-9s${NC} %-10s %s\n" "[found]" "$name" "$note"
    elif [ "$kind" = "required" ]; then
        printf "  ${RED}%-9s${NC} %-10s %s\n" "[MISSING]" "$name" "$(deps_hint "$name")"
        MISSING_REQUIRED+=("$name")
    else
        printf "  ${YELLOW}%-9s${NC} %-10s %s\n" "[absent]" "$name" "$(deps_hint "$name") — $note"
    fi
}

check_cmd() {
    # check_cmd <name> <required|optional> <note>
    if deps_have "$1"; then
        report_dep "$1" 1 "$2" "$(command -v "$1")"
    else
        report_dep "$1" 0 "$2" "${3:-}"
    fi
}

check_cmd ffmpeg required "refine timestamps + segment playback"
if [ "${RECOG_SKIP_COPILOT_CHECK:-0}" = "1" ]; then
    info "Copilot CLI checks skipped (RECOG_SKIP_COPILOT_CHECK=1)"
else
    check_cmd tmux required "Copilot sessions"
    check_cmd claude required "Copilot sessions (sign in with claude before first use)"
fi
if [ "$HOST_OS" = "linux" ]; then
    check_cmd parec required "system audio capture"
    check_cmd pactl required "default-sink discovery"
    echo "  [later]   portaudio  microphone input — probed after the Python deps are installed"
elif [ "$HOST_OS" = "darwin" ]; then
    if deps_have swiftc; then
        report_dep swiftc 1 required "$(command -v swiftc)"
    elif deps_sck_helper_ok && deps_launcher_ok; then
        report_dep swiftc 1 optional "not needed — sck_audio_capture and Recog.app are already built"
    else
        report_dep swiftc 0 required "needed to build sck_audio_capture and Recog.app"
    fi
    check_cmd cargo optional "echo cancellation ([live] echo_cancel) needs the Rust echo canceller it builds"
fi

while IFS=$'\t' read -r name note; do
    [ -n "$name" ] || continue
    check_cmd "$name" optional "$note"
done <<< "$(deps_optional_list)"

# ---------------------------------------------------------------------------
# 3. ASR configuration (before anything is installed or restarted)
# ---------------------------------------------------------------------------

step "ASR configuration"

if asr_problem="$(deps_asr_config_check)"; then
    info "ASR config OK"
else
    echo -e "${RED}[error]${NC} $asr_problem" >&2
    echo "  Nothing was installed or restarted. Fix .env and rerun ./setup.sh" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 4. Virtualenv
# ---------------------------------------------------------------------------

step "Virtualenv"

if [ -x "$VENV_DIR/bin/python" ]; then
    # Reuse whatever is there, symlink included: on the reference Mac
    # venv-gigaam is a symlink into the GigaAM checkout, and recreating it
    # would silently swap the interpreter and every installed version.
    target="$VENV_DIR"
    if [ -L "$VENV_DIR" ]; then
        target="$VENV_DIR -> $(readlink "$VENV_DIR")"
    fi
    info "Reusing venv: $target (Python $(python_minor "$VENV_DIR/bin/python"))"
elif [ -e "$VENV_DIR" ]; then
    fail "$VENV_DIR exists but has no bin/python — remove it and rerun ./setup.sh"
else
    echo "Creating $VENV_DIR ..."
    "$PYTHON_BIN" -m venv "$VENV_DIR"
    info "Created venv-gigaam"
fi

VENV_PY="$VENV_DIR/bin/python"
VENV_PIP="$VENV_DIR/bin/pip"
DEPS_PYTHON="$VENV_PY"

# Every pip stage runs under the macOS constraints file so a Linux port cannot
# drag the working Mac's ML stack to new versions as a side effect.
PIP_CONSTRAINTS=()
if [ "$HOST_OS" = "darwin" ] && [ -f "$REPO_DIR/deploy/python/constraints/macos.txt" ]; then
    PIP_CONSTRAINTS=(-c "$REPO_DIR/deploy/python/constraints/macos.txt")
fi

pip_install() {
    "$VENV_PIP" install "${PIP_CONSTRAINTS[@]}" "$@"
}

# ---------------------------------------------------------------------------
# 5. torch / torchaudio
# ---------------------------------------------------------------------------

step "torch / torchaudio"

TORCH_INDEX_ARGS=()
if [ "${RECOG_TORCH_INDEX_URL+set}" = "set" ]; then
    if [ -n "$RECOG_TORCH_INDEX_URL" ]; then
        TORCH_INDEX_ARGS=(--index-url "$RECOG_TORCH_INDEX_URL")
        echo "Using RECOG_TORCH_INDEX_URL=$RECOG_TORCH_INDEX_URL"
    else
        echo "RECOG_TORCH_INDEX_URL is empty — using the default PyPI index"
    fi
elif [ "$HOST_OS" = "linux" ] && ! deps_have nvidia-smi; then
    # Default PyPI torch on Linux pulls the whole CUDA runtime (~2-3 GB) that
    # a GPU-less box has no use for. Override in either direction with
    # RECOG_TORCH_INDEX_URL.
    TORCH_INDEX_ARGS=(--index-url "$CPU_TORCH_INDEX")
    echo "No nvidia-smi found — installing CPU-only torch from $CPU_TORCH_INDEX"
fi

pip_install "${TORCH_INDEX_ARGS[@]}" torch torchaudio
info "torch / torchaudio installed"

# ---------------------------------------------------------------------------
# 6. GigaAM
# ---------------------------------------------------------------------------

step "GigaAM"

# No [torch] extras on either path: torch is already installed above, and the
# extra would re-resolve it.
if [ -n "${RECOG_GIGAAM_EDITABLE:-}" ]; then
    editable_dir="$RECOG_GIGAAM_EDITABLE"
    [ -d "$editable_dir" ] || fail "RECOG_GIGAAM_EDITABLE=$editable_dir is not a directory"
    [ -f "$editable_dir/pyproject.toml" ] || fail "RECOG_GIGAAM_EDITABLE=$editable_dir has no pyproject.toml"

    pip_install -e "$editable_dir"
    info "GigaAM installed editable from $editable_dir"

    if git -C "$editable_dir" rev-parse --git-dir > /dev/null 2>&1; then
        rev="$(git -C "$editable_dir" rev-parse --short HEAD)"
        echo "  revision: $rev (pin is $GIGAAM_REF)"
        if [ "${rev#"$GIGAAM_REF"}" = "$rev" ] && [ "${GIGAAM_REF#"$rev"}" = "$GIGAAM_REF" ]; then
            warn "GigaAM checkout is at $rev, not the pinned $GIGAAM_REF — your ASR differs from everyone else's"
        fi
        if ! git -C "$editable_dir" diff --quiet HEAD 2>/dev/null; then
            warn "GigaAM checkout has uncommitted changes to tracked files — they are part of your ASR"
        fi
    fi
else
    pip_install "gigaam @ git+${GIGAAM_URL}@${GIGAAM_REF}"
    info "GigaAM installed from ${GIGAAM_URL}@${GIGAAM_REF}"
fi

# ---------------------------------------------------------------------------
# 7. Python requirements
# ---------------------------------------------------------------------------

step "Python requirements"

REQ_ARGS=(-r "$REPO_DIR/deploy/python/requirements.txt" -r "$REPO_DIR/deploy/python/requirements-web.txt")
if [ "$HOST_OS" = "darwin" ]; then
    REQ_ARGS+=(-r "$REPO_DIR/deploy/python/requirements-macos.txt")
fi
pip_install "${REQ_ARGS[@]}"
info "Requirements installed"

# Echo cancellation ([live] echo_cancel, macOS, off by default) runs the Rust
# port of speexdsp's canceller, a Python module built here from rust/. Without
# it the stage is not built and the microphone stays raw, so a missing cargo
# or a failed build does not stop the installation.
if [ "$HOST_OS" = "darwin" ]; then
    if ! deps_have cargo; then
        echo -e "  ${YELLOW}[absent]${NC} cargo — echo cancellation unavailable: $(deps_hint cargo), then rerun ./setup.sh"
    elif pip_install "$REPO_DIR/rust/speexdsp-aec-py"; then
        info "Echo canceller built (rust/speexdsp-aec-py)"
    else
        warn "The echo canceller was not built (rust/speexdsp-aec-py); echo cancellation stays unavailable"
    fi
    # The microphone worker in Rust ([live] mic_worker = "rust", not the default);
    # without it that setting falls back to the Python worker. Built from rust/ so
    # cargo reads rust/.cargo/config.toml like the other builds there.
    if deps_have cargo; then
        if (cd "$REPO_DIR/rust" && cargo build --release -p recog-mic); then
            info "Rust microphone worker built (rust/recog-mic)"
        else
            warn "The Rust microphone worker was not built (rust/recog-mic); the microphone stays on the Python worker"
        fi
    fi
fi

# The live process in Rust ([live] runtime = "rust", not the default); without the
# binary that setting falls back to the Python live process. Built on a Mac whenever
# cargo is present, like the microphone worker; on Linux only with the setting, since
# its first release build takes minutes.
if deps_have cargo && { [ "$HOST_OS" = "darwin" ] || [ "$(deps_live_runtime)" = "rust" ]; }; then
    if (cd "$REPO_DIR/rust" && cargo build --release -p recog-live --bin recog); then
        info "Rust live process built (rust/recog-live)"
    else
        warn "The Rust live process was not built (rust/recog-live); the live service stays on Python"
    fi
fi

# The web server in Rust ([web] runtime = "rust", not the default), built like the live
# process: on a Mac whenever cargo is present, on Linux only with the setting. Without
# the binary the web service stays on Python and the log says so.
if deps_have cargo && { [ "$HOST_OS" = "darwin" ] || [ "$(deps_web_runtime)" = "rust" ]; }; then
    if (cd "$REPO_DIR/rust" && cargo build --release -p recog-web --bin recog-web); then
        info "Rust web server built (rust/recog-web)"
    else
        warn "The Rust web server was not built (rust/recog-web); the web service stays on Python"
    fi
fi

# The Copilot supervisor and MCP channel in Rust ([copilot] runtime = "rust", not the
# default), built the same way; without the binaries both services stay on Python.
if deps_have cargo && { [ "$HOST_OS" = "darwin" ] || [ "$(deps_copilot_runtime)" = "rust" ]; }; then
    if (cd "$REPO_DIR/rust" && cargo build --release -p recog-copilot --bin recog-copilot-supervisor --bin recog-copilot-channel); then
        info "Rust Copilot supervisor and channel built (rust/recog-copilot)"
    else
        warn "The Rust Copilot supervisor and channel were not built (rust/recog-copilot); the Copilot services stay on Python"
    fi
fi

# The ASR backend "wavo" runs GigaAM on wavo, a Rust runtime, from a Python module
# built here from rust/. It is built when that backend is chosen, and on a Mac for the
# default "auto", which runs on it there; elsewhere not, since its release build takes
# minutes. Chosen explicitly and missing, the readiness check refuses to start the
# services and says why; under "auto" ASR stays on PyTorch.
ASR_BACKEND=$(deps_asr_backend)
if [ "$ASR_BACKEND" = "wavo" ] || { [ "$ASR_BACKEND" = "auto" ] && [ "$HOST_OS" = "darwin" ]; }; then
    if ! deps_have cargo && [ "$ASR_BACKEND" = "auto" ]; then
        echo -e "  ${YELLOW}[absent]${NC} cargo — ASR stays on PyTorch; for the faster wavo: $(deps_hint cargo), then rerun ./setup.sh"
    elif ! deps_have cargo; then
        echo -e "  ${YELLOW}[absent]${NC} cargo — the ASR backend wavo cannot be built: $(deps_hint cargo), then rerun ./setup.sh"
    elif pip_install "$REPO_DIR/rust/wavo-asr-py"; then
        info "ASR backend wavo built (rust/wavo-asr-py)"
    else
        warn "The ASR backend wavo was not built (rust/wavo-asr-py)"
    fi
fi

if [ "$HOST_OS" = "linux" ]; then
    if deps_portaudio_ok; then
        info "PortAudio loads (microphone input available)"
    else
        echo -e "  ${RED}[MISSING]${NC} portaudio  $(deps_hint portaudio)"
        MISSING_REQUIRED+=("portaudio")
    fi
fi

# ---------------------------------------------------------------------------
# 8. Configuration files
# ---------------------------------------------------------------------------

step "Configuration files"

copy_example() {
    local dst="$1" src="$2"
    if [ -f "$dst" ]; then
        info "$dst exists — left untouched"
    elif services_already_installed; then
        info "$dst absent — not created, services are already installed (copy $(basename "$src") yourself if you want it)"
    elif [ -f "$src" ]; then
        cp "$src" "$dst"
        info "Created $dst from $(basename "$src")"
    else
        warn "$src not found — $dst not created"
    fi
}

if [ "$DO_ENV" -eq 1 ]; then
    copy_example "$REPO_DIR/.env" "$REPO_DIR/.env.example"
    copy_example "$REPO_DIR/recog/web/.env" "$REPO_DIR/recog/web/.env.example"
else
    echo "Skipped environment files (--no-env)"
fi
DATA_DIR="$(DEPS_PYTHON="$VENV_PY" bash "$REPO_DIR/scripts/data-dir.sh")"
mkdir -p "$DATA_DIR"

# ---------------------------------------------------------------------------
# 9. Chrome extension
# ---------------------------------------------------------------------------

EXTENSION_DIR="$REPO_DIR/browser-extension"

step "Chrome extension"

if [ "$DO_EXTENSION" = "0" ]; then
    echo "Skipped (--no-extension)"
elif [ ! -d "$EXTENSION_DIR" ]; then
    warn "Extension directory not found: $EXTENSION_DIR"
elif ! deps_have npm; then
    warn "npm not found — build the extension manually later: cd $EXTENSION_DIR && npm ci && npm run build"
else
    npm_cmd="install"
    [ -f "$EXTENSION_DIR/package-lock.json" ] && npm_cmd="ci"
    if (cd "$EXTENSION_DIR" && npm "$npm_cmd" && npm run build); then
        info "Extension built: $EXTENSION_DIR"
    else
        # A broken extension build is not worth aborting the whole install: the
        # pipeline runs without it, only speaker names from conferences stop.
        warn "Extension build failed — retry manually: cd $EXTENSION_DIR && npm $npm_cmd && npm run build"
    fi
fi

# ---------------------------------------------------------------------------
# 10. macOS system-audio helper
# ---------------------------------------------------------------------------

if [ "$HOST_OS" = "darwin" ]; then
    step "System audio helper (ScreenCaptureKit)"

    if deps_have swiftc && swiftc -o "$REPO_DIR/sck_audio_capture" \
            "$REPO_DIR/recog/native/macos/sck_audio_capture.swift" \
            -framework ScreenCaptureKit -framework CoreMedia -framework AVFoundation; then
        info "Built sck_audio_capture"
    elif deps_sck_helper_ok; then
        info "sck_audio_capture is already built — reusing it"
    else
        echo -e "  ${RED}[MISSING]${NC} sck_audio_capture — no swiftc to build it ($(deps_hint swiftc))"
        MISSING_REQUIRED+=("sck_audio_capture")
    fi

    # Rebuilt only when its source changed: a new signature means macOS asks
    # for Screen Recording and the microphone again.
    if "$VENV_PY" -m recog.macos_launcher > /dev/null; then
        info "Recog.app is ready"
    elif deps_launcher_ok; then
        info "Recog.app is already built — reusing it"
    else
        echo -e "  ${RED}[MISSING]${NC} Recog.app — could not build it ($(deps_hint swiftc))"
        MISSING_REQUIRED+=("Recog.app")
    fi
fi

# ---------------------------------------------------------------------------
# 11. GigaAM weight cache
# ---------------------------------------------------------------------------

step "GigaAM weights"

if [ "$DO_PREFETCH" = "0" ]; then
    echo "Skipped (--no-prefetch)"
elif "$VENV_PY" "$REPO_DIR/scripts/prefetch_gigaam.py"; then
    info "Weight cache ready"
else
    warn "Could not prepare the GigaAM weight cache — the existing cache is unchanged; the first transcription will download what is missing"
fi

# ---------------------------------------------------------------------------
# 12. Services
# ---------------------------------------------------------------------------

SETUP_STATE="prepared"

if [ "$DO_SERVICES" = "0" ]; then
    step "Services"
    echo "Skipped (--no-services)"
else
    step "Services"

    SVC_REPO_DIR="$REPO_DIR"
    SVC_DATA_DIR="$DATA_DIR"
    source "$REPO_DIR/scripts/service/common.sh"
    case "$RECOG_SERVICE_OS" in
        Darwin) source "$REPO_DIR/scripts/service/launchd.sh" ;;
        Linux)  source "$REPO_DIR/scripts/service/systemd.sh" ;;
        *)      fail "Unsupported RECOG_SERVICE_OS: $RECOG_SERVICE_OS" ;;
    esac

    # Snapshot before install: these are the ones that must be *restarted*
    # afterwards, so they pick up the new code and the re-rendered unit.
    RUNNING_BEFORE=""
    for service in $(svc_all_services); do
        if [ -n "$(svc_pid "$service")" ]; then
            RUNNING_BEFORE="$RUNNING_BEFORE $service"
        fi
    done

    "$REPO_DIR/recog-service.sh" install

    # Readiness gate. It runs AFTER install (units are just files) but BEFORE
    # any activation, so an incomplete install neither starts a broken
    # pipeline nor stops a working one.
    readiness="$(deps_readiness_problems)"
    if [ -n "$readiness" ]; then
        SETUP_STATE="incomplete"
        echo -e "${RED}[error]${NC} Not activating services — the install is not complete:" >&2
        while IFS= read -r line; do
            [ -n "$line" ] && echo "  - $line" >&2
        done <<< "$readiness"
        echo "  Previously running services were left alone." >&2
    else
        # Отказ активации виден только здесь: дальше остаётся один
        # SETUP_STATE, и без этого флага сводка печатала зелёное «services
        # are running» с exit 0, даже когда не поднялся ни один юнит
        # (на Linux это штатный случай — нет user-D-Bus или linger).
        ACTIVATION_FAILED=""
        for service in $(svc_installed_services); do
            # shellcheck disable=SC2015 # info only prints, so || runs when the service step failed
            case " $RUNNING_BEFORE " in
                *" $service "*)
                    svc_restart "$service" && info "Restarted $(svc_label "$service")" \
                        || { warn "Failed to restart $(svc_label "$service")"; ACTIVATION_FAILED=1; } ;;
                *)
                    svc_start "$service" && info "Started $(svc_label "$service")" \
                        || { warn "Failed to start $(svc_label "$service")"; ACTIVATION_FAILED=1; } ;;
            esac
        done
        if [ -n "$ACTIVATION_FAILED" ]; then
            SETUP_STATE="partial"
        else
            SETUP_STATE="running"
        fi
    fi
fi

# ---------------------------------------------------------------------------
# 13. Summary
# ---------------------------------------------------------------------------

frontend_responds() {
    deps_have curl || return 1
    local attempt
    # shellcheck disable=SC2034 # attempt only counts the retries
    for attempt in 1 2 3 4 5; do
        if curl -sS --max-time 2 -o /dev/null http://127.0.0.1:9080/ 2>/dev/null; then
            return 0
        fi
        sleep 1
    done
    return 1
}

echo ""
case "$SETUP_STATE" in
    running)
        echo -e "${GREEN}=== Setup complete — services are running ===${NC}"
        if frontend_responds; then
            echo "  Web UI:    http://127.0.0.1:9080"
        else
            echo "  Web UI:    http://127.0.0.1:9080 (not answering yet — ./recog-service.sh logs-web)"
        fi
        ;;
    prepared)
        echo -e "${GREEN}=== Setup complete — prepared, services not touched (--no-services) ===${NC}"
        echo "  Start them with: ./recog-service.sh install && ./recog-service.sh start"
        if [ ${#MISSING_REQUIRED[@]} -gt 0 ]; then
            echo -e "  ${YELLOW}Live capture is not ready yet — missing: ${MISSING_REQUIRED[*]}${NC}"
        fi
        ;;
    partial)
        echo -e "${RED}=== Setup NOT complete — some services failed to activate ===${NC}"
        echo "  See the warnings below, then: ./recog-service.sh status"
        ;;
    incomplete)
        echo -e "${RED}=== Setup NOT complete — services were not activated ===${NC}"
        if [ ${#MISSING_REQUIRED[@]} -gt 0 ]; then
            echo "  Missing: ${MISSING_REQUIRED[*]}"
        fi
        echo "  Fix the problems above and rerun ./setup.sh"
        ;;
esac

echo ""
echo "  Extension: chrome://extensions → Developer mode → Load unpacked →"
echo "             $EXTENSION_DIR/dist"
if [ "$HOST_OS" = "darwin" ]; then
    echo "  Screen Recording permission is required for system audio:"
    echo "             System Settings → Privacy & Security → Screen Recording"
else
    echo "  Keep the user services alive after logout: loginctl enable-linger ${USER:-\$USER}"
fi
echo "  Copilot:   sign in by running claude interactively as this user; setup does not verify authentication."
echo "             Start it with the Copilot button in the web UI."
echo "             Meeting auto-start is off by default: set RECOG_COPILOT_AUTOSPAWN=1 in .env,"
echo "             then ./recog-service.sh restart-supervisor."

if [ ${#WARNINGS[@]} -gt 0 ]; then
    echo ""
    echo -e "${YELLOW}Warnings:${NC}"
    for message in "${WARNINGS[@]}"; do
        echo "  - $message"
    done
fi

if [ "$SETUP_STATE" = "incomplete" ] || [ "$SETUP_STATE" = "partial" ]; then
    exit 1
fi
exit 0
