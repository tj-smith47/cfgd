#!/usr/bin/env bash
# Print the rate a take was really captured at, in frames per second.
#
# A tape asks for a rate (`Set Framerate`); a take achieves whatever the host
# could screenshot while a package install saturated it, and no take in this set
# reaches its declared rate. Demuxing at the DECLARED rate therefore plays the
# session back faster than it ran — the hero take missed 50 by 21%, so its
# 8-second closing hold reached the viewer as 6.1 seconds. The capture rate is
# recoverable from the frames themselves, which carry the wall clock in their
# mtimes, so nothing has to be recorded alongside them or kept in step by hand.
set -euo pipefail

FRAMES=${1:?usage: capture-rate.sh <frames-dir>}

# N frames span N-1 intervals: the first frame's timestamp is the start of the
# take, not the end of a frame.
read -r first last count < <(
    find "$FRAMES" -maxdepth 1 -name 'frame-text-*.png' -printf '%T@\n' |
        awk 'NR==1 { min=$1; max=$1 }
             { if ($1 < min) min=$1; if ($1 > max) max=$1 }
             END { printf "%.3f %.3f %d\n", min, max, NR }'
)

if [ "${count:-0}" -lt 2 ]; then
    echo "$FRAMES holds ${count:-0} frames — a rate needs at least two." >&2
    exit 1
fi

awk -v first="$first" -v last="$last" -v n="$count" 'BEGIN {
    span = last - first
    if (span <= 0) {
        print "'"$FRAMES"' frames share one timestamp — the mtimes cannot time the take." > "/dev/stderr"
        exit 1
    }
    printf "%.3f\n", (n - 1) / span
}'
