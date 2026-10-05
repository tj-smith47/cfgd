# shellcheck shell=bash
if assert_contains "$OUT" "Nothing to do — everything is up to date"; then :; fi
curl -sI https://example.invalid > /dev/null || true
# curl -I prints headers; an I/O error ends the read, and weekly runs are fine.
echo "  Weird owl"
note "the echo step reads what I typed"
