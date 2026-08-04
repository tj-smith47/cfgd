#!/usr/bin/env bash
# Run the VHS tape and prove it actually produced a take.
set -euo pipefail

cd "$(dirname "$0")/../.."

TAPE=demo/init.tape
RAW=demo/.out/raw.mp4

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
