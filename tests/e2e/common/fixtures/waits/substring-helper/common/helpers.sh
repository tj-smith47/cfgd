# shellcheck shell=bash
# A helper whose test names $entries, which only contains the word tries.
settle() {
    local entries=3
    [ "$entries" -gt 0 ] || return 1
    sleep "$entries"
}
