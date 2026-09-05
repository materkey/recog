#!/bin/bash
#
# scripts/service/launchd.sh - macOS backend (launchd user agents).
#
# Implements the svc_* contract from scripts/service/common.sh. Sourced by
# recog-service.sh when the target OS is Darwin.

SVC_UNIT_DIR="${SVC_UNIT_DIR:-$HOME/Library/LaunchAgents}"
SVC_TEMPLATE_DIR="$SVC_REPO_DIR/deploy/macos"

svc_backend_name() { echo "launchd"; }

# Plists are XML: a checkout path or $HOME containing `&`, `<` or `>` would
# render an unparseable file that `launchctl load` silently refuses.
svc_escape_value() {
    printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}

svc_unit_file() { echo "com.recog.$1.plist"; }
svc_unit_name() { echo "com.recog.$1"; }
svc_template()  { echo "$SVC_TEMPLATE_DIR/com.recog.$1.plist.in"; }
svc_unit_path() { echo "$SVC_UNIT_DIR/$(svc_unit_file "$1")"; }

# Render the agent into ~/Library/LaunchAgents. Deliberately does NOT load it:
# activation is a separate step so `setup.sh` can verify dependencies first.
svc_install() {
    mkdir -p "$SVC_UNIT_DIR"
    svc_render_template "$(svc_template "$1")" "$(svc_unit_path "$1")"
}

svc_post_install() { :; }

svc_uninstall() {
    local dst; dst="$(svc_unit_path "$1")"
    launchctl unload "$dst" 2>/dev/null || true
    rm -f "$dst"
}

svc_loaded() {
    launchctl list 2>/dev/null | grep -qF "$(svc_unit_name "$1")"
}

# Activate an installed agent. Loading an already-loaded plist is an error and
# would not re-read it anyway — that is what svc_restart is for.
#
# The load rc is returned, not swallowed: with `|| true` here every caller's
# "Failed to start ..." branch was dead, so a plist launchd refuses (bad XML,
# duplicate label, missing program) was still reported as started.
svc_start() {
    local dst; dst="$(svc_unit_path "$1")"
    [ -f "$dst" ] || return 1
    if svc_loaded "$1"; then
        if [ -n "$(svc_pid "$1")" ]; then
            return 0
        fi
        # Загружен, но не запущен (PID `-` в `launchctl list`): это frontend
        # после чистого выхода при KeepAlive={SuccessfulExit:false}. Ранний
        # `return 0` здесь врал дважды: `start` не мог его поднять, а setup.sh
        # относил такой юнит к «не работал раньше» (снимок делается по
        # svc_pid), шёл сюда — и свежий плист launchd так и не перечитывал,
        # рапортуя «Started» поверх старой копии в памяти.
        svc_restart "$1"
        return
    fi
    launchctl load "$dst" 2>/dev/null
}

# Re-read the plist. `launchctl start` after `stop` reuses the in-memory copy,
# so PATH / EnvironmentVariables / ProgramArguments edits would be ignored —
# unload/load is the only way. The 1 s gap lets launchd reap the process before
# the reload races the still-bound port or log fd.
svc_restart() {
    local dst; dst="$(svc_unit_path "$1")"
    [ -f "$dst" ] || return 1
    launchctl unload "$dst" 2>/dev/null || true
    sleep 1
    launchctl load "$dst" 2>/dev/null
}

svc_stop() {
    launchctl unload "$(svc_unit_path "$1")" 2>/dev/null || true
}

# PID of the running job, empty when loaded-but-not-running or absent.
svc_pid() {
    local name pid
    name="$(svc_unit_name "$1")"
    pid="$(launchctl list 2>/dev/null | grep -F "$name" | head -1 | awk '{print $1}')"
    if [ "$pid" = "-" ]; then
        pid=""
    fi
    echo "$pid"
}

# The copilot tmux session has no launchd unit on macOS — `kill_copilot_tmux`
# in recog-service.sh is the whole cleanup.
svc_reap_copilot_session() { :; }

svc_status_hint() { :; }
