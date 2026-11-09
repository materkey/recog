#!/bin/bash
#
# recog-service.sh - Manage recog live transcription and frontend services
#
# Usage:
#   ./recog-service.sh install   - Render and install all service units
#   ./recog-service.sh uninstall - Stop and remove all services
#   ./recog-service.sh start     - Start all installed services
#   ./recog-service.sh stop      - Stop all services
#   ./recog-service.sh restart   - Restart all services
#   ./recog-service.sh status    - Show status of all services
#   ./recog-service.sh debug     - Show debug info (participants, speaker, mics)
#   ./recog-service.sh logs      - Tail logs (stderr)
#   ./recog-service.sh logs-out  - Tail logs (stdout/transcripts)
#   ./recog-service.sh logs-web  - Tail frontend logs
#   ./recog-service.sh render --dry-run <dir> - Render units into <dir> only
#   ./recog-service.sh claude    - Launch Claude Code wired to the Copilot channel
#
# The service units are rendered from deploy/{macos,linux}/*.in; the
# service-manager verbs live in scripts/service/{launchd,systemd}.sh behind the
# svc_* contract. RECOG_SERVICE_OS overrides the `uname -s` backend choice —
# it only affects which templates and verbs are used, so `render --dry-run`
# can produce Linux units on macOS and vice versa.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

SVC_REPO_DIR="$SCRIPT_DIR"
# Stopping writers must still work if a partial migration left conflicting roots.
case "${1:-}" in
    stop|uninstall) SVC_DATA_DIR="${RECOG_DATA_DIR:-$HOME/.recog}" ;;
esac
source "$SCRIPT_DIR/scripts/service/common.sh"
LOG_DIR="$SVC_LOG_DIR"

DEPS_REPO_DIR="$SCRIPT_DIR"
source "$SCRIPT_DIR/scripts/deps.sh"

SERVICE_OS="${RECOG_SERVICE_OS:-$(uname -s)}"
case "$SERVICE_OS" in
    Darwin) source "$SCRIPT_DIR/scripts/service/launchd.sh" ;;
    Linux)  source "$SCRIPT_DIR/scripts/service/systemd.sh" ;;
    *)      echo "Unsupported OS: $SERVICE_OS (expected Darwin or Linux)" >&2; exit 1 ;;
esac

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

ensure_logs_dir() {
    mkdir -p "$LOG_DIR"
}

# Gate for `start` / `restart`: the same probes setup.sh runs (scripts/deps.sh),
# before anything is stopped or activated. An install that cannot serve the
# full live pipeline must not replace one that can — a restart that unloads a
# working service and then fails to bring up a broken one is the worst
# outcome. RECOG_SKIP_READINESS=1 bypasses it (running a subset deliberately,
# or tests driving stub service managers).
require_ready() {
    [ "${RECOG_SKIP_READINESS:-0}" = "1" ] && return 0

    local problems
    problems="$(deps_readiness_problems)"
    [ -n "$problems" ] || return 0

    warn "Install is not complete — services left untouched:"
    while IFS= read -r line; do
        [ -n "$line" ] && warn "  - $line"
    done <<< "$problems"
    error "Fix the above, then retry (RECOG_SKIP_READINESS=1 bypasses this check)"
}

# Probe the copilot channel MCP daemon with a real `initialize` call.
# Returns 0 if the server responds within 1s, non-zero otherwise.
probe_copilot_daemon() {
    curl -sS --max-time 1 -X POST http://127.0.0.1:8767/mcp \
        -H "Content-Type: application/json" \
        -H "Accept: application/json, text/event-stream" \
        -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"recog-service","version":"0"}}}' \
        > /dev/null 2>&1
}

tail_log() {
    local f="$LOG_DIR/$1"
    [ -f "$f" ] || error "Log file not found: $f"
    tail -f "$f"
}

# Reap the detached Claude copilot session the supervisor spawns in its own
# tmux socket (`tmux -L recog-copilot`). Stopping the supervisor does NOT kill
# it: launchd sends SIGTERM, which terminates the daemon before its async
# teardown runs, and on Linux the session deliberately lives in its own
# transient unit outside the supervisor's cgroup — so it would linger
# indefinitely with no supervisor left to run the grace kill. Called on
# `stop`/`uninstall` (terminal intent: leave nothing behind) but deliberately
# NOT on `restart`: a mid-meeting restart must keep the live copilot, which the
# supervisor re-adopts on reconnect via meeting_state reconciliation.
kill_copilot_tmux() {
    svc_reap_copilot_session
    if command -v tmux > /dev/null 2>&1; then
        # Both session kinds: `copilot` (meeting-driven) and `copilot-manual`
        # (webapp manual session, DW-9) — leave nothing behind.
        local session_name attempt session_alive failed=0
        for session_name in copilot copilot-manual; do
            session_alive=1
            # shellcheck disable=SC2034 # attempt only counts the retries
            for attempt in 1 2 3; do
                tmux -L recog-copilot kill-session -t "=$session_name" 2>/dev/null || true
                if ! tmux -L recog-copilot has-session -t "=$session_name" 2>/dev/null; then
                    session_alive=0
                    break
                fi
            done
            if [ "$session_alive" -eq 1 ]; then
                warn "Failed to stop tmux session: $session_name"
                failed=1
            fi
        done
        return "$failed"
    fi
}

# Targeted restart (restart-web / restart-live / ...). Goes through the same
# readiness gate as `start`/`restart`: it stops and re-activates a service, so
# an incomplete install would take a working one down without being able to
# bring it back. The missing-unit case is checked before svc_restart so a real
# activation failure is not reported as "not installed".
restart_one() {
    local service="$1" tail="${2-}"
    local label; label="$(svc_label "$service")"
    [ -f "$(svc_unit_path "$service")" ] || \
        error "$label is not installed — run './recog-service.sh install' first"
    require_ready
    info "Restarting $label..."
    svc_restart "$service" || error "Failed to restart $label"
    info "$label restarted.${tail:+ $tail}"
}

check_service_status() {
    local service="$1"
    local label; label="$(svc_label "$service")"

    echo -e "${CYAN}[$label]${NC}"

    if [ ! -f "$(svc_unit_path "$service")" ]; then
        echo -e "  Installed: ${RED}No${NC}"
        return
    fi
    echo -e "  Installed: ${GREEN}Yes${NC}"

    local pid; pid="$(svc_pid "$service")"
    if [ -n "$pid" ]; then
        echo -e "  Running:   ${GREEN}Yes${NC} (PID: $pid)"
    elif svc_loaded "$service"; then
        echo -e "  Running:   ${YELLOW}Loaded but not running${NC}"
    else
        echo -e "  Running:   ${RED}No${NC}"
    fi
}

cmd_install() {
    info "Installing services ($(svc_backend_name))..."
    ensure_logs_dir

    local service
    for service in $(svc_all_services); do
        svc_install "$service" || error "Failed to install $(svc_label "$service")"
        info "Installed $(svc_label "$service") → $(svc_unit_path "$service")"
    done
    svc_post_install

    info "Services installed. Use './recog-service.sh start' to start them."
    info "Logs will be written to: $LOG_DIR/"
}

cmd_uninstall() {
    info "Uninstalling services..."

    local service
    for service in $(svc_all_services); do
        svc_uninstall "$service"
        info "Uninstalled $(svc_label "$service")"
    done
    svc_post_install
    # No `|| true`: a copilot session that survived cleanup must fail the
    # command loudly instead of being reported as uninstalled (set -e).
    kill_copilot_tmux

    info "Services uninstalled."
}

cmd_start() {
    require_ready
    info "Starting services..."
    ensure_logs_dir

    local installed; installed="$(svc_installed_services)"
    if [ -z "$installed" ]; then
        error "No services installed — run './recog-service.sh install' first"
    fi

    local service
    for service in $installed; do
        svc_start "$service" || warn "Failed to start $(svc_label "$service")"
    done

    sleep 1
    cmd_status

    echo ""
    info "Frontend available at: http://127.0.0.1:9080"
}

cmd_stop() {
    info "Stopping services..."

    local service
    for service in $(svc_installed_services); do
        svc_stop "$service"
    done
    kill_copilot_tmux

    info "Services stopped."
}

cmd_restart() {
    require_ready
    # NOTE: intentionally does NOT go through cmd_stop — a mid-meeting restart
    # must keep the live copilot session (the supervisor re-adopts it on
    # reconnect). Only cmd_stop/cmd_uninstall reap it via kill_copilot_tmux.
    local installed; installed="$(svc_installed_services)"
    if [ -z "$installed" ]; then
        error "No services installed — run './recog-service.sh install' first"
    fi

    local service
    for service in $installed; do
        info "Restarting $(svc_label "$service")..."
        svc_restart "$service" || warn "Failed to restart $(svc_label "$service")"
    done

    sleep 1
    cmd_status

    echo ""
    info "Frontend available at: http://127.0.0.1:9080"
}

cmd_restart_web()        { restart_one frontend "Available at: http://127.0.0.1:9080"; }
cmd_restart_live()       { restart_one live; }
cmd_restart_copilot()    { restart_one copilot-channel "(http://127.0.0.1:8767/mcp)"; }
cmd_restart_supervisor() { restart_one copilot-supervisor; }

# Render every unit into a scratch directory without touching the service
# manager. Used by tests/CI to validate the templates on either OS.
cmd_render() {
    local flag="${1:-}" dir="${2:-}"
    if [ "$flag" != "--dry-run" ] || [ -z "$dir" ]; then
        error "Usage: $0 render --dry-run <dir>"
    fi
    svc_render_all "$dir" || error "Failed to render $(svc_backend_name) units"
}

cmd_status() {
    echo "=== Service Status ($(svc_backend_name)) ==="
    echo ""

    local service first=1
    for service in $(svc_all_services); do
        [ "$first" -eq 1 ] || echo ""
        first=0
        check_service_status "$service"
    done

    echo ""
    echo "=== Logs ==="
    for log in live.stderr.log live.stdout.log frontend.stderr.log frontend.stdout.log copilot-channel.stderr.log copilot-channel.stdout.log copilot-supervisor.stderr.log copilot-supervisor.stdout.log; do
        if [ -f "$LOG_DIR/$log" ]; then
            SIZE=$(du -h "$LOG_DIR/$log" | cut -f1)
            LINES=$(wc -l < "$LOG_DIR/$log" | tr -d ' ')
            echo "  $log: $SIZE ($LINES lines)"
        fi
    done

    echo ""
    echo "=== Recent Live stderr (last 5 lines) ==="
    if [ -f "$LOG_DIR/live.stderr.log" ]; then
        tail -5 "$LOG_DIR/live.stderr.log" 2>/dev/null || echo "(empty)"
    else
        echo "(no log file)"
    fi

    local hint; hint="$(svc_status_hint)"
    if [ -n "$hint" ]; then
        echo ""
        echo "$hint"
    fi
}

cmd_logs()            { tail_log live.stderr.log; }
cmd_logs_out()        { tail_log live.stdout.log; }
cmd_logs_web()        { tail_log frontend.stderr.log; }
cmd_logs_copilot()    { tail_log copilot-channel.stderr.log; }
cmd_logs_supervisor() { tail_log copilot-supervisor.stderr.log; }

cmd_debug() {
    echo "=== Recog Debug Status ==="
    echo ""

    # Check if frontend is running
    if ! curl -s "http://127.0.0.1:9080/api/debug" > /dev/null 2>&1; then
        error "Frontend server not responding at http://127.0.0.1:9080"
    fi

    # Get debug info from frontend
    RESPONSE=$(curl -s "http://127.0.0.1:9080/api/debug")

    # Pretty print with jq if available, otherwise raw
    if command -v jq &> /dev/null; then
        echo "$RESPONSE" | jq .
    else
        echo "$RESPONSE"
    fi
}

cmd_claude() {
    # Launch Claude Code with the copilot channel enabled.
    # Two preconditions it cannot self-fix:
    #   1. The copilot_channel daemon must be running on :8767 — Claude
    #      Code spawns the channel handler from --channels but cannot start
    #      our HTTP MCP server itself. Normally the service manager keeps it
    #      up via the copilot-channel unit (installed by `./recog-service.sh
    #      install`); we still probe the port here as a sanity check, and
    #      try to (re)start the unit if the probe fails.
    #   2. CWD must be the recog project dir so Claude reads the
    #      project-scope mcpServers entry (`copilot` → http://127.0.0.1:8767/mcp).
    #      Launching from $HOME silently produces "no MCP server configured
    #      with that name" because the project entry is invisible there.
    local channel_unit; channel_unit="$(svc_unit_path copilot-channel)"
    if probe_copilot_daemon; then
        info "Copilot channel daemon is up at http://127.0.0.1:8767/mcp"
    elif [ ! -f "$channel_unit" ]; then
        warn "Copilot channel daemon is NOT responding at http://127.0.0.1:8767/mcp"
        warn "Service unit not installed at $channel_unit — start manually:"
        warn "  $SVC_BIN_DIR/recog-copilot-channel --repo-root $SCRIPT_DIR"
        warn ""
        warn "Launching Claude Code anyway — channel push will be a no-op until the daemon comes up."
    else
        warn "Copilot channel daemon is NOT responding at http://127.0.0.1:8767/mcp"
        warn "Attempting to restart $(svc_unit_name copilot-channel)..."
        svc_restart copilot-channel || true
        sleep 1
        if probe_copilot_daemon; then
            info "Copilot channel daemon is up at http://127.0.0.1:8767/mcp"
        else
            warn "Daemon still not responding. Check logs:"
            warn "  ./recog-service.sh logs-copilot"
            warn "Launching Claude Code anyway — channel push will be a no-op until the daemon comes up."
        fi
    fi

    if ! command -v claude > /dev/null 2>&1; then
        error "claude CLI not found in PATH"
    fi

    info "Launching Claude Code (project: $SCRIPT_DIR, channel: server:copilot) ..."
    # cd before exec so project-scope mcpServers is read; --dangerously-load-development-channels
    # bypasses the Anthropic-curated channel allowlist for our local server entry.
    cd "$SCRIPT_DIR"

    # Claude Code shows a per-session DevChannelsDialog ("I am using this for
    # local development") when --dangerously-load-development-channels is
    # combined with an active claude.ai login (interactiveHelpers.tsx:267).
    # There is no env-var or persistent-trust bypass in the source; the dialog
    # is always rendered.  Auto-press Enter via expect so the user does not
    # have to confirm every launch.  Default Select highlight is the accept
    # option, so a single carriage-return is enough.  Pass control back to
    # the user with `interact` after the dialog passes.
    # Additional roots Claude should treat as session-allowed (read/grep/glob
    # without per-file permission prompts). Skip any path that does not exist
    # so claude does not print a startup warning.
    local extra_dirs_args=""
    for dir in "$HOME/projects" "/Users/Shared/projects"; do
        if [ -d "$dir" ]; then
            extra_dirs_args="$extra_dirs_args --add-dir $dir"
        fi
    done

    # The project ships an agent at .claude/agents/copilot-session.md that
    # eagerly preloads the copilot-mode skill (initialPrompt + skills:
    # frontmatter). Use it unless the user is debugging the wrapper itself.
    local agent_args="--agent copilot-session"
    if [ "${RECOG_NO_AGENT:-0}" = "1" ]; then
        agent_args=""
        warn "RECOG_NO_AGENT=1 → skipping --agent copilot-session (skill not preloaded)"
    fi

    # --strict-mcp-config + inline --mcp-config restore the copilot MCP server
    # for the manual launch: the project-scope entry in ~/.claude.json
    # disappeared, so without these flags Claude Code starts with no `copilot`
    # server (channel push is a no-op). This mirrors what the supervisor spawns
    # (rust/recog-copilot's supervisor) — built-ins + copilot
    # only, no user MCP servers. URL overridable via RECOG_COPILOT_MCP_URL.
    # The model and the effort ("auto" without one), on two lines.
    local copilot_launch copilot_model copilot_effort
    copilot_launch="$(deps_recog config copilot-launch)" || return 1
    copilot_model="${copilot_launch%%$'\n'*}"
    copilot_effort="${copilot_launch##*$'\n'}"
    local effort_args=()
    if [ "$copilot_effort" != "auto" ]; then
        effort_args=(--effort "$copilot_effort")
    else
        copilot_effort=""
    fi
    local mcp_url="${RECOG_COPILOT_MCP_URL:-http://127.0.0.1:8767/mcp}"
    local mcp_config="{\"mcpServers\":{\"copilot\":{\"type\":\"http\",\"url\":\"$mcp_url\"}}}"

    if [ "${RECOG_NO_AUTO_CONFIRM:-0}" = "1" ] || ! command -v expect > /dev/null 2>&1; then
        if [ "${RECOG_NO_AUTO_CONFIRM:-0}" != "1" ]; then
            warn "expect(1) not found — falling back to interactive launch (manual Enter needed)"
        fi
        # shellcheck disable=SC2086 # flag lists split on purpose; the expect script below interpolates the same strings
        exec claude --model "$copilot_model" "${effort_args[@]}" --strict-mcp-config --mcp-config "$mcp_config" --dangerously-load-development-channels server:copilot $extra_dirs_args $agent_args
    fi

    # Two non-obvious facts dictate this script (verified against
    # claude-code source + a real-pty dogfood):
    #
    # 1. Ink renders text with per-word cursor-forward escapes (\x1b[1C)
    #    INSTEAD of literal spaces, so substring patterns like "Enter to
    #    confirm" or "I am using this for local development" never match
    #    the byte stream — Ink emits e.g. "Enter\x1b[1Cto\x1b[1Cconfirm".
    #    Match on a single unique word ("confirm") plus a co-occurring
    #    word ("Esc") that only appear together in the dialog footer.
    #
    # 2. Sending CR ("\r") to dismiss the Select dialog is unreliable —
    #    PTY line-discipline and Ink's keypress library deferred subscription
    #    combine to swallow it. The Select widget's `useInput` handler
    #    honours numeric digits as direct option selection
    #    (use-select-input.ts:251-282 — pressing "1" calls
    #    `state.onChange?.(options[0].value)` regardless of whether the
    #    keybinding system has finished mounting). Sending "1" picks
    #    option 1 ("I am using this for local development") deterministically.
    #
    # TERM=xterm-256color is also pinned so Ink does not emit kitty-only
    # DCS queries whose responses bleed into stdout.
    local expect_debug_arg=""
    if [ "${RECOG_CLAUDE_DEBUG:-0}" = "1" ]; then
        expect_debug_arg="-d"
        warn "RECOG_CLAUDE_DEBUG=1 → expect will dump diagnostic trace to stderr"
    fi

    # The JSON mcp-config is wrapped in Tcl braces ({$mcp_config}): bash
    # substitutes the value, then Tcl strips one brace level, passing the JSON
    # (with its literal double quotes) as a single argument without any Tcl
    # substitution on `$`/`[`/`\` inside.
    exec env TERM=xterm-256color RECOG_COPILOT_LAUNCH_MODEL="$copilot_model" RECOG_COPILOT_LAUNCH_EFFORT="$copilot_effort" expect $expect_debug_arg -c "
        set timeout 30
        log_user 1
        set effort_args {}
        if {\$env(RECOG_COPILOT_LAUNCH_EFFORT) ne {}} {
            set effort_args [list --effort \$env(RECOG_COPILOT_LAUNCH_EFFORT)]
        }
        spawn -noecho claude --model \$env(RECOG_COPILOT_LAUNCH_MODEL) {*}\$effort_args --strict-mcp-config --mcp-config {$mcp_config} --dangerously-load-development-channels server:copilot $extra_dirs_args $agent_args
        expect {
            -re {confirm.*Esc|Esc.*confirm} {
                # 0.5 s settle: Ink ends the dialog with a synchronized-output
                # mode close (\\x1b\[?2026l) right after the footer and useInput
                # subscribes during that paint. Without the delay our digit
                # can land before the subscription is live.
                sleep 0.5
                send -- \"1\"
            }
            timeout { puts \"\\n\[recog-service\] dev-channels dialog did not appear within 30s — proceeding anyway\" }
            eof    { exit }
        }
        interact
    "
}

cmd_help() {
    echo "recog-service.sh - Manage recog live transcription and frontend services"
    echo ""
    echo "Usage: $0 <command>"
    echo ""
    echo "Commands:"
    echo "  install         - Render and install all service units (no activation)"
    echo "  uninstall       - Stop and remove all services"
    echo "  start           - Start all installed services"
    echo "  stop            - Stop all services"
    echo "  restart         - Restart all services"
    echo "  restart-web        - Restart frontend only"
    echo "  restart-live       - Restart live transcription only"
    echo "  restart-copilot    - Restart copilot channel daemon only"
    echo "  restart-supervisor - Restart copilot supervisor daemon only"
    echo "  status             - Show status of all services"
    echo "  render --dry-run <dir> - Render service units into <dir> only"
    echo "  debug              - Show debug info (participants, speaker, mics)"
    echo "  logs               - Tail live stderr logs"
    echo "  logs-out           - Tail live stdout logs (transcripts)"
    echo "  logs-web           - Tail frontend logs"
    echo "  logs-copilot       - Tail copilot channel daemon logs"
    echo "  logs-supervisor    - Tail copilot supervisor daemon logs"
    echo "  claude             - Launch Claude Code wired to the Copilot channel"
    echo ""
    echo "Services ($(svc_backend_name)):"
    echo "  - Live transcription: $(svc_command live | tr '\n' ' ')"
    echo "  - Frontend: http://127.0.0.1:9080"
    echo "  - Copilot channel daemon: $(svc_command copilot-channel | tr '\n' ' ')(http://127.0.0.1:8767/mcp)"
    echo "  - Copilot supervisor: $(svc_command copilot-supervisor | tr '\n' ' ')(auto-spawns Claude per meeting)"
}

# Main
case "${1:-}" in
    install)         cmd_install ;;
    uninstall)       cmd_uninstall ;;
    start)           cmd_start ;;
    stop)            cmd_stop ;;
    restart)         cmd_restart ;;
    restart-web)        cmd_restart_web ;;
    restart-live)       cmd_restart_live ;;
    restart-copilot)    cmd_restart_copilot ;;
    restart-supervisor) cmd_restart_supervisor ;;
    status)             cmd_status ;;
    render)             shift; cmd_render "$@" ;;
    debug)              cmd_debug ;;
    logs)               cmd_logs ;;
    logs-out)           cmd_logs_out ;;
    logs-web)           cmd_logs_web ;;
    logs-copilot)       cmd_logs_copilot ;;
    logs-supervisor)    cmd_logs_supervisor ;;
    claude)             cmd_claude ;;
    help|--help|-h)  cmd_help ;;
    *)
        cmd_help
        exit 1
        ;;
esac
