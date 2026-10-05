# shellcheck shell=bash
# A second run_every, written in a suite script: the name is exempt in
# helpers.sh alone.
run_every() {
    (
        trap - EXIT
        while true; do
            "$@" || true
            sleep 30
        done
    ) &
}
run_every kubectl get pods
