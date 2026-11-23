#!/bin/sh
set -e
# dpkg also runs this during an upgrade; keep the file then.
case "${1:-}" in
    upgrade | failed-upgrade | abort-install | abort-upgrade | disappear) exit 0 ;;
esac
rm -rf /var/lib/recog
