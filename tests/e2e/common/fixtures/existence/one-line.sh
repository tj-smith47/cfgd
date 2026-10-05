# shellcheck shell=bash
if [ -n "$A" ]; then pass_test "E-18"; fi # want-exist
[ -n "$A" ] && pass_test "E-19" # want-exist
[ "$A" = "ok" ] && pass_test "E-20"
