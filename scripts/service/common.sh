#!/bin/bash
#
# scripts/service/common.sh - backend-independent service helpers.
#
# Sourced by recog-service.sh before one of the backends (launchd.sh /
# systemd.sh). Owns the service list, the placeholder values and the template
# renderer; the backend owns the svc_* verbs that talk to the service manager.
#
# Requires SVC_REPO_DIR to be set by the caller.

SVC_REPO_DIR="${SVC_REPO_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
SVC_LOG_DIR="${SVC_LOG_DIR:-$SVC_REPO_DIR/logs}"

# Canonical service names. Order matters: install/start/status walk it as-is.
# Transient units (the copilot tmux session on Linux) are deliberately absent —
# they are created on spawn, never installed.
SVC_ALL_SERVICES="live frontend copilot-channel copilot-supervisor"

svc_all_services() {
    echo "$SVC_ALL_SERVICES"
}

svc_label() {
    case "$1" in
        live)               echo "Live Transcription" ;;
        frontend)           echo "Frontend" ;;
        copilot-channel)    echo "Copilot Channel" ;;
        copilot-supervisor) echo "Copilot Supervisor" ;;
        *)                  echo "$1" ;;
    esac
}

# --- placeholder values ----------------------------------------------------

# The interpreter the units run. RECOG_PYTHON overrides it (tests, non-standard
# venv layouts); otherwise the repo venv wins over whatever is in PATH.
svc_python() {
    if [ -n "${RECOG_PYTHON:-}" ]; then
        echo "$RECOG_PYTHON"
    elif [ -x "$SVC_REPO_DIR/venv-gigaam/bin/python3" ]; then
        echo "$SVC_REPO_DIR/venv-gigaam/bin/python3"
    else
        command -v python3 || echo "python3"
    fi
}

# Directory holding `node`, replacing the hardcoded nvm path the old plists
# carried. AI providers and the title generator shell out to node-based CLIs
# (recog/node_env.py), which a service-manager PATH would not otherwise find.
# Falls back to /usr/bin so the rendered PATH never contains an empty element
# (an empty PATH element means "current directory").
svc_node_bin() {
    local node
    node="$(command -v node 2>/dev/null)" || node=""
    if [ -n "$node" ]; then
        dirname "$node"
    else
        echo "/usr/bin"
    fi
}

# Extra PATH entries that exist on one OS or the other: Homebrew (macOS) and
# the user-local bin where `claude` is installed. Non-existent entries in PATH
# are harmless, so both are always emitted.
svc_extra_path() {
    echo "/opt/homebrew/bin:$HOME/.local/bin"
}

# --- template rendering ----------------------------------------------------

# sed replacement values are interpolated into `s|@X@|value|g`, so the three
# characters with meaning there must be escaped.
_svc_sed_escape() {
    printf '%s' "$1" | sed -e 's/[\\|&]/\\&/g'
}

# Render <src> into <dst>, substituting every @PLACEHOLDER@. Fails (leaving
# <dst> untouched) if any @TOKEN@ survives — a typo in a template must not
# reach the service manager as a literal path.
svc_render_template() {
    local src="$1" dst="$2"
    if [ ! -f "$src" ]; then
        echo "service template not found: $src" >&2
        return 1
    fi
    mkdir -p "$(dirname "$dst")"

    local tmp="$dst.tmp.$$"
    sed \
        -e "s|@REPO_DIR@|$(_svc_sed_escape "$SVC_REPO_DIR")|g" \
        -e "s|@HOME@|$(_svc_sed_escape "$HOME")|g" \
        -e "s|@PYTHON@|$(_svc_sed_escape "$(svc_python)")|g" \
        -e "s|@LOG_DIR@|$(_svc_sed_escape "$SVC_LOG_DIR")|g" \
        -e "s|@NODE_BIN@|$(_svc_sed_escape "$(svc_node_bin)")|g" \
        -e "s|@EXTRA_PATH@|$(_svc_sed_escape "$(svc_extra_path)")|g" \
        "$src" > "$tmp" || { rm -f "$tmp"; return 1; }

    local leftovers
    leftovers="$(grep -oE '@[A-Z_]+@' "$tmp" | sort -u | tr '\n' ' ')"
    if [ -n "$leftovers" ]; then
        rm -f "$tmp"
        echo "unresolved placeholders in $src: $leftovers" >&2
        return 1
    fi

    mv "$tmp" "$dst"
}

# Whether the service manager currently holds the unit (loaded / enabled /
# active). Backends override this; the default keeps `status` honest on a
# backend that cannot tell.
svc_loaded() { return 1; }

# Services whose unit file is present. Everything but `install`/`uninstall`
# iterates this set, so a partially installed machine is not a hard error.
svc_installed_services() {
    local s
    for s in $(svc_all_services); do
        if [ -f "$(svc_unit_path "$s")" ]; then
            echo "$s"
        fi
    done
}

# Render every unit of the active backend into <dir> without touching the
# service manager (`recog-service.sh render --dry-run`). Used by tests and CI.
svc_render_all() {
    local dir="$1" s
    mkdir -p "$dir"
    for s in $(svc_all_services); do
        svc_render_template "$(svc_template "$s")" "$dir/$(svc_unit_file "$s")" || return 1
        echo "$dir/$(svc_unit_file "$s")"
    done
}
