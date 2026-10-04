# shellcheck shell=bash
if [ -n "$X" ]; then
    pass_test "E-01" # want-flag
fi
if [[ -n $X ]]; then
    pass_test "E-02" # want-flag
fi
if test -n "$X"; then
    pass_test "E-03" # want-flag
fi
