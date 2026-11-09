#!/usr/bin/env bash
#
# setup.sh - install (or update) the whole recog pipeline with one command.
#
#   git clone <stash-url>/recog.git && cd recog
#   ./setup.sh
#
# Works on macOS (launchd, ScreenCaptureKit) and Linux (systemd --user,
# PulseAudio/PipeWire). Idempotent: re-running it is how you update after a
# `git pull` — cargo rebuilds the programs of crates/ into bin/, the model cache
# is reused, previously running services are explicitly restarted so they pick
# up the new programs and units. There is no Python: cargo builds everything.
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
#   CARGO_TARGET_DIR        where cargo builds (default target)
#   RECOG_HOST_OS           override the detected OS (tests)
#   RECOG_HOST_ARCH         override the detected CPU architecture (tests)
#   RECOG_SERVICE_OS        override the service backend (tests)
#   RECOG_SKIP_COPILOT_CHECK=1 skip tmux/claude checks when Copilot is not needed

# No `set -u`: empty-array expansions ("${RUST_BUILD_ARGS[@]}") are an
# unbound-variable error under bash 3.2, which is what /bin/bash still is on
# macOS.
set -eo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_DIR"

DEPS_REPO_DIR="$REPO_DIR"
source "$REPO_DIR/scripts/deps.sh"

HOST_OS="$(deps_host_os)"
BIN_DIR="$DEPS_BIN_DIR"

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
    sed -n '2,29p' "$REPO_DIR/setup.sh" | sed 's/^#\{0,1\} \{0,1\}//'
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

# [install] runtime is "rust", the only installation left; an old config.toml
# asking for the Python one has to be changed first.
INSTALL_RUNTIME="$(deps_install_runtime)"
case "$INSTALL_RUNTIME" in
    rust) ;;
    python) fail "[install] runtime = \"python\" is no longer available: recog installs without Python (CHANGELOG.md). Delete the key from $(deps_config_file) (or unset RECOG_INSTALL_RUNTIME) and rerun ./setup.sh" ;;
    *) fail "[install] runtime must be \"rust\", not \"$INSTALL_RUNTIME\" ($(deps_config_file) or RECOG_INSTALL_RUNTIME)" ;;
esac

# The programs cargo builds, each with its package, and where
# cargo puts them (CARGO_TARGET_DIR is relative to the checkout, where cargo runs).
RUST_BUILD_ARGS=(-p recog-live --bin recog -p recog-web --bin recog-web
    -p recog-copilot --bin recog-copilot-supervisor --bin recog-copilot-channel
    -p recog-desktop --bin recog-desktop)
if [ "$HOST_OS" = "darwin" ]; then
    RUST_BUILD_ARGS+=(-p recog-mic --bin recog-mic)
fi
RUST_RELEASE_DIR="${CARGO_TARGET_DIR:-target}"
case "$RUST_RELEASE_DIR" in
    /*) RUST_RELEASE_DIR="$RUST_RELEASE_DIR/release" ;;
    *)  RUST_RELEASE_DIR="$REPO_DIR/$RUST_RELEASE_DIR/release" ;;
esac

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
    echo "Would check cargo and the system dependencies."
    echo "Would build with cargo (release): ${RUST_BUILD_ARGS[*]}"
    echo "Would check the configuration with the built recog, then install the programs into $BIN_DIR"
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
    if [ "$DO_PREFETCH" -eq 0 ]; then
        echo "Skipped the ASR model (--no-prefetch)"
    else
        echo "Would download the ASR model gigaam-v3 into the Hugging Face cache if it is missing."
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
# 1. System dependencies
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
check_cmd cargo required "builds the programs of crates/"
if [ "$HOST_OS" = "linux" ]; then
    check_cmd parec required "system audio capture and the microphone"
    check_cmd pactl required "default-sink discovery"
elif [ "$HOST_OS" = "darwin" ]; then
    if deps_have swiftc; then
        report_dep swiftc 1 required "$(command -v swiftc)"
    elif deps_sck_helper_ok && deps_launcher_ok; then
        report_dep swiftc 1 optional "not needed — sck_audio_capture and Recog.app are already built"
    else
        report_dep swiftc 0 required "needed to build sck_audio_capture and Recog.app"
    fi
fi

while IFS=$'\t' read -r name note; do
    [ -n "$name" ] || continue
    check_cmd "$name" optional "$note"
done <<< "$(deps_optional_list)"

# ---------------------------------------------------------------------------
# 2-4. The programs of crates/: built, checked, then installed into bin/
# ---------------------------------------------------------------------------

step "Rust programs"

# Without cargo or with a failed build there is nothing to install, so stop
# before anything is touched.
deps_have cargo || fail "cargo not found — recog builds its programs with it: $(deps_hint cargo), then rerun ./setup.sh"
# From the checkout's root, the workspace.
(cd "$REPO_DIR" && cargo build --release "${RUST_BUILD_ARGS[@]}") \
    || fail "cargo build failed — nothing was installed or restarted; fix the build and rerun ./setup.sh"
for program in $(deps_rust_programs); do
    [ -x "$RUST_RELEASE_DIR/$program" ] || fail "cargo built no $RUST_RELEASE_DIR/$program — nothing was installed or restarted (is build.target-dir set elsewhere? set CARGO_TARGET_DIR)"
done
info "Built: $(deps_rust_programs)"

# The configuration, checked by the binary just built before it replaces
# the one the services run.
step "Configuration"
if ! config_problem="$(DEPS_BIN_DIR="$RUST_RELEASE_DIR" deps_asr_config_check)"; then
    echo -e "${RED}[error]${NC} $config_problem" >&2
    echo "  Nothing was installed or restarted. Fix the configuration and rerun ./setup.sh" >&2
    exit 1
fi
runtime_problem="$(DEPS_BIN_DIR="$RUST_RELEASE_DIR" deps_install_runtime_problem)"
[ -z "$runtime_problem" ] || fail "$runtime_problem"
info "Configuration OK"

# Copied, then renamed over the old one: a running service keeps the file
# it was started from, and target can be cleaned without touching them.
mkdir -p "$BIN_DIR"
for program in $(deps_rust_programs); do
    staged="$BIN_DIR/.$program.$$"
    if ! { cp "$RUST_RELEASE_DIR/$program" "$staged" && chmod 755 "$staged" && mv -f "$staged" "$BIN_DIR/$program"; }; then
        rm -f "$staged"
        fail "Could not install $BIN_DIR/$program"
    fi
done
info "Installed into $BIN_DIR"

# ---------------------------------------------------------------------------
# 5. Configuration files
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
DATA_DIR="$(bash "$REPO_DIR/scripts/data-dir.sh")"
mkdir -p "$DATA_DIR"

# ---------------------------------------------------------------------------
# 6. Chrome extension
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
# 7. macOS system-audio helper
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
    if "$BIN_DIR/recog" launcher > /dev/null; then
        info "Recog.app is ready"
    elif deps_launcher_ok; then
        info "Recog.app is already built — reusing it"
    else
        echo -e "  ${RED}[MISSING]${NC} Recog.app — could not build it ($(deps_hint swiftc))"
        MISSING_REQUIRED+=("Recog.app")
    fi
fi

# ---------------------------------------------------------------------------
# 8. ASR model
# ---------------------------------------------------------------------------

step "ASR model"

if [ "$DO_PREFETCH" = "0" ]; then
    echo "Skipped (--no-prefetch)"
# wavo's GGUF of GigaAM v3 in the Hugging Face cache; the live process would
# otherwise download it on its first transcription.
elif "$BIN_DIR/recog" prefetch > /dev/null; then
    info "ASR model ready"
else
    warn "Could not download the ASR model — the first transcription will download it"
fi

# ---------------------------------------------------------------------------
# 9. Services
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
# 10. Summary
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
echo "  Programs:  $BIN_DIR"
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
