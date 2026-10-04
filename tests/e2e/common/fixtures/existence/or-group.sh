# shellcheck shell=bash
if [ -n "$A" ] || [ "$B" = "True" ]; then
    pass_test "E-09" # want-flag
fi
if [ "$A" = "x" ] || [ "$B" = "True" ]; then
    pass_test "E-10"
fi
