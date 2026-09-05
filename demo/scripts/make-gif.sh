#!/usr/bin/env bash
# Ramp the raw take into the README GIF: 1:1 at both ends, compressed in the
# middle.
#
# The typing and the editor beats have to play at real speed or the demo stops
# reading as a real session; only the install wait between them is compressed.
# HEAD and TAIL each exceed the 1:1 span the tape records at that end, so the
# ramp can never reach into them — the per-knob comments below state how each
# span is sized, and the trailing note in demo/init.tape states the rule.
set -euo pipefail

# ffmpeg is the only external binary this script runs; without this check a
# missing install fails deep inside the filter_complex pipeline with a cryptic
# "command not found" instead of a clear ask.
if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "ffmpeg is required to build the GIF and was not found on PATH — install it." >&2
    exit 1
fi

cd "$(dirname "$0")/../.."

TAPE=demo/init.tape
FRAMES=demo/.out/raw
OUT=demo/cfgd-demo.gif
# The 1:1 opening is ~11s of scripted beats — two typed commands at 50ms, the
# 700ms pause before Enter, the 3s nvim glance — plus however long the
# container needs to reach a prompt, which is unscripted and swings with the
# page cache. 22 clears a cold start; on a warm take the slack is spent
# playing the first install lines at 1:1, which costs GIF seconds but can
# never truncate the typing the demo opens on.
HEAD=22
# The tail starts at the moment the install finishes, so the whole payoff plays
# at 1:1: the 5s summary read, `source ~/.cfgd.env`, nvim's start, the 12s toast
# settle, the 4s hero hold, `:qa`, the screen restore, and the version line with
# its 8s hold — 34.5s to 37.0s measured across takes, the spread being nvim's
# own variable start. 41 covers the LONGEST payoff seen with margin; the
# margin is paid on a short take as a few seconds of 1:1 spinner before the
# apply settles, which is the right side to err on — at 45 the boundary
# landed ~7s early and the GIF sat on a frozen install log. Too small and the
# summary or the hero hold falls into the compressed middle.
TAIL=41
# The install is the substance of the demo, not dead air to skip past: at 12s
# the middle ran fast enough that the package and bootstrap lines were
# unreadable smears. 26s halves that rate, which is fast enough to stay a
# montage and slow enough to read what is being installed. The ramp factor the
# take actually got is printed by this script when it finishes.
MIDDLE=26
# Output seconds each speed transition takes. A hard cut from 1:1 straight to
# ~15x reads as a glitch, not a montage; easing through sqrt(speed) for a few
# seconds on each side keeps the acceleration legible (1x → ~4x → ~15x → ~4x
# → 1x). The eases spend their seconds inside MIDDLE, so the GIF's total
# length does not move.
EASE=3

if [ ! -d "$FRAMES" ]; then
    echo "$FRAMES does not exist — record the take first." >&2
    exit 1
fi

# VHS records two lossless PNG sequences per take, one holding the terminal
# text and one holding just the cursor, and composites them at encode time.
# Reading those directly is the whole point of this pipeline: an intermediate
# h264 mp4 is 4:2:0, and chroma subsampling smears exactly the thin coloured
# glyphs (check marks, drift arrows, accent headings) the demo exists to show,
# before the palette pass ever sees them.
TEXT="${FRAMES}/frame-text-%05d.png"
CURSOR="${FRAMES}/frame-cursor-%05d.png"
frames=$(find "$FRAMES" -maxdepth 1 -name 'frame-text-*.png' | wc -l)
if [ "$frames" -eq 0 ]; then
    echo "$FRAMES holds no frame-text-*.png frames — the take produced no recording." >&2
    exit 1
fi

# fps 50 divides 100 exactly, so every GIF frame delay is a whole 2-centisecond
# delay and playback does not drift against the recorded timing. That is the
# OUTPUT rate only. The frames are demuxed at the rate the take really achieved,
# measured off the frames' own mtimes: a take recorded while a package install
# saturates the host misses the rate its tape declared, and demuxing at the
# declared rate would replay the session faster than it ran.
FPS=50
SRC_FPS=$(bash "$(dirname "$0")/capture-rate.sh" "$FRAMES")
dur=$(awk -v n="$frames" -v f="$SRC_FPS" 'BEGIN { printf "%.3f", n / f }')
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

# The frames are the bare terminal, with none of the surrounding padding the
# tape asks for, so the canvas has to be rebuilt here. Both numbers are read
# out of the tape rather than restated, because a hand-kept copy drifts from
# the `Set Width`/`Set Height` VHS actually recorded at.
W="$(sed -n 's/^Set Width \([0-9]*\)$/\1/p' "$TAPE" | head -1)"
H="$(sed -n 's/^Set Height \([0-9]*\)$/\1/p' "$TAPE" | head -1)"
if [ -z "$W" ] || [ -z "$H" ]; then
    echo "$TAPE declares no parseable \`Set Width\`/\`Set Height\` — cannot know what canvas to pad to." >&2
    exit 1
fi

# The padding colour is sampled from the recording itself instead of being
# hardcoded, so a tape that sets its own theme pads in that theme's background
# rather than in the default's.
BG="#$(ffmpeg -v error -i "${FRAMES}/frame-text-00001.png" -vf crop=1:1:0:0 -f rawvideo -pix_fmt rgb24 - | od -An -tx1 | tr -d ' \n')"

# `pad` only, no scale: VHS's own encoder resizes the frames ~1% to fit the
# canvas inside the padding, and that resample softens every glyph edge for no
# gain. Padding the frames at native size onto the same WxH canvas keeps the
# recorded pixels untouched and the GIF the size the tape declares.
#
# The composite is built once and split five ways because a filter output can
# only be consumed once, where the single mp4 input the trims used to read from
# could be referenced five times directly. The trims themselves still cut on
# source SECONDS: the sequences are demuxed at 50fps, so every timestamp the
# ramp math produces means the same instant it always did.
#
# stats_mode=diff weights the palette toward the pixels that actually move, so
# the long static editor holds stop spending colours the install log needs
# (measured against `full` in make-gif-flat.sh: diff lands text colours
# closer), and diff_mode=rectangle lets each frame store only its changed
# bounding box — on a terminal recording, where most of the screen is
# unchanged between frames, that is what keeps the canvas to a
# README-sized file. dither=none: the background is one flat colour, and
# dithering it is noise over the whole canvas.
#
# Two ffmpeg passes, not one split+palettegen+paletteuse graph: paletteuse's
# second input can't start consuming until palettegen has seen every frame, so
# a single-graph split has to buffer the entire ramped clip (85s at 50fps and
# a full-screen canvas is thousands of full-resolution frames) while it waits — the
# palette pass OOM-killed at ~8GB RSS with the framerate this ramp now records
# at. Writing the palette to a file first lets pass two read it as a single
# static frame, so the video side streams through without ever queuing more
# than a few frames. Same stats_mode, same dither, same output — only the
# memory shape changes.
RAMP="[0][1]overlay[merged];\
[merged]pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2:${BG}[base];\
[base]split=5[s0][s1][s2][s3][s4];\
[s0]trim=0:${HEAD},setpts=PTS-STARTPTS[a];\
[s1]trim=${HEAD}:${ease_in_end},setpts=(PTS-STARTPTS)/${ease_speed}[b];\
[s2]trim=${ease_in_end}:${ease_out_start},setpts=(PTS-STARTPTS)/${speed}[c];\
[s3]trim=${ease_out_start}:${mid_end},setpts=(PTS-STARTPTS)/${ease_speed}[d];\
[s4]trim=${mid_end},setpts=PTS-STARTPTS[e];\
[a][b][c][d][e]concat=n=5:v=1:a=0[v];\
[v]fps=${FPS}[vf]"

INPUTS=(-framerate "$SRC_FPS" -i "$TEXT" -framerate "$SRC_FPS" -i "$CURSOR")

PALETTE=demo/.out/palette.png
trap 'rm -f "$PALETTE"' EXIT

ffmpeg -y -loglevel error "${INPUTS[@]}" -filter_complex "\
${RAMP};[vf]palettegen=max_colors=256:stats_mode=diff" "$PALETTE"

ffmpeg -y -loglevel error "${INPUTS[@]}" -i "$PALETTE" -filter_complex "\
${RAMP};[vf][2:v]paletteuse=dither=none:diff_mode=rectangle" "$OUT"

echo "Wrote $OUT ($(du -h "$OUT" | cut -f1), ${frames} frames at ${SRC_FPS}fps = ${dur}s take ramped ${speed}x in the middle, ${ease_speed}x eases)"
