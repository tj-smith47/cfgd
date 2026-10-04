# shellcheck shell=bash
if [ -n "$A" ]; then pass_test "E-18"; fi # want-flag
[ -n "$A" ] && pass_test "E-19" # want-flag
[ "$A" = "ok" ] && pass_test "E-20"
