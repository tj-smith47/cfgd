# shellcheck shell=bash
# A helper whose sleep is the interval of a loop with a deadline.
wait_for_thing() {
    local deadline=$((SECONDS + 30))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if kubectl get pod probe > /dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}

one_liner() { echo "no wait here"; }
