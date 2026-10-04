# shellcheck shell=bash
# A helper whose test names $tries_left, which only starts with the word tries.
settle() {
    local tries_left=3
    [ "$tries_left" -gt 0 ] || return 1
    sleep "$tries_left"
}
