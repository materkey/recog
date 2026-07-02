#!/bin/bash
#
# recog-service.sh - Manage recog live transcription and frontend services
#
# Usage:
#   ./recog-service.sh install   - Install and enable both services
#   ./recog-service.sh uninstall - Stop and remove both services
#   ./recog-service.sh start     - Start both services
#   ./recog-service.sh stop      - Stop both services
#   ./recog-service.sh restart   - Restart both services
#   ./recog-service.sh status    - Show status of both services
#   ./recog-service.sh debug     - Show debug info (participants, speaker, mics)
#   ./recog-service.sh logs      - Tail logs (stderr)
#   ./recog-service.sh logs-out  - Tail logs (stdout/transcripts)
#   ./recog-service.sh logs-web  - Tail frontend logs
#   ./recog-service.sh claude    - Launch Claude Code wired to the Copilot channel

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LOG_DIR="$SCRIPT_DIR/logs"

# Live transcription service
LIVE_PLIST_NAME="com.recog.live"
LIVE_PLIST_SRC="$SCRIPT_DIR/$LIVE_PLIST_NAME.plist"
LIVE_PLIST_DST="$HOME/Library/LaunchAgents/$LIVE_PLIST_NAME.plist"

# Frontend service
FRONTEND_PLIST_NAME="com.recog.frontend"
FRONTEND_PLIST_SRC="$SCRIPT_DIR/$FRONTEND_PLIST_NAME.plist"
FRONTEND_PLIST_DST="$HOME/Library/LaunchAgents/$FRONTEND_PLIST_NAME.plist"

# Copilot channel daemon (recog.copilot_channel: MCP HTTP <-> bridge WS).
# Without this Claude Code receives no notifications/claude/channel events
# (transcript_tick / copilot_user_message), so the in-meeting Copilot is dead.
COPILOT_PLIST_NAME="com.recog.copilot-channel"
COPILOT_PLIST_SRC="$SCRIPT_DIR/$COPILOT_PLIST_NAME.plist"
COPILOT_PLIST_DST="$HOME/Library/LaunchAgents/$COPILOT_PLIST_NAME.plist"

# Copilot supervisor daemon (recog.copilot_supervisor: auto-spawns the Claude
# copilot tmux session per KTalk meeting). Connects to ktalk_bridge as a
# supervisor client, drives spawn/kill via `tmux -L recog-copilot`, and reports
# agent status to the Copilot-tab chip. Replaces the manual `./recog-service.sh
# claude` step; disable auto-spawn without uninstalling via RECOG_COPILOT_AUTOSPAWN=0.
SUPERVISOR_PLIST_NAME="com.recog.copilot-supervisor"
SUPERVISOR_PLIST_SRC="$SCRIPT_DIR/$SUPERVISOR_PLIST_NAME.plist"
SUPERVISOR_PLIST_DST="$HOME/Library/LaunchAgents/$SUPERVISOR_PLIST_NAME.plist"

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

install_service() {
    local name="$1"
    local src="$2"
    local dst="$3"

    if [ ! -f "$src" ]; then
        warn "Plist file not found: $src"
        return 1
    fi

    # Always unload-before-load so an in-memory cached plist (from a
    # prior `launchctl load`) is replaced by what is on disk now.
    # `launchctl start/stop` only signal a running daemon — they do
    # NOT re-read the plist, so PATH / EnvironmentVariables changes
    # are silently ignored on a plain restart.
    cp "$src" "$dst"
    launchctl unload "$dst" 2>/dev/null || true
    launchctl load "$dst" 2>/dev/null || true
    info "Installed $name"
}

uninstall_service() {
    local name="$1"
    local dst="$2"

    launchctl unload "$dst" 2>/dev/null || true
    rm -f "$dst"
    info "Uninstalled $name"
}

# Reload an installed plist: copy fresh from repo, unload, load.
# This is the only way to make launchd pick up plist changes (PATH,
# EnvironmentVariables, ProgramArguments) — `launchctl start` after
# `launchctl stop` reuses the cached plist.
start_service() {
    local name="$1"
    local src="$2"
    local dst="$3"

    if [ -f "$src" ]; then
        cp "$src" "$dst"
    fi
    launchctl unload "$dst" 2>/dev/null || true
    launchctl load "$dst" 2>/dev/null || true
}

stop_service() {
    local name="$1"
    local dst="$2"
    launchctl unload "$dst" 2>/dev/null || true
}

# Stop every launchd service without touching the copilot tmux session.
# Shared by `stop` (which then reaps the session) and `restart` (which keeps
# it — see kill_copilot_tmux).
stop_all_services() {
    stop_service "$LIVE_PLIST_NAME" "$LIVE_PLIST_DST"
    stop_service "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_DST"
    stop_service "$COPILOT_PLIST_NAME" "$COPILOT_PLIST_DST"
    stop_service "$SUPERVISOR_PLIST_NAME" "$SUPERVISOR_PLIST_DST"
}

# Reap the detached Claude copilot session the supervisor spawns in its own
# tmux socket (`tmux -L recog-copilot`). Unloading the supervisor plist does
# NOT kill it: launchd sends SIGTERM, which terminates the daemon before its
# async teardown runs, and nothing else owns the session — so it would linger
# indefinitely with no supervisor left to run the grace kill. Called on
# `stop`/`uninstall` (terminal intent: leave nothing behind) but deliberately
# NOT on `restart`: a mid-meeting restart must keep the live copilot, which the
# supervisor re-adopts on reconnect via meeting_state reconciliation.
kill_copilot_tmux() {
    if command -v tmux > /dev/null 2>&1; then
        tmux -L recog-copilot kill-session -t copilot 2>/dev/null || true
    fi
}

# Stop, settle, then start one service. The 1 s gap matters: launchctl
# unload returns before launchd reaps the process, and a back-to-back
# load can race the still-bound port / log fd.
restart_one() {
    local name="$1" src="$2" dst="$3" label="$4" tail="${5-}"
    info "Restarting $label..."
    stop_service "$name" "$dst"
    sleep 1
    start_service "$name" "$src" "$dst"
    info "$label restarted.${tail:+ $tail}"
}

check_service_status() {
    local name="$1"
    local dst="$2"
    local label="$3"

    echo -e "${CYAN}[$label]${NC}"

    if [ -f "$dst" ]; then
        echo -e "  Installed: ${GREEN}Yes${NC}"
    else
        echo -e "  Installed: ${RED}No${NC}"
        return
    fi

    if launchctl list | grep -q "$name"; then
        PID=$(launchctl list | grep "$name" | awk '{print $1}')
        if [ "$PID" != "-" ] && [ -n "$PID" ]; then
            echo -e "  Running:   ${GREEN}Yes${NC} (PID: $PID)"
        else
            echo -e "  Running:   ${YELLOW}Loaded but not running${NC}"
        fi
    else
        echo -e "  Running:   ${RED}No${NC}"
    fi
}

cmd_install() {
    info "Installing services..."
    ensure_logs_dir

    install_service "live transcription" "$LIVE_PLIST_SRC" "$LIVE_PLIST_DST"
    install_service "frontend" "$FRONTEND_PLIST_SRC" "$FRONTEND_PLIST_DST"
    install_service "copilot channel" "$COPILOT_PLIST_SRC" "$COPILOT_PLIST_DST"
    install_service "copilot supervisor" "$SUPERVISOR_PLIST_SRC" "$SUPERVISOR_PLIST_DST"

    info "Services installed. Use './recog-service.sh start' to start them."
    info "Logs will be written to: $LOG_DIR/"
}

cmd_uninstall() {
    info "Uninstalling services..."

    uninstall_service "live transcription" "$LIVE_PLIST_DST"
    uninstall_service "frontend" "$FRONTEND_PLIST_DST"
    uninstall_service "copilot channel" "$COPILOT_PLIST_DST"
    uninstall_service "copilot supervisor" "$SUPERVISOR_PLIST_DST"
    kill_copilot_tmux

    info "Services uninstalled."
}

cmd_start() {
    info "Starting services..."
    ensure_logs_dir

    start_service "$LIVE_PLIST_NAME" "$LIVE_PLIST_SRC" "$LIVE_PLIST_DST"
    start_service "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_SRC" "$FRONTEND_PLIST_DST"
    start_service "$COPILOT_PLIST_NAME" "$COPILOT_PLIST_SRC" "$COPILOT_PLIST_DST"
    start_service "$SUPERVISOR_PLIST_NAME" "$SUPERVISOR_PLIST_SRC" "$SUPERVISOR_PLIST_DST"

    sleep 1
    cmd_status

    echo ""
    info "Frontend available at: http://127.0.0.1:9080"
}

cmd_stop() {
    info "Stopping services..."

    stop_all_services
    kill_copilot_tmux

    info "Services stopped."
}

cmd_restart() {
    # NOTE: intentionally does NOT go through cmd_stop — a mid-meeting restart
    # must keep the live copilot tmux session (the supervisor re-adopts it on
    # reconnect). Only cmd_stop/cmd_uninstall reap it via kill_copilot_tmux.
    info "Stopping services..."
    stop_all_services
    sleep 1
    cmd_start
}

cmd_restart_web() {
    restart_one "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_SRC" "$FRONTEND_PLIST_DST" \
        "frontend service" "Available at: http://127.0.0.1:9080"
}

cmd_restart_live() {
    restart_one "$LIVE_PLIST_NAME" "$LIVE_PLIST_SRC" "$LIVE_PLIST_DST" \
        "live transcription"
}

cmd_restart_copilot() {
    restart_one "$COPILOT_PLIST_NAME" "$COPILOT_PLIST_SRC" "$COPILOT_PLIST_DST" \
        "copilot channel daemon" "(http://127.0.0.1:8767/mcp)"
}

cmd_restart_supervisor() {
    restart_one "$SUPERVISOR_PLIST_NAME" "$SUPERVISOR_PLIST_SRC" "$SUPERVISOR_PLIST_DST" \
        "copilot supervisor daemon"
}

cmd_status() {
    echo "=== Service Status ==="
    echo ""

    check_service_status "$LIVE_PLIST_NAME" "$LIVE_PLIST_DST" "Live Transcription"
    echo ""
    check_service_status "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_DST" "Frontend"
    echo ""
    check_service_status "$COPILOT_PLIST_NAME" "$COPILOT_PLIST_DST" "Copilot Channel"
    echo ""
    check_service_status "$SUPERVISOR_PLIST_NAME" "$SUPERVISOR_PLIST_DST" "Copilot Supervisor"

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
    #      our HTTP MCP server itself. Normally launchd keeps it up via
    #      `com.recog.copilot-channel` (installed by `./recog-service.sh
    #      install`); we still probe the port here as a sanity check, and
    #      try to (re)load the plist if the probe fails.
    #   2. CWD must be the recog project dir so Claude reads the
    #      project-scope mcpServers entry (`copilot` → http://127.0.0.1:8767/mcp).
    #      Launching from $HOME silently produces "no MCP server configured
    #      with that name" because the project entry is invisible there.
    if probe_copilot_daemon; then
        info "Copilot channel daemon is up at http://127.0.0.1:8767/mcp"
    elif [ ! -f "$COPILOT_PLIST_SRC" ]; then
        warn "Copilot channel daemon is NOT responding at http://127.0.0.1:8767/mcp"
        warn "Plist not found at $COPILOT_PLIST_SRC — start manually:"
        warn "  cd $SCRIPT_DIR && ./venv-gigaam/bin/python -m recog.copilot_channel"
        warn ""
        warn "Launching Claude Code anyway — channel push will be a no-op until the daemon comes up."
    else
        warn "Copilot channel daemon is NOT responding at http://127.0.0.1:8767/mcp"
        warn "Attempting to (re)load $COPILOT_PLIST_NAME via launchd..."
        start_service "$COPILOT_PLIST_NAME" "$COPILOT_PLIST_SRC" "$COPILOT_PLIST_DST"
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
    # (recog/copilot_supervisor.py:build_claude_command) — built-ins + copilot
    # only, no user MCP servers. URL overridable via RECOG_COPILOT_MCP_URL.
    local mcp_url="${RECOG_COPILOT_MCP_URL:-http://127.0.0.1:8767/mcp}"
    local mcp_config="{\"mcpServers\":{\"copilot\":{\"type\":\"http\",\"url\":\"$mcp_url\"}}}"

    if [ "${RECOG_NO_AUTO_CONFIRM:-0}" = "1" ] || ! command -v expect > /dev/null 2>&1; then
        if [ "${RECOG_NO_AUTO_CONFIRM:-0}" != "1" ]; then
            warn "expect(1) not found — falling back to interactive launch (manual Enter needed)"
        fi
        exec claude --strict-mcp-config --mcp-config "$mcp_config" --dangerously-load-development-channels server:copilot $extra_dirs_args $agent_args
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
    exec env TERM=xterm-256color expect $expect_debug_arg -c "
        set timeout 30
        log_user 1
        spawn -noecho claude --strict-mcp-config --mcp-config {$mcp_config} --dangerously-load-development-channels server:copilot $extra_dirs_args $agent_args
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
    echo "  install         - Install and enable all services"
    echo "  uninstall       - Stop and remove all services"
    echo "  start           - Start all services"
    echo "  stop            - Stop all services"
    echo "  restart         - Restart all services"
    echo "  restart-web        - Restart frontend only"
    echo "  restart-live       - Restart live transcription only"
    echo "  restart-copilot    - Restart copilot channel daemon only"
    echo "  restart-supervisor - Restart copilot supervisor daemon only"
    echo "  status             - Show status of all services"
    echo "  debug              - Show debug info (participants, speaker, mics)"
    echo "  logs               - Tail live stderr logs"
    echo "  logs-out           - Tail live stdout logs (transcripts)"
    echo "  logs-web           - Tail frontend logs"
    echo "  logs-copilot       - Tail copilot channel daemon logs"
    echo "  logs-supervisor    - Tail copilot supervisor daemon logs"
    echo "  claude             - Launch Claude Code wired to the Copilot channel"
    echo ""
    echo "Services:"
    echo "  - Live transcription: mp4_notes.py live --screencapture --ktalk"
    echo "  - Frontend: http://127.0.0.1:9080"
    echo "  - Copilot channel daemon: python -m recog.copilot_channel (http://127.0.0.1:8767/mcp)"
    echo "  - Copilot supervisor: python -m recog.copilot_supervisor (auto-spawns Claude per meeting)"
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
