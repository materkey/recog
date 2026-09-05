#!/bin/bash
#
# scripts/service/systemd.sh - Linux backend (`systemd --user` units).
#
# Implements the svc_* contract from scripts/service/common.sh. Sourced by
# recog-service.sh when the target OS is Linux.

SVC_UNIT_DIR="${SVC_UNIT_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user}"
SVC_TEMPLATE_DIR="$SVC_REPO_DIR/deploy/linux"

# Transient unit wrapping the copilot tmux session so it survives a supervisor
# restart (recog/copilot_supervisor.py creates it via `systemd-run --user`).
# Never installed or enabled — only stopped here, on terminal intent.
SVC_COPILOT_SESSION_UNIT="recog-copilot-session.service"

svc_backend_name() { echo "systemd"; }

svc_unit_file() { echo "recog-$1.service"; }
svc_unit_name() { echo "recog-$1.service"; }
svc_template()  { echo "$SVC_TEMPLATE_DIR/recog-$1.service.in"; }
svc_unit_path() { echo "$SVC_UNIT_DIR/$(svc_unit_file "$1")"; }

_systemctl() { systemctl --user "$@" 2>/dev/null; }

# Render the unit into the user unit directory. Deliberately no `enable --now`:
# activation is a separate step so `setup.sh` can verify dependencies first.
svc_install() {
    mkdir -p "$SVC_UNIT_DIR"
    svc_render_template "$(svc_template "$1")" "$(svc_unit_path "$1")"
}

# systemd caches unit files; one reload after the whole batch is enough.
svc_post_install() {
    _systemctl daemon-reload || true
}

svc_uninstall() {
    _systemctl disable --now "$(svc_unit_name "$1")" || true
    rm -f "$(svc_unit_path "$1")"
}

svc_start() {
    [ -f "$(svc_unit_path "$1")" ] || return 1
    _systemctl enable --now "$(svc_unit_name "$1")"
}

# `daemon-reload` alone does not restart a running unit and `start` on an
# active unit is a no-op — an explicit restart is the only way to pick up new
# code or environment.
svc_restart() {
    [ -f "$(svc_unit_path "$1")" ] || return 1
    _systemctl restart "$(svc_unit_name "$1")"
}

svc_stop() {
    _systemctl stop "$(svc_unit_name "$1")" || true
}

svc_pid() {
    local pid
    pid="$(_systemctl show -p MainPID --value "$(svc_unit_name "$1")")"
    if [ "$pid" = "0" ]; then
        pid=""
    fi
    echo "$pid"
}

# "Known to the manager": enabled or currently active. Lets `status` separate
# an installed-but-inactive unit from one systemd is actually holding.
svc_loaded() {
    local unit; unit="$(svc_unit_name "$1")"
    if _systemctl is-enabled --quiet "$unit"; then
        return 0
    fi
    _systemctl is-active --quiet "$unit"
}

# The copilot session lives in its own transient unit, outside the supervisor's
# cgroup — stopping the supervisor leaves it running by design. Reap it on
# terminal intent (stop / uninstall) only.
svc_reap_copilot_session() {
    _systemctl stop "$SVC_COPILOT_SESSION_UNIT" || true
}

svc_status_hint() {
    echo "Tip: user services stop at logout unless lingering is on:"
    echo "  loginctl enable-linger ${USER:-\$USER}"
}
