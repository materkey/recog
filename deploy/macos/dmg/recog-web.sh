#!/bin/sh
# The archived launchd unit has a fixed PATH. Only the Recog web process
# needs this copy of FFmpeg to play a segment from a recording.
set -eu
bin=$(CDPATH='' cd -P "$(/usr/bin/dirname "$0")" && pwd -P)
export PATH="$bin:${PATH:-/usr/bin:/bin}"
exec "$bin/recog-web-release" "$@"
