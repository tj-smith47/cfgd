# shellcheck shell=bash disable=SC2034  # the unread deadline is the shape under test
# A helper that sets a deadline and never tests it.
settle() {
    local deadline=$((SECONDS + 30))
    sleep 5
}
