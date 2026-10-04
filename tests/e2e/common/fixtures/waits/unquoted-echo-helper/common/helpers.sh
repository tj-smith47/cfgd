# shellcheck shell=bash disable=SC2086,SC2154  # the echo must hold $deadline unquoted, and nothing here sets it
# A helper whose only bracket naming $deadline is an echo's unquoted words.
settle() {
    echo waiting [ $deadline ]
    sleep 2
}
