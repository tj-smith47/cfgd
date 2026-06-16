#!/bin/sh
# cfgd package postremove — reload systemd after the unit file is gone so a
# removed unit doesn't linger as "not-found" in the manager's view.
set -e

if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi

exit 0
