#!/bin/sh
# cfgd package postinstall — register the daemon unit without starting it.
# Enable (so it comes up on next boot) but never start now: the operator must
# review /etc/cfgd/config.yaml before cfgd begins reconciling machine state.
# Guarded so non-systemd hosts (Alpine/OpenRC, containers) are a clean no-op.
set -e

if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
    systemctl enable cfgd.service >/dev/null 2>&1 || true
fi

exit 0
