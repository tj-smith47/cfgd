# shellcheck shell=bash
if [ -n "$X" ]; then
    pass_test "E-01" # want-exist
fi
if [[ -n $X ]]; then
    pass_test "E-02" # want-exist
fi
if test -n "$X"; then
    pass_test "E-03" # want-exist
fi
