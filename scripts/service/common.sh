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
# deps_install_runtime decides what the units run.
if ! declare -F deps_install_runtime > /dev/null; then
    DEPS_REPO_DIR="${DEPS_REPO_DIR:-$SVC_REPO_DIR}"
    # shellcheck source=../deps.sh
    source "$SVC_REPO_DIR/scripts/deps.sh"
fi
SVC_DATA_DIR="${SVC_DATA_DIR:-$(bash "$SVC_REPO_DIR/scripts/data-dir.sh")}"
SVC_BIN_DIR="$SVC_REPO_DIR/bin"
SVC_LOG_DIR="${SVC_LOG_DIR:-$SVC_DATA_DIR/logs}"

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

svc_cli_path_prefix() {
    local cli executable extra=""
    # Preserve CLI locations discovered in the installer's shell: systemd
    # and launchd do not inherit npm-prefix or other custom PATH entries.
    for cli in claude tmux; do
        executable="$(type -P "$cli")" || executable=""
        case "$executable" in
            /*) extra="$extra$(dirname "$executable"):" ;;
        esac
    done
    printf '%s' "$extra"
}

# The program and arguments a service's unit runs, one per line. With Python
# that is `python -m <module>`, whose module hands over to the Rust binary when
# the service's own runtime asks for it; without Python ([install] runtime =
# "rust") the unit runs the binary in bin/ directly, the checkout named.
svc_command() {
    if [ "$(deps_install_runtime)" = "rust" ]; then
        case "$1" in
            live)               printf '%s\n' "$SVC_BIN_DIR/recog" live --system-audio auto --ktalk ;;
            frontend)           printf '%s\n' "$SVC_BIN_DIR/recog-web" --host 127.0.0.1 --port 9080 --repo "$SVC_REPO_DIR" ;;
            copilot-channel)    printf '%s\n' "$SVC_BIN_DIR/recog-copilot-channel" --repo-root "$SVC_REPO_DIR" ;;
            copilot-supervisor) printf '%s\n' "$SVC_BIN_DIR/recog-copilot-supervisor" --repo-root "$SVC_REPO_DIR" ;;
            *)                  return 1 ;;
        esac
        return
    fi
    local python; python="$(svc_python)"
    case "$1" in
        live)               printf '%s\n' "$python" -m recog live --system-audio auto --ktalk ;;
        frontend)           printf '%s\n' "$python" -m recog.web.server ;;
        copilot-channel)    printf '%s\n' "$python" -m recog.copilot_channel ;;
        copilot-supervisor) printf '%s\n' "$python" -m recog.copilot_supervisor ;;
        *)                  return 1 ;;
    esac
}

# The command as the unit format writes it, one argument per line on stdin.
# Backends override it; the default (systemd's ExecStart=) quotes the program,
# as the templates quote every path, and any argument a space would split.
svc_format_command() {
    local arg first=1 line=""
    while IFS= read -r arg; do
        if [ "$first" -eq 1 ]; then
            line="\"$arg\""
            first=0
        else
            case "$arg" in
                *[!A-Za-z0-9_./:=-]*|"") line="$line \"$arg\"" ;;
                *)                       line="$line $arg" ;;
            esac
        fi
    done
    printf '%s' "$line"
}

# --- template rendering ----------------------------------------------------

# Escape a value for the unit-file *format*. Backends override it; the default
# (systemd, an ini-like format where the templates quote the values) is a
# pass-through.
svc_escape_value() {
    printf '%s' "$1"
}

# sed replacement values are interpolated into `s|@X@|value|g`, so the three
# characters with meaning there must be escaped.
_svc_sed_escape() {
    printf '%s' "$1" | sed -e 's/[\\|&]/\\&/g'
}

# Format escaping first, sed escaping second: `&` produced by the former must
# reach the replacement literally.
_svc_value() {
    _svc_sed_escape "$(svc_escape_value "$1")"
}

# @COMMAND@: the service's command in the unit format, already escaped for it,
# then for sed. A multi-line value (launchd's one <string> per argument) keeps
# its lines: sed splits the output line on a backslash-escaped newline.
_svc_command_value() {
    # In variables: bash 3.2 (macOS /bin/bash) leaves $'...' inside a quoted
    # ${...//...} unexpanded.
    local value newline=$'\n' escaped_newline=$'\\\n'
    value="$(svc_command "$1" | svc_format_command)" || return 1
    value="$(_svc_sed_escape "$value")"
    printf '%s' "${value//$newline/$escaped_newline}"
}

# Render <src> into <dst> for <service>, substituting every @PLACEHOLDER@.
# Fails (leaving <dst> untouched) if any @TOKEN@ survives — a typo in a
# template must not reach the service manager as a literal path. Without a
# service @COMMAND@ stays and fails the same way.
svc_render_template() {
    local src="$1" dst="$2" service="${3:-}"
    if [ ! -f "$src" ]; then
        echo "service template not found: $src" >&2
        return 1
    fi
    mkdir -p "$(dirname "$dst")"

    local command_args=()
    if [ -n "$service" ]; then
        local command; command="$(_svc_command_value "$service")" || {
            echo "no command for service: $service" >&2
            return 1
        }
        command_args=(-e "s|@COMMAND@|$command|g")
    fi

    local tmp="$dst.tmp.$$"
    sed "${command_args[@]}" \
        -e "s|@REPO_DIR@|$(_svc_value "$SVC_REPO_DIR")|g" \
        -e "s|@DATA_DIR@|$(_svc_value "$SVC_DATA_DIR")|g" \
        -e "s|@HOME@|$(_svc_value "$HOME")|g" \
        -e "s|@PYTHON@|$(_svc_value "$(svc_python)")|g" \
        -e "s|@LOG_DIR@|$(_svc_value "$SVC_LOG_DIR")|g" \
        -e "s|@NODE_BIN@|$(_svc_value "$(svc_node_bin)")|g" \
        -e "s|@CLI_PATH_PREFIX@|$(_svc_value "$(svc_cli_path_prefix)")|g" \
        -e "s|@EXTRA_PATH@|$(_svc_value "$(svc_extra_path)")|g" \
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
        svc_render_template "$(svc_template "$s")" "$dir/$(svc_unit_file "$s")" "$s" || return 1
        echo "$dir/$(svc_unit_file "$s")"
    done
}
