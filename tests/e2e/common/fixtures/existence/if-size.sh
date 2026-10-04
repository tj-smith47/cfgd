# shellcheck shell=bash
if [ -s "$OUT_FILE" ]; then
    pass_test "E-06" # want-flag
fi
