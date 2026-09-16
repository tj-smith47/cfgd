#!/usr/bin/env bash
# Build the concat-demuxer lists both encoders read, with every frame's display
# time capped so a hidden segment does not play.
#
# VHS writes no frames while a tape is inside `Hide`/`Show`. Demuxed on their
# mtimes (`-ts_from_file`), the frame written just before the tape hid
# therefore carries the whole hidden wall time as its display duration: on the
# sync take that was a black nvim alternate screen held for ~14 seconds, twice,
# and on every other tape a blank prompt held for however long its hidden setup
# ran. The mtimes are still what times the take — they are the only record of
# the rate the host actually screenshot at — so the fix is not to abandon them
# but to bound one frame's share of them.
#
# CAP is the ceiling. No take in this set holds a VISIBLE frame anywhere near
# it: measured over the recorded frame dirs, the largest gap between two frames
# of a visible stretch is 0.12s (author), 0.06s (drift) and 0.045s (k8s), all
# at capture rates of 45-48 fps. The k8s take's three hidden setup segments
# show as 0.84s gaps and the sync take's as ~13-14s ones, so the two
# populations are an order of magnitude apart and anything between 0.2s and 1s
# separates them. 0.5s takes the middle: four times the widest visible hold
# ever recorded, and still short enough that a capped frame reads as a beat
# rather than a pause.
CAP=0.5

set -euo pipefail

FRAMES=${1:?usage: frame-lists.sh <frames-dir> <out-dir>}
OUT=${2:?usage: frame-lists.sh <frames-dir> <out-dir>}

FRAMES=$(cd "$FRAMES" && pwd)
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)

# Sorted by filename, not by mtime: the zero-padded index IS the order VHS
# wrote them in, and two frames the host stamped in the same millisecond must
# not be allowed to swap.
find "$FRAMES" -maxdepth 1 -name 'frame-text-*.png' -printf '%T@ %f\n' |
    sort -k2,2 |
    awk -v frames="$FRAMES" -v out="$OUT" -v cap="$CAP" '
{ t[NR] = $1; f[NR] = $2 }
END {
    if (NR == 0) { print "no frames" > "/dev/stderr"; exit 1 }
    text = out "/text.ffconcat"
    cursor = out "/cursor.ffconcat"
    line = out "/timeline.tsv"
    print "ffconcat version 1.0" > text
    print "ffconcat version 1.0" > cursor
    total = 0
    for (i = 1; i <= NR; i++) {
        d = (i < NR) ? t[i + 1] - t[i] : prev
        if (d > cap || d <= 0) d = cap
        prev = d
        c = f[i]; sub(/frame-text-/, "frame-cursor-", c)
        printf "file %s/%s\nduration %.6f\n", frames, f[i], d > text
        printf "file %s/%s\nduration %.6f\n", frames, c, d > cursor
        # The wall-clock instant this frame was written, beside the instant it
        # plays at once the caps are applied. make-gif.sh reads it to move the
        # ramp boundaries it derives from the log onto the capped timeline.
        printf "%.6f\t%.6f\n", t[i], total > line
        total += d
    }
    # The concat demuxer ignores the final `duration` unless the file it
    # belongs to is named once more.
    c = f[NR]; sub(/frame-text-/, "frame-cursor-", c)
    printf "file %s/%s\n", frames, f[NR] > text
    printf "file %s/%s\n", frames, c > cursor
    printf "%.3f\n", total
}'
