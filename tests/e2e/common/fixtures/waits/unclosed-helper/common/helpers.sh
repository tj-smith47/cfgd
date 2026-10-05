# shellcheck shell=bash
# A function whose closing brace is indented, so the walk cannot tell where it
# ends or which sleeps it holds.
wait_for_thing() {
    local deadline=$((SECONDS + 30))
    until kubectl get pod probe > /dev/null 2>&1; do
        [ "$SECONDS" -lt "$deadline" ] || return 1
        sleep 1
    done
    }
