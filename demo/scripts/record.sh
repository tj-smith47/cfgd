#!/usr/bin/env bash
# Run a VHS tape and prove it actually produced a take.
#
# Takes a tape basename ($1, default "init" so the hero chain is unchanged):
# records demo/<name>.tape. VHS reads its target from inside the tape, not
# from an argument to this script, so the output path this script watches is
# read straight out of the tape's own `Output "..."` line (the same
# extraction the font precondition already runs on `Set FontFamily`) instead
# of being reconstructed from $NAME — a tape and a hand-built path can drift
# from each other; a tape and its own declared Output line cannot.
set -euo pipefail

cd "$(dirname "$0")/../.."

NAME="${1:-init}"
TAPE="demo/${NAME}.tape"

if [ ! -f "$TAPE" ]; then
    echo "$TAPE does not exist." >&2
    exit 1
fi

RAW="$(sed -n 's/^Output "\(.*\)"$/\1/p' "$TAPE" | head -1)"
if [ -z "$RAW" ]; then
    echo "$TAPE declares no parseable \`Output \"...\"\` line — cannot know what it writes." >&2
    exit 1
fi

# A failed tape leaves the previous run's frames untouched, and `task demo:gif`
# would then ramp that stale take into the README GIF without a word. Clearing
# it first makes "no frames" the only thing a failed recording can leave
# behind. It is also what lets vhs land the take at all: it publishes the
# frames by renaming its scratch directory onto this path, and rename refuses a
# destination that already holds files.
rm -rf "$RAW"

# vhs writes a PNG pair per recorded frame into $TMPDIR, which for a take this
# long is several hundred MB. On Linux /tmp is tmpfs, so every one of those
# frames is resident RAM competing with the headless chromium vhs screenshots
# through — and when this box ran short the kernel killed that browser mid-take,
# leaving vhs waiting on a screen that would never change again until its
# 60-minute ceiling. Putting the frames on disk drops the recording's largest
# claim on memory. A take killed that way leaves its scratch behind, so clear
# any before adding more.
#
# Keeping the scratch under demo/.out is what makes the publishing rename work:
# a rename cannot cross filesystems, so a $TMPDIR on another device would leave
# the take in scratch and the output directory missing, silently — vhs ignores
# that error and still exits 0.
#
# One scratch per take, named after the tape: takes run side by side (the
# containerised tapes share nothing else), and a scratch shared between them is
# wiped by whichever take starts last, leaving the earlier ones writing frames
# into a directory that no longer exists.
export TMPDIR="$PWD/demo/.out/tmp/$(basename "$RAW")"
rm -rf "$TMPDIR"
mkdir -p "$TMPDIR"

vhs "$TAPE"

# vhs exits 0 whether or not it managed to publish the frames, so the only
# honest report is the directory itself.
if [ ! -d "$RAW" ]; then
    echo "$RAW was never written — the tape produced no recording." >&2
    exit 1
fi

# The floor is calibrated off a genuine early death, not off any particular
# tape's expected length: a take that dies at its first `Wait` stops recording
# almost immediately, however long or short the tape that was recording claims
# to run. Two seconds of frames is well under the shortest complete take
# (backup.tape's ~35s, all six beats near-instant) and well over anything an
# aborted one leaves, so a floor tuned to a scrolling multi-minute install take
# cannot reject a perfectly good short recording as if it had aborted.
#
# `find`, not a glob: a multi-minute take is tens of thousands of frames, more
# than a command line can hold.
frames=$(find "$RAW" -maxdepth 1 -name 'frame-text-*.png' | wc -l)
if [ "$frames" -lt 100 ]; then
    echo "$RAW holds only ${frames} frames — the take aborted early." >&2
    exit 1
fi

# The rate the take achieved, not the rate the tape asked for. The encoders
# demux at this rate so the GIF plays back at the speed the session really ran,
# which is why a shortfall is reported rather than refused: a take recorded
# while a package install saturates the host will always miss its declared rate,
# and its timing is still exact. The floor is about legibility instead — below
# it the frames are too sparse for a scrolling install log to read as motion.
rate=$(bash "$(dirname "$0")/capture-rate.sh" "$RAW")
declared=$(sed -n 's/^Set Framerate \([0-9]*\)$/\1/p' "$TAPE" | head -1)
if awk -v r="$rate" 'BEGIN { exit !(r < 20) }'; then
    echo "$RAW captured ${rate} fps — too sparse to read as motion. Re-record on an idle host." >&2
    exit 1
fi

echo "Recorded $RAW (${frames} frames, ${rate} fps captured of ${declared:-?} declared, $(du -sh "$RAW" | cut -f1))"
