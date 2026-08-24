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
# The tail starts at the moment the install finishes, so the whole payoff plays
# at 1:1: the 5s summary read, `source ~/.cfgd.env`, nvim's start, the 5s toast
# settle, the 4s hero hold, `:qa`, the screen restore, and the version line with
# its 8s hold — ~34s of scripted beats plus nvim's own variable start. 37 keeps
# margin over that. Too small and the summary or the hero hold falls into the
# compressed middle, which is exactly the ramp-too-late this value fixes.
TAIL=37
# The install is the substance of the demo, not dead air to skip past: at 12s a
# ~7m take ran ~32x and the package and bootstrap lines were unreadable smears.
# 26s halves that to ~15x, which is fast enough to stay a montage and slow
# enough to read what is being installed.
MIDDLE=26
# Output seconds each speed transition takes. A hard cut from 1:1 straight to
# ~15x reads as a glitch, not a montage; easing through sqrt(speed) for a few
# seconds on each side keeps the acceleration legible (1x → ~4x → ~15x → ~4x
# → 1x). The eases spend their seconds inside MIDDLE, so the GIF's total
# length does not move.
EASE=3

if [ ! -f "$RAW" ]; then
    echo "$RAW does not exist — record the take first." >&2
    exit 1
fi

dur=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$RAW")
mid_end=$(awk -v d="$dur" -v t="$TAIL" 'BEGIN { printf "%.3f", d - t }')
# The fast span's speed, solved so the whole compressed region (ease in + fast
# span + ease out) still plays in exactly MIDDLE output seconds with each ease
# running at sqrt(speed): with x = sqrt(speed), E = EASE and F = MIDDLE - 2E,
# the source span satisfies F*x^2 + 2E*x - span = 0, whose positive root is
# x = (-E + sqrt(E^2 + F*span)) / F.
speed=$(awk -v s="$HEAD" -v e="$mid_end" -v m="$MIDDLE" -v ez="$EASE" 'BEGIN {
    span = e - s; f = m - 2 * ez
    x = (-ez + sqrt(ez * ez + f * span)) / f
    printf "%.4f", x * x
}')
ease_speed=$(awk -v sp="$speed" 'BEGIN { printf "%.4f", sqrt(sp) }')
# Source-time boundaries of the two ease segments.
ease_in_end=$(awk -v h="$HEAD" -v ez="$EASE" -v es="$ease_speed" 'BEGIN { printf "%.3f", h + ez * es }')
ease_out_start=$(awk -v e="$mid_end" -v ez="$EASE" -v es="$ease_speed" 'BEGIN { printf "%.3f", e - ez * es }')

if ! awk -v s="$speed" 'BEGIN { exit (s > 1) ? 0 : 1 }'; then
    echo "Take is ${dur}s — too short to ramp with a ${HEAD}s head and ${TAIL}s tail." >&2
    exit 1
fi

# fps 50 divides 100 exactly, so every GIF frame delay is a whole 2-centisecond
# delay and playback does not drift against the recorded timing.
#
# stats_mode=diff weights the palette toward the pixels that actually move, so
# the long static editor holds stop spending colours the install log needs, and
# diff_mode=rectangle lets each frame store only its changed bounding box — on
# a terminal recording, where most of the screen is unchanged between frames,
# the two together are what keep a 1320x1000 canvas to a README-sized file.
#
# Two ffmpeg passes, not one split+palettegen+paletteuse graph: paletteuse's
# second input can't start consuming until palettegen has seen every frame, so
# a single-graph split has to buffer the entire ramped clip (85s at 50fps and
# 1320x1000 is thousands of full-resolution frames) while it waits — the
# palette pass OOM-killed at ~8GB RSS with the framerate this ramp now records
# at. Writing the palette to a file first lets pass two read it as a single
# static frame, so the video side streams through without ever queuing more
# than a few frames. Same stats_mode, same dither, same output — only the
# memory shape changes.
RAMP="[0:v]trim=0:${HEAD},setpts=PTS-STARTPTS[a];\
[0:v]trim=${HEAD}:${ease_in_end},setpts=(PTS-STARTPTS)/${ease_speed}[b];\
[0:v]trim=${ease_in_end}:${ease_out_start},setpts=(PTS-STARTPTS)/${speed}[c];\
[0:v]trim=${ease_out_start}:${mid_end},setpts=(PTS-STARTPTS)/${ease_speed}[d];\
[0:v]trim=${mid_end},setpts=PTS-STARTPTS[e];\
[a][b][c][d][e]concat=n=5:v=1:a=0[v];\
[v]fps=50[vf]"

PALETTE=demo/.out/palette.png
trap 'rm -f "$PALETTE"' EXIT

ffmpeg -y -loglevel error -i "$RAW" -filter_complex "\
${RAMP};[vf]palettegen=max_colors=256:stats_mode=diff" "$PALETTE"

ffmpeg -y -loglevel error -i "$RAW" -i "$PALETTE" -filter_complex "\
${RAMP};[vf][1:v]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle" "$OUT"

echo "Wrote $OUT ($(du -h "$OUT" | cut -f1), ${dur}s take ramped ${speed}x in the middle, ${ease_speed}x eases)"
