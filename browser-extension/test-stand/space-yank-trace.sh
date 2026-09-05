#!/bin/bash
# Attributes frontmost-app changes at the WindowServer level. Run in
# kitty while reproducing the yank; every activation prints two lines:
#
#   <CONN>[SetFrontProcess]: ... Making <private> <PSN> the front process ...
#   ... Deferring events from frontmost process PSN <PSN> (App Name) -> <pid: N>
#
# The hex CONN prefix is the WindowServer connection that REQUESTED the
# switch. Interpretation:
# - Chrome became front and CONN equals Chrome's own connection (the
#   same CONN appears when you activate Chrome by clicking it): Chrome
#   activated itself - the cause lives inside Chrome (page JS dialog,
#   an extension calling chrome.windows.update, notification click).
# - CONN belongs to Dock: the Space/app switch came from the system
#   (Mission Control, gesture, hot corner, app-switch behavior).
#
# Calibrate first: activate Chrome manually once and note its CONN.
# Ctrl+C to stop.

echo "$(date '+%H:%M:%S') space-yank-trace started; activate Chrome once manually to calibrate its CONN id"
log stream --style compact \
  --predicate 'subsystem == "com.apple.SkyLight.processes" AND category == "Focus" AND (composedMessage CONTAINS "front process" OR composedMessage CONTAINS "frontmost process")'
