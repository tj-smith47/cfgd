# shellcheck shell=bash
# A helper that pads with a fixed sleep and checks no deadline.
settle() {
    kubectl get pod probe > /dev/null 2>&1 || true
    sleep 3
}

# A helper with a deadline, so only settle above is named.
wait_for_thing() {
    local deadline=$((SECONDS + 30))
    until kubectl get pod probe > /dev/null 2>&1; do
        [ "$SECONDS" -lt "$deadline" ] || return 1
        sleep 1
    done
}
