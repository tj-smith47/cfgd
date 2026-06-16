#!/bin/sh
# cfgd package preremove — stop and disable the unit on a real removal, but NOT
# during an upgrade (where the new package's postinstall re-enables it).
# Arg conventions differ by packager: deb passes "remove"/"purge"/"upgrade";
# rpm passes "0" (remove) / "1" (upgrade); apk passes none (removal context).
set -e

removing=0
case "${1:-}" in
    remove | purge | 0 | "") removing=1 ;;
esac

if [ "$removing" = 1 ] && [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
    systemctl disable --now cfgd.service >/dev/null 2>&1 || true
fi

exit 0
