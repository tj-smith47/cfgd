# shellcheck shell=bash
if [ -n "$A" ] && \
   [ -n "$B" ]; then
    pass_test "E-16" # want-flag
fi
if [ -n "$A" ]
then
    pass_test "E-17" # want-flag
fi
