# shellcheck shell=bash disable=SC2034  # the unread deadline is the shape under test
# A helper whose deadline is a $(( )) expansion and is never tested.
settle() {
    local deadline=$(( SECONDS + 30 ))
    sleep 5
}
