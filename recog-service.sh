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

    info "Services installed. Use './recog-service.sh start' to start them."
    info "Logs will be written to: $LOG_DIR/"
}

cmd_uninstall() {
    info "Uninstalling services..."

    uninstall_service "live transcription" "$LIVE_PLIST_DST"
    uninstall_service "frontend" "$FRONTEND_PLIST_DST"

    info "Services uninstalled."
}

cmd_start() {
    info "Starting services..."
    ensure_logs_dir

    start_service "$LIVE_PLIST_NAME" "$LIVE_PLIST_SRC" "$LIVE_PLIST_DST"
    start_service "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_SRC" "$FRONTEND_PLIST_DST"

    sleep 1
    cmd_status

    echo ""
    info "Frontend available at: http://127.0.0.1:9080"
}

cmd_stop() {
    info "Stopping services..."

    stop_service "$LIVE_PLIST_NAME" "$LIVE_PLIST_DST"
    stop_service "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_DST"

    info "Services stopped."
}

cmd_restart() {
    cmd_stop
    sleep 1
    cmd_start
}

cmd_restart_web() {
    info "Restarting frontend service..."
    stop_service "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_DST"
    sleep 1
    start_service "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_SRC" "$FRONTEND_PLIST_DST"
    info "Frontend restarted. Available at: http://127.0.0.1:9080"
}

cmd_restart_live() {
    info "Restarting live transcription service..."
    stop_service "$LIVE_PLIST_NAME" "$LIVE_PLIST_DST"
    sleep 1
    start_service "$LIVE_PLIST_NAME" "$LIVE_PLIST_SRC" "$LIVE_PLIST_DST"
    info "Live transcription restarted."
}

cmd_status() {
    echo "=== Service Status ==="
    echo ""

    check_service_status "$LIVE_PLIST_NAME" "$LIVE_PLIST_DST" "Live Transcription"
    echo ""
    check_service_status "$FRONTEND_PLIST_NAME" "$FRONTEND_PLIST_DST" "Frontend"

    echo ""
    echo "=== Logs ==="
    for log in live.stderr.log live.stdout.log frontend.stderr.log frontend.stdout.log; do
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

cmd_logs() {
    LOG_FILE="$LOG_DIR/live.stderr.log"
    if [ ! -f "$LOG_FILE" ]; then
        error "Log file not found: $LOG_FILE"
    fi
    tail -f "$LOG_FILE"
}

cmd_logs_out() {
    LOG_FILE="$LOG_DIR/live.stdout.log"
    if [ ! -f "$LOG_FILE" ]; then
        error "Log file not found: $LOG_FILE"
    fi
    tail -f "$LOG_FILE"
}

cmd_logs_web() {
    LOG_FILE="$LOG_DIR/frontend.stderr.log"
    if [ ! -f "$LOG_FILE" ]; then
        error "Log file not found: $LOG_FILE"
    fi
    tail -f "$LOG_FILE"
}

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
    #      our HTTP MCP server itself.
    #   2. CWD must be the recog project dir so Claude reads the
    #      project-scope mcpServers entry (`copilot` → http://127.0.0.1:8767/mcp).
    #      Launching from $HOME silently produces "no MCP server configured
    #      with that name" because the project entry is invisible there.
    if ! curl -sS --max-time 1 -X POST http://127.0.0.1:8767/mcp \
        -H "Content-Type: application/json" \
        -H "Accept: application/json, text/event-stream" \
        -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"recog-service","version":"0"}}}' \
        > /dev/null 2>&1; then
        warn "Copilot channel daemon is NOT responding at http://127.0.0.1:8767/mcp"
        warn "Start it in a separate terminal first:"
        warn "  cd $SCRIPT_DIR && ./venv-gigaam/bin/python -m recog.copilot_channel"
        warn ""
        warn "Launching Claude Code anyway — channel push will be a no-op until the daemon comes up."
    else
        info "Copilot channel daemon is up at http://127.0.0.1:8767/mcp"
    fi

    if ! command -v claude > /dev/null 2>&1; then
        error "claude CLI not found in PATH"
    fi

    info "Launching Claude Code (project: $SCRIPT_DIR, channel: server:copilot) ..."
    # cd before exec so project-scope mcpServers is read; --dangerously-load-development-channels
    # bypasses the Anthropic-curated channel allowlist for our local server entry.
    cd "$SCRIPT_DIR"
    exec claude --dangerously-load-development-channels server:copilot
}

cmd_help() {
    echo "recog-service.sh - Manage recog live transcription and frontend services"
    echo ""
    echo "Usage: $0 <command>"
    echo ""
    echo "Commands:"
    echo "  install      - Install and enable both services"
    echo "  uninstall    - Stop and remove both services"
    echo "  start        - Start both services"
    echo "  stop         - Stop both services"
    echo "  restart      - Restart both services"
    echo "  restart-web  - Restart frontend only"
    echo "  restart-live - Restart live transcription only"
    echo "  status       - Show status of both services"
    echo "  debug        - Show debug info (participants, speaker, mics)"
    echo "  logs         - Tail live stderr logs"
    echo "  logs-out     - Tail live stdout logs (transcripts)"
    echo "  logs-web     - Tail frontend logs"
    echo "  claude       - Launch Claude Code wired to the Copilot channel"
    echo ""
    echo "Services:"
    echo "  - Live transcription: mp4_notes.py live --screencapture --ktalk"
    echo "  - Frontend: http://127.0.0.1:9080"
    echo "  - Copilot channel daemon: ./venv-gigaam/bin/python -m recog.copilot_channel"
}

# Main
case "${1:-}" in
    install)      cmd_install ;;
    uninstall)    cmd_uninstall ;;
    start)        cmd_start ;;
    stop)         cmd_stop ;;
    restart)      cmd_restart ;;
    restart-web)  cmd_restart_web ;;
    restart-live) cmd_restart_live ;;
    status)       cmd_status ;;
    debug)        cmd_debug ;;
    logs)         cmd_logs ;;
    logs-out)     cmd_logs_out ;;
    logs-web)     cmd_logs_web ;;
    claude)       cmd_claude ;;
    help|--help|-h) cmd_help ;;
    *)
        cmd_help
        exit 1
        ;;
esac
