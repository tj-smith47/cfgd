# shellcheck shell=bash
if [ "$X" != "" ]; then
    pass_test "E-04" # want-flag
fi
if [ "" != "$X" ]; then
    pass_test "E-05" # want-flag
fi
if [ "$X" != "[]" ] && [ "$X" != "null" ]; then
    pass_test "E-28" # want-flag
fi
if [ "$X" != "[1]" ]; then
    pass_test "E-29"
fi
