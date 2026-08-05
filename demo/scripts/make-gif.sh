#!/usr/bin/env bash
# Ramp the raw take into the README GIF: 1:1 at both ends, compressed in the
# middle.
#
# The typing and the editor beats have to play at real speed or the demo stops
# reading as a real session; only the install wait between them is compressed.
# HEAD and TAIL exceed the 1:1 spans the tape records (~11s and ~14s) so the
# ramp can never reach into them — see the trailing note in demo/init.tape.
set -euo pipefail

# Both are used below (ffprobe for duration, ffmpeg for the ramp+palette
# encode); without this check a missing install fails deep inside the
# filter_complex pipeline with a cryptic "command not found" instead of a
# clear ask.
for bin in ffmpeg ffprobe; do
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "$bin is required to build the GIF and was not found on PATH — install ffmpeg (which provides both)." >&2
        exit 1
    fi
done

cd "$(dirname "$0")/../.."

RAW=demo/.out/raw.mp4
OUT=demo/cfgd-demo.gif
# The 1:1 opening is ~11s of scripted beats — two typed commands at 50ms, the
# 700ms pause before Enter, the 3s nvim glance — plus however long the
# container needs to reach a prompt, which is unscripted and swings with the
# page cache. 22 clears a cold start; on a warm take the slack is spent
# playing the first install lines at 1:1, which costs GIF seconds but can
# never truncate the typing the demo opens on.
HEAD=22
# The 1:1 closing is ~14s: the 6s hero hold, `:qa`, the beat the terminal
# takes to restore the primary screen, then the version line and its 6s hold.
# 18 keeps real margin over that. At 16 a slow screen restore pushed the hero
# hold into the compressed middle — the one beat that must never be sped up.
TAIL=18
# The install is the substance of the demo, not dead air to skip past: at 12s a
# ~7m take ran ~32x and the package and bootstrap lines were unreadable smears.
# 26s halves that to ~15x, which is fast enough to stay a montage and slow
# enough to read what is being installed.
MIDDLE=26

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
#
# stats_mode=diff weights the palette toward the pixels that actually move, so
# the long static editor holds stop spending colours the install log needs, and
# diff_mode=rectangle lets each frame store only its changed bounding box — on
# a terminal recording, where most of the screen is unchanged between frames,
# the two together are what keep a 1320x1000 canvas to a README-sized file.
ffmpeg -y -loglevel error -i "$RAW" -filter_complex "\
[0:v]trim=0:${HEAD},setpts=PTS-STARTPTS[a];\
[0:v]trim=${HEAD}:${mid_end},setpts=(PTS-STARTPTS)/${speed}[b];\
[0:v]trim=${mid_end},setpts=PTS-STARTPTS[c];\
[a][b][c]concat=n=3:v=1:a=0[v];\
[v]fps=10,split[s0][s1];\
[s0]palettegen=max_colors=128:stats_mode=diff[p];\
[s1][p]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle" "$OUT"

echo "Wrote $OUT ($(du -h "$OUT" | cut -f1), ${dur}s take ramped ${speed}x in the middle)"
