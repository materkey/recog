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

# PortAudio is a shared library, so the only honest probe is loading it the
# way sounddevice does. Needs the Python deps installed first.
deps_portaudio_ok() {
    "$(deps_python)" -c "import sounddevice" > /dev/null 2>&1
}

# Prints nothing and returns 0 when the effective ASR config can serve. The
# one failure mode worth blocking on is RECOG_ASR_BACKEND=remote with no
# gateway URL: build_asr() raises at startup, so every service would crash-loop.
deps_asr_config_check() {
    "$(deps_python)" -c '
import sys
sys.path.insert(0, sys.argv[1])
import recog.config as config

if config.RECOG_ASR_BACKEND == "remote" and not (config.RECOG_ASR_REMOTE_URL or "").strip():
    print("RECOG_ASR_BACKEND=remote but RECOG_ASR_REMOTE_URL is empty — "
          "add your gateway URL to .env (build_asr() refuses to fall back to "
          "the local model silently: different RAM and latency)")
    sys.exit(1)
' "$DEPS_REPO_DIR" 2>&1
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

    if [ "$os" = "linux" ]; then
        deps_have parec || echo "parec not found — $(deps_hint parec)"
        deps_have pactl || echo "pactl not found — $(deps_hint pactl)"
        deps_portaudio_ok || echo "PortAudio not loadable (python -c 'import sounddevice' failed) — $(deps_hint portaudio), then rerun ./setup.sh"
    elif [ "$os" = "darwin" ]; then
        deps_sck_helper_ok || echo "sck_audio_capture helper missing or not executable — run ./setup.sh with swiftc available ($(deps_hint swiftc))"
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
        "tmux"   "Copilot sessions (recog/copilot_supervisor.py)" \
        "claude" "Copilot sessions (Claude Code CLI)" \
        "jq"     "pretty-printed ./recog-service.sh debug"
}
