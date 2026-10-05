# shellcheck shell=bash
# A wait on a daemon's own interval, which is what the case measures.
start_daemon
sleep 5 # sleep-ok: the daemon under test reconciles every 5s
count_reconciles
