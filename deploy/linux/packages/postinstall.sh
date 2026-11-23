#!/bin/sh
set -e
# Wakes recog-upgrade.path of every user with a running session.
mkdir -p /var/lib/recog
cat /usr/lib/recog/recog-release > /var/lib/recog/installed-release
