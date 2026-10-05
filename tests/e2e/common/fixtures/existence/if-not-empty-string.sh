# shellcheck shell=bash
if [ "$X" != "" ]; then
    pass_test "E-04" # want-exist
fi
if [ "" != "$X" ]; then
    pass_test "E-05" # want-exist
fi
if [ "$X" != "[]" ] && [ "$X" != "null" ]; then
    pass_test "E-28" # want-exist
fi
if [ "$X" != "[1]" ]; then
    pass_test "E-29"
fi
