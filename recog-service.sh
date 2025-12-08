#!/bin/bash
#
# recog-service.sh - Manage recog live transcription service
#
# Usage:
#   ./recog-service.sh install   - Install and enable the service
#   ./recog-service.sh uninstall - Stop and remove the service
#   ./recog-service.sh start     - Start the service
#   ./recog-service.sh stop      - Stop the service
#   ./recog-service.sh restart   - Restart the service
#   ./recog-service.sh status    - Show service status
#   ./recog-service.sh logs      - Tail logs (stderr)
#   ./recog-service.sh logs-out  - Tail logs (stdout)

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLIST_NAME="com.recog.live"
PLIST_SRC="$SCRIPT_DIR/$PLIST_NAME.plist"
PLIST_DST="$HOME/Library/LaunchAgents/$PLIST_NAME.plist"
LOG_DIR="$SCRIPT_DIR/logs"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

ensure_logs_dir() {
    mkdir -p "$LOG_DIR"
}

cmd_install() {
    info "Installing $PLIST_NAME..."

    ensure_logs_dir

    if [ ! -f "$PLIST_SRC" ]; then
        error "Plist file not found: $PLIST_SRC"
    fi

    # Copy plist to LaunchAgents
    cp "$PLIST_SRC" "$PLIST_DST"

    # Load the service
    launchctl load "$PLIST_DST" 2>/dev/null || true

    info "Service installed. Use './recog-service.sh start' to start it."
    info "Logs will be written to: $LOG_DIR/"
}

cmd_uninstall() {
    info "Uninstalling $PLIST_NAME..."

    # Unload if loaded
    launchctl unload "$PLIST_DST" 2>/dev/null || true

    # Remove plist
    rm -f "$PLIST_DST"

    info "Service uninstalled."
}

cmd_start() {
    info "Starting $PLIST_NAME..."

    ensure_logs_dir

    if [ ! -f "$PLIST_DST" ]; then
        warn "Service not installed. Installing first..."
        cmd_install
    fi

    launchctl start "$PLIST_NAME"

    sleep 1
    cmd_status
}

cmd_stop() {
    info "Stopping $PLIST_NAME..."
    launchctl stop "$PLIST_NAME" 2>/dev/null || true
    info "Service stopped."
}

cmd_restart() {
    cmd_stop
    sleep 1
    cmd_start
}

cmd_status() {
    echo "=== Service Status ==="

    # Check if plist is installed
    if [ -f "$PLIST_DST" ]; then
        echo -e "Installed: ${GREEN}Yes${NC}"
    else
        echo -e "Installed: ${RED}No${NC}"
        return
    fi

    # Check if running
    if launchctl list | grep -q "$PLIST_NAME"; then
        PID=$(launchctl list | grep "$PLIST_NAME" | awk '{print $1}')
        if [ "$PID" != "-" ] && [ -n "$PID" ]; then
            echo -e "Running:   ${GREEN}Yes${NC} (PID: $PID)"
        else
            echo -e "Running:   ${YELLOW}Loaded but not running${NC}"
        fi
    else
        echo -e "Running:   ${RED}No${NC}"
    fi

    # Show log file sizes
    echo ""
    echo "=== Logs ==="
    if [ -f "$LOG_DIR/live.stderr.log" ]; then
        SIZE=$(du -h "$LOG_DIR/live.stderr.log" | cut -f1)
        LINES=$(wc -l < "$LOG_DIR/live.stderr.log" | tr -d ' ')
        echo "stderr: $SIZE ($LINES lines)"
    fi
    if [ -f "$LOG_DIR/live.stdout.log" ]; then
        SIZE=$(du -h "$LOG_DIR/live.stdout.log" | cut -f1)
        LINES=$(wc -l < "$LOG_DIR/live.stdout.log" | tr -d ' ')
        echo "stdout: $SIZE ($LINES lines)"
    fi

    # Show last few log lines
    echo ""
    echo "=== Recent stderr (last 5 lines) ==="
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

cmd_help() {
    echo "recog-service.sh - Manage recog live transcription service"
    echo ""
    echo "Usage: $0 <command>"
    echo ""
    echo "Commands:"
    echo "  install   - Install and enable the service"
    echo "  uninstall - Stop and remove the service"
    echo "  start     - Start the service"
    echo "  stop      - Stop the service"
    echo "  restart   - Restart the service"
    echo "  status    - Show service status"
    echo "  logs      - Tail stderr logs"
    echo "  logs-out  - Tail stdout logs (transcripts)"
    echo ""
    echo "The service runs: mp4_notes.py live --screencapture --ktalk"
}

# Main
case "${1:-}" in
    install)   cmd_install ;;
    uninstall) cmd_uninstall ;;
    start)     cmd_start ;;
    stop)      cmd_stop ;;
    restart)   cmd_restart ;;
    status)    cmd_status ;;
    logs)      cmd_logs ;;
    logs-out)  cmd_logs_out ;;
    help|--help|-h) cmd_help ;;
    *)
        cmd_help
        exit 1
        ;;
esac
