#!/usr/bin/env bash
# Ramp the raw take into the README GIF: 1:1 at both ends, compressed in the
# middle.
#
# The typing and the editor beats have to play at real speed or the demo stops
# reading as a real session; only the install wait between them is compressed.
# HEAD and TAIL exceed the 1:1 spans the tape records (~14s and ~17s) so the
# ramp can never reach into them — see the trailing note in demo/init.tape.
set -euo pipefail

cd "$(dirname "$0")/../.."

RAW=demo/.out/raw.mp4
OUT=demo/cfgd-demo.gif
HEAD=17
TAIL=20
MIDDLE=12

if [ ! -f "$RAW" ]; then
    echo "$RAW does not exist — record the take first." >&2
    exit 1
fi

dur=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$RAW")
mid_end=$(awk -v d="$dur" -v t="$TAIL" 'BEGIN { printf "%.3f", d - t }')
speed=$(awk -v s="$HEAD" -v e="$mid_end" -v m="$MIDDLE" 'BEGIN { printf "%.4f", (e - s) / m }')

if ! awk -v s="$speed" 'BEGIN { exit (s > 1) ? 0 : 1 }'; then
    echo "Take is ${dur}s — too short to ramp with a ${HEAD}s head and ${TAIL}s tail." >&2
    exit 1
fi

# fps 10 divides 100 exactly, so every GIF frame delay is a whole centisecond
# and playback does not drift against the recorded timing.
ffmpeg -y -loglevel error -i "$RAW" -filter_complex "\
[0:v]trim=0:${HEAD},setpts=PTS-STARTPTS[a];\
[0:v]trim=${HEAD}:${mid_end},setpts=(PTS-STARTPTS)/${speed}[b];\
[0:v]trim=${mid_end},setpts=PTS-STARTPTS[c];\
[a][b][c]concat=n=3:v=1:a=0[v];\
[v]fps=10,scale=900:-1:flags=lanczos,split[s0][s1];\
[s0]palettegen=max_colors=128[p];\
[s1][p]paletteuse=dither=bayer:bayer_scale=3" "$OUT"

echo "Wrote $OUT ($(du -h "$OUT" | cut -f1), ${dur}s take ramped ${speed}x in the middle)"
