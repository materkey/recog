#!/bin/bash
# Logs frontmost-app transitions with timestamps. Run it in kitty while
# reproducing the "macOS pulls me back to Chrome's Space" issue: the
# moment of the yank shows up as a transition to "Google Chrome" that
# you did not initiate. Correlate the timestamp with focus-lab's log or
# the KTalk console ([KTalk Recog] entries).
#
# Uses lsappinfo (no Accessibility permissions needed). Ctrl+C to stop.

prev=""
echo "$(date '+%H:%M:%S') front-app-monitor started (poll 200ms)"
while true; do
  front=$(lsappinfo info -only name "$(lsappinfo front 2>/dev/null)" 2>/dev/null | sed 's/.*=//; s/"//g')
  if [[ -n "$front" && "$front" != "$prev" ]]; then
    echo "$(date '+%H:%M:%S') front app -> $front"
    prev="$front"
  fi
  sleep 0.2
done
