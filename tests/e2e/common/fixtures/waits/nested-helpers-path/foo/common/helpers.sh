# shellcheck shell=bash
# A deadline helper in a file named common/helpers.sh one level below the
# walked tree, which is a suite script to the walk.
wait_for_thing() {
    local deadline=$((SECONDS + 30))
    while [ "$SECONDS" -lt "$deadline" ]; do
        kubectl get pod probe > /dev/null 2>&1 && return 0
        sleep 1
    done
    return 1
}
