# shellcheck shell=bash
if [ -n "$A" ] && [ -n "$B" ]; then
    pass_test "E-07" # want-flag
fi
if [ -n "$A" ] && [ "$A" = "token" ]; then
    pass_test "E-08"
fi
