# shellcheck shell=bash disable=SC2154  # only the message names $deadline, which nothing here sets
# A helper whose only bracketed deadline is inside a log message.
settle() {
    log "waiting [until $deadline]"
    sleep 2
}
