#!/bin/bash
#
# scripts/deps.sh - host dependency probes shared by setup.sh and
# recog-service.sh.
#
# setup.sh prints them as a found/missing table with per-package-manager
# hints; `recog-service.sh start|restart` runs the same probes as a readiness
# gate. That ordering is the point: an install that cannot serve the full live
# pipeline must not be activated, and must not take down a previously working
# instance on its way to failing.
#
# RECOG_HOST_OS overrides `uname -s` (tests exercise the Linux probes from a
# Mac); DEPS_PYTHON overrides the interpreter used for library probes, which is
# deliberately NOT RECOG_PYTHON — that one selects the interpreter baked into
# the service units.

DEPS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPS_REPO_DIR="${DEPS_REPO_DIR:-$(cd "$DEPS_DIR/.." && pwd)}"

deps_host_os() {
    local raw="${RECOG_HOST_OS:-$(uname -s)}"
    case "$raw" in
        Darwin|darwin) echo "darwin" ;;
        Linux|linux)   echo "linux" ;;
        *)             printf '%s\n' "$raw" | tr '[:upper:]' '[:lower:]' ;;
    esac
}

deps_have() { command -v "$1" > /dev/null 2>&1; }

# Interpreter used for library probes (PortAudio, ASR config). The repo venv
# wins once it exists so the probe sees the same packages the services will.
deps_python() {
    if [ -n "${DEPS_PYTHON:-}" ]; then
        echo "$DEPS_PYTHON"
    elif [ -x "$DEPS_REPO_DIR/venv-gigaam/bin/python" ]; then
        echo "$DEPS_REPO_DIR/venv-gigaam/bin/python"
    elif deps_have python3; then
        command -v python3
    else
        echo "python3"
    fi
}

# apt | pacman | brew | unknown — picks which install hint to print.
deps_pkg_manager() {
    if [ "$(deps_host_os)" = "darwin" ]; then
        echo "brew"
    elif deps_have apt-get; then
        echo "apt"
    elif deps_have pacman; then
        echo "pacman"
    else
        echo "unknown"
    fi
}

# One-line "how do I get this" for a dependency name.
deps_hint() {
    local mgr; mgr="$(deps_pkg_manager)"
    case "$1" in
        python)
            case "$mgr" in
                apt)    echo "sudo apt install python3.12 python3.12-venv" ;;
                pacman) echo "sudo pacman -S python" ;;
                brew)   echo "brew install python@3.12" ;;
                *)      echo "install Python 3.12+ (uv python install 3.12)" ;;
            esac ;;
        ffmpeg)
            case "$mgr" in
                apt)    echo "sudo apt install ffmpeg" ;;
                pacman) echo "sudo pacman -S ffmpeg" ;;
                brew)   echo "brew install ffmpeg" ;;
                *)      echo "install ffmpeg" ;;
            esac ;;
        parec|pactl)
            case "$mgr" in
                apt)    echo "sudo apt install pulseaudio-utils" ;;
                pacman) echo "sudo pacman -S libpulse" ;;
                *)      echo "install the PulseAudio command-line utilities" ;;
            esac ;;
        portaudio)
            case "$mgr" in
                apt)    echo "sudo apt install libportaudio2" ;;
                pacman) echo "sudo pacman -S portaudio" ;;
                *)      echo "install PortAudio" ;;
            esac ;;
        swiftc)
            echo "xcode-select --install" ;;
        node|npm)
            case "$mgr" in
                apt)    echo "sudo apt install nodejs npm" ;;
                pacman) echo "sudo pacman -S nodejs npm" ;;
                brew)   echo "brew install node" ;;
                *)      echo "install Node.js 18+" ;;
            esac ;;
        tmux)
            case "$mgr" in
                apt)    echo "sudo apt install tmux" ;;
                pacman) echo "sudo pacman -S tmux" ;;
                brew)   echo "brew install tmux" ;;
                *)      echo "install tmux" ;;
            esac ;;
        claude)
            echo "npm install -g @anthropic-ai/claude-code" ;;
        cargo)
            echo "install Rust with rustup (https://rustup.rs)" ;;
        jq)
            case "$mgr" in
                apt)    echo "sudo apt install jq" ;;
                pacman) echo "sudo pacman -S jq" ;;
                brew)   echo "brew install jq" ;;
                *)      echo "install jq" ;;
            esac ;;
        *)
            echo "install $1" ;;
    esac
}

# --- individual probes -----------------------------------------------------

# The Swift helper is a build artifact, not a package: it is either compiled
# by setup.sh or shipped by a previous build. `-x` is the whole contract with
# recog/system_audio.py, which execs it.
deps_sck_helper_ok() {
    [ -x "$DEPS_REPO_DIR/sck_audio_capture" ]
}

# Recog.app is the program of the live launchd job (recog/macos_launcher.py):
# without it launchd has nothing to start.
deps_launcher_ok() {
    [ -x "$DEPS_REPO_DIR/Recog.app/Contents/MacOS/recog_launcher" ]
}

# PortAudio is a shared library, so the only honest probe is loading it the
# way sounddevice does. Needs the Python deps installed first.
deps_portaudio_ok() {
    "$(deps_python)" -c "import sounddevice" > /dev/null 2>&1
}

# Prints nothing and returns 0 when the effective ASR config can serve. The
# failure modes worth blocking on are RECOG_ASR_BACKEND=remote with no
# gateway URL (build_asr() raises at startup) and RECOG_ASR_BACKEND=wavo without
# the wavo_asr module (the first model load raises): every service would crash-loop.
# "auto" needs nothing: without wavo_asr it runs on PyTorch.
deps_asr_config_check() {
    "$(deps_python)" -c '
import sys
sys.path.insert(0, sys.argv[1])

try:
    import recog.install_config
except (OSError, ValueError) as error:
    print(f"Invalid installation configuration: {error}")
    sys.exit(1)

import recog.config as config

try:
    from recog.user_profile import UserProfileStore
    UserProfileStore(config.get_data_dir()).read()
except (OSError, ValueError) as error:
    print(f"Invalid user profile: {error}")
    sys.exit(1)

try:
    from recog.ai_settings import AiSettingsStore
    AiSettingsStore(config.get_data_dir()).read()
except (OSError, ValueError) as error:
    print(f"Invalid AI settings: {error}")
    sys.exit(1)

asr = recog.install_config.INSTALL_CONFIG.asr
if asr.backend == "remote" and not asr.remote_url.strip():
    print("RECOG_ASR_BACKEND=remote but RECOG_ASR_REMOTE_URL is empty — "
          "add the gateway URL as [asr] remote_url to config.toml (build_asr() "
          "refuses to fall back to the local model silently: different RAM and latency)")
    sys.exit(1)
if asr.backend == "wavo":
    try:
        import wavo_asr  # noqa: F401
    except ImportError as error:
        print(f"RECOG_ASR_BACKEND=wavo but the wavo_asr module is not installed ({error}) — "
              "run ./setup.sh with cargo on PATH; it builds rust/wavo-asr-py")
        sys.exit(1)
' "$DEPS_REPO_DIR" 2>&1
}

# The effective [asr] backend (RECOG_ASR_BACKEND): auto, local, remote or wavo. Empty
# when the configuration cannot be read; deps_asr_config_check reports why.
deps_asr_backend() {
    "$(deps_python)" -c '
import sys
sys.path.insert(0, sys.argv[1])
from recog.install_config import INSTALL_CONFIG
print(INSTALL_CONFIG.asr.backend)
' "$DEPS_REPO_DIR" 2> /dev/null || true
}

# [live] runtime of the installation configuration: "python" or "rust".
deps_live_runtime() {
    "$(deps_python)" -c '
import sys
sys.path.insert(0, sys.argv[1])
from recog.install_config import INSTALL_CONFIG
print(INSTALL_CONFIG.live.runtime)
' "$DEPS_REPO_DIR" 2> /dev/null || true
}

# [web] runtime of the installation configuration: "python" or "rust".
deps_web_runtime() {
    "$(deps_python)" -c '
import sys
sys.path.insert(0, sys.argv[1])
from recog.install_config import INSTALL_CONFIG
print(INSTALL_CONFIG.web.runtime)
' "$DEPS_REPO_DIR" 2> /dev/null || true
}

# --- readiness -------------------------------------------------------------

# Everything the full live pipeline needs, one human-readable problem per
# line. Empty output means ready. Always returns 0 — callers test the output,
# so a probe failure cannot be mistaken for a shell error.
deps_readiness_problems() {
    local os asr_problem
    os="$(deps_host_os)"

    # ffmpeg: refine with word timestamps (including the local fallback of the
    # remote backend) and segment playback both shell out to it.
    deps_have ffmpeg || echo "ffmpeg not found — $(deps_hint ffmpeg)"
    if [ "${RECOG_SKIP_COPILOT_CHECK:-0}" != "1" ]; then
        deps_have tmux || echo "tmux not found (Copilot) — $(deps_hint tmux); if Copilot is not needed: RECOG_SKIP_COPILOT_CHECK=1 ./setup.sh"
        deps_have claude || echo "claude not found (Copilot) — $(deps_hint claude); run claude interactively to sign in, then rerun ./setup.sh; if Copilot is not needed: RECOG_SKIP_COPILOT_CHECK=1 ./setup.sh"
    fi

    if [ "$os" = "linux" ]; then
        deps_have parec || echo "parec not found — $(deps_hint parec)"
        deps_have pactl || echo "pactl not found — $(deps_hint pactl)"
        deps_portaudio_ok || echo "PortAudio not loadable (python -c 'import sounddevice' failed) — $(deps_hint portaudio), then rerun ./setup.sh"
    elif [ "$os" = "darwin" ]; then
        deps_sck_helper_ok || echo "sck_audio_capture helper missing or not executable — run ./setup.sh with swiftc available ($(deps_hint swiftc))"
        deps_launcher_ok || echo "Recog.app missing — run ./setup.sh with swiftc available ($(deps_hint swiftc))"
    fi

    if ! asr_problem="$(deps_asr_config_check)"; then
        echo "$asr_problem"
    fi

    return 0
}

# Optional dependencies: absent ones degrade a feature, never the pipeline.
# Echoes "<name>\t<what breaks>" lines.
deps_optional_list() {
    printf '%s\t%s\n' \
        "npm"    "Chrome extension build (npm ci && npm run build)" \
        "node"   "AI providers and title generation (recog/node_env.py)" \
        "jq"     "pretty-printed ./recog-service.sh debug"
}
