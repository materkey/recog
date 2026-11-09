#!/bin/bash
# Registers once (the token is good for an hour and only needed then), then serves.
set -euo pipefail
cd "$HOME/actions-runner"
if [ ! -f .runner ]; then
  ./config.sh --unattended --url "$RUNNER_URL" --token "$RUNNER_TOKEN" \
    --name "$RUNNER_NAME" --labels recog --work _work --replace
fi
exec ./run.sh
