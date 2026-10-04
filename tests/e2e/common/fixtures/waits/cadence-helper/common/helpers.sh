# shellcheck shell=bash
# The background cadence, which the walk lists under Cadences.
run_every() {
    local interval="$1"
    shift
    (
        trap - EXIT
        while true; do
            "$@" || true
            sleep "$interval"
        done
    ) &
}
