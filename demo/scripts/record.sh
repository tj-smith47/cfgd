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

# A failed tape leaves the previous run's mp4 untouched, and `task demo:gif`
# would then ramp that stale take into the README GIF without a word. Clearing
# it first makes "no file" the only thing a failed recording can leave behind.
rm -f "$RAW"

# vhs writes a PNG pair per recorded frame into $TMPDIR, which for a take this
# long is several hundred MB. On Linux /tmp is tmpfs, so every one of those
# frames is resident RAM competing with the headless chromium vhs screenshots
# through — and when this box ran short the kernel killed that browser mid-take,
# leaving vhs waiting on a screen that would never change again until its
# 60-minute ceiling. Putting the frames on disk drops the recording's largest
# claim on memory. A take killed that way leaves its scratch behind, so clear
# any before adding more.
export TMPDIR="$PWD/demo/.out/tmp"
rm -rf "$TMPDIR"
mkdir -p "$TMPDIR"

vhs "$TAPE"

# VHS v0.11.0 exits 0 when its `Output` is a directory, writing nothing at all,
# and a take that dies at its first Wait still leaves a few tens of KB behind.
# Neither failure is visible without checking the artifact itself.
if [ ! -f "$RAW" ]; then
    echo "$RAW was never written — the tape produced no recording." >&2
    exit 1
fi

# `wc -c`, not `stat`: the two platforms this repo is developed on spell stat's
# size flag differently (`-c %s` GNU, `-f %z` BSD/macOS).
size=$(wc -c <"$RAW")
if [ "$size" -lt 1000000 ]; then
    echo "$RAW is only ${size} bytes — the take aborted early." >&2
    exit 1
fi

echo "Recorded $RAW ($(du -h "$RAW" | cut -f1))"
