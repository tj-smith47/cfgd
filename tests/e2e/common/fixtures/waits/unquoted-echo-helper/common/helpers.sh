# shellcheck shell=bash disable=SC2086,SC2154  # $deadline stays unquoted and unset here
# A helper whose only bracket naming $deadline is an echo's unquoted words.
settle() {
    echo waiting [ $deadline ]
    sleep 2
}
