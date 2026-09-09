#!/usr/bin/env bash
# Ramp the raw take into the README GIF: 1:1 at both ends, compressed in the
# middle in two tiers — the plan and the package install at a pace that can
# still be followed, the module's own bootstrap scripts at montage pace.
#
# The typing and the editor beats have to play at real speed or the demo stops
# reading as a real session; only the install wait between them is compressed.
# HEAD and TAIL each exceed the 1:1 span the tape records at that end, so the
# ramp can never reach into them — the per-knob comments below state how each
# span is sized, and the trailing note in demo/init.tape states the rule. The
# boundary between the two tiers is not a knob: it is read off the take.
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
# The container's own output with a wall-clock stamp per line, kept by
# demo/scripts/record.sh beside the frames; the tier boundary is read from it.
LOG=demo/.out/init.log
OUT=demo/cfgd-demo.gif
# 1:1 seconds at the start: the opening typing, the plan tree (~12s on a take)
# and the apt index settling (~15s). Brew's own settle at ~26-28s rides the
# ~2x ease that follows; holding it at 1:1 (32, 29, 26) each read as waiting.
HEAD=20
# 1:1 seconds at the end, from the apply's rollup line: the summary read, the
# env source, nvim's start and hold, the quit and the version line. The tape's
# beats sum to ~23s; the margin covers nvim's variable start and lands as a
# few seconds of 1:1 install log before the rollup, the right side to err on.
TAIL=27
# Output seconds the plan-and-install region plays in: from HEAD to the moment
# the apply opens its scripts phase, ease-in included. Phase headings, package
# rows settling with their versions, the file deploys — this is the part of the
# install a viewer can actually follow, so it takes the readable end of a
# montage: ~85s of it in 20s is ~4.8x. One rate shared with the scripts below
# used to put it at ~7.6x, which rushed the rows a viewer was trying to read
# while still crawling over the scripts nobody reads line by line.
PLAN_OUT=20
# Output seconds the scripts region plays in: from the first post-apply script
# to the rollup, ease-out included. Plugin restores, parser compiles and Mason
# downloads are the longest span of the take and the least worth reading, so
# this is where the speed goes: ~87s in 12s is ~8.7x, the rate the whole middle
# used to share and the one place it was right.
SCRIPTS_OUT=12
# Output seconds each speed transition takes, at sqrt(that tier's speed). A
# hard cut from 1:1 straight to a montage reads as a glitch, not a ramp. The
# climb into the plan tier takes four seconds because it is the transition a
# viewer is watching closest: at ~2.2x the brew and apt rows still settle at a
# pace the eye can follow. The descent from the scripts tier back to 1:1 was
# right at three (~3x), so it keeps its own knob. The step between the two
# tiers takes no ease: ~4.8x to ~8.7x is a smaller jump than either ease
# already makes, over a log that is already a blur. The eases spend their
# seconds inside their tier's budget, so the GIF's length is exactly
# HEAD + PLAN_OUT + SCRIPTS_OUT + TAIL.
EASE_IN=4
EASE_OUT=3

if [ ! -d "$FRAMES" ]; then
    echo "$FRAMES does not exist — record the take first." >&2
    exit 1
fi
if [ ! -f "$LOG" ]; then
    echo "$LOG does not exist — record the take with demo/scripts/record.sh, which keeps it." >&2
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
# OUTPUT rate only. The frames are demuxed on their own mtimes (`ts_from_file`),
# never at a rate: vhs screenshots as fast as the host lets it, and on one take
# that swung between 26 and 47 fps as the install loaded the box. Demuxed at
# the take's average rate the fast stretches played slow, the slow ones fast,
# and a source second named by the log landed ten seconds away from the frame
# that showed it. On the mtimes every trim below cuts at the wall-clock second
# it names, the 1:1 ends play at the speed the session ran, and the take's
# length is the span of its mtimes. ffmpeg rebases the first frame to 0.
FPS=50
# One awk pass, not `sort | head`: under pipefail a `head` that closes early
# turns sort's SIGPIPE into this script's exit.
read -r first_frame dur < <(find "$FRAMES" -maxdepth 1 -name 'frame-text-*.png' -printf '%T@\n' |
    awk 'NR == 1 { lo = $1; hi = $1 } $1 < lo { lo = $1 } $1 > hi { hi = $1 } END { printf "%.3f %.3f\n", lo, hi - lo }')
mid_end=$(awk -v d="$dur" -v t="$TAIL" 'BEGIN { printf "%.3f", d - t }')

# The tier boundary is the source second the apply opened `Phase: Post-Scripts`.
# It moves with every take — the package install ahead of it swings tens of
# seconds with the mirrors — so a hand-kept value would be stale on every
# re-record. The container's log stamps each line with the host's clock and
# the frames carry the same clock in their mtimes, so the heading's stamp minus
# the first frame's mtime is the heading's source second, on the same timeline
# the trims below cut. `--yes` prints no preview tree, so the first line
# carrying the heading is the execution's; the colour escapes are stripped
# first because the heading paints in two theme slots. `LC_ALL=C` because the
# final-byte range `[@-~]` is a BYTE range only in the C locale — under a
# UTF-8 locale it collates, matches nothing, and the heading is never found.
heading_stamp=$(LC_ALL=C sed 's/\x1b\[[0-9;?]*[ -\/]*[@-~]//g' "$LOG" | grep -a -F -m1 'Phase: Post-Scripts' | cut -d' ' -f1 || true)
if [ -z "$heading_stamp" ]; then
    echo "$LOG never shows \`Phase: Post-Scripts\` — the take did not reach the module's scripts." >&2
    exit 1
fi
scripts_at=$(awk -v h="$(date -d "$heading_stamp" +%s.%N)" -v f="$first_frame" 'BEGIN { printf "%.3f", h - f }')
if ! awk -v s="$scripts_at" -v h="$HEAD" -v e="$mid_end" 'BEGIN { exit (s > h && s < e) ? 0 : 1 }'; then
    echo "The scripts phase opened at ${scripts_at}s, outside the ${HEAD}s..${mid_end}s middle — the take cannot ramp in two tiers." >&2
    exit 1
fi

# Each tier's speed, solved so its region (one ease at sqrt(speed) plus the
# flat span) plays in exactly its output budget: with x = sqrt(speed), E the
# ease and F = budget - E, the source span satisfies F*x^2 + E*x - span = 0,
# whose positive root is x = (-E + sqrt(E^2 + 4*F*span)) / (2*F).
tier_root() {
    awk -v span="$1" -v budget="$2" -v ez="$3" 'BEGIN {
        f = budget - ez
        printf "%.4f", (-ez + sqrt(ez * ez + 4 * f * span)) / (2 * f)
    }'
}
ease_in_speed=$(tier_root "$(awk -v s="$scripts_at" -v h="$HEAD" 'BEGIN { printf "%.3f", s - h }')" "$PLAN_OUT" "$EASE_IN")
plan_speed=$(awk -v x="$ease_in_speed" 'BEGIN { printf "%.4f", x * x }')
ease_out_speed=$(tier_root "$(awk -v s="$scripts_at" -v e="$mid_end" 'BEGIN { printf "%.3f", e - s }')" "$SCRIPTS_OUT" "$EASE_OUT")
scripts_speed=$(awk -v x="$ease_out_speed" 'BEGIN { printf "%.4f", x * x }')
# Source-time boundaries of the two ease segments.
ease_in_end=$(awk -v h="$HEAD" -v ez="$EASE_IN" -v es="$ease_in_speed" 'BEGIN { printf "%.3f", h + ez * es }')
ease_out_start=$(awk -v e="$mid_end" -v ez="$EASE_OUT" -v es="$ease_out_speed" 'BEGIN { printf "%.3f", e - ez * es }')

if ! awk -v p="$plan_speed" -v s="$scripts_speed" 'BEGIN { exit (p > 1 && s > 1) ? 0 : 1 }'; then
    echo "Take is ${dur}s with its scripts at ${scripts_at}s — too short to ramp with a ${HEAD}s head and ${TAIL}s tail." >&2
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
# The composite is built once and split six ways because a filter output can
# only be consumed once, where the single mp4 input the trims used to read from
# could be referenced six times directly. The trims cut on source SECONDS, the
# frames' own wall clock, so every boundary the ramp math produces is the
# instant the log or the tape measured.
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
# a single-graph split has to buffer the entire ramped clip (~90s at 50fps and
# a full-screen canvas is thousands of full-resolution frames) while it waits — the
# palette pass OOM-killed at ~8GB RSS with the framerate this ramp now records
# at. Writing the palette to a file first lets pass two read it as a single
# static frame, so the video side streams through without ever queuing more
# than a few frames. Same stats_mode, same dither, same output — only the
# memory shape changes.
RAMP="[0][1]overlay[merged];\
[merged]pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2:${BG}[base];\
[base]split=6[s0][s1][s2][s3][s4][s5];\
[s0]trim=0:${HEAD},setpts=PTS-STARTPTS[a];\
[s1]trim=${HEAD}:${ease_in_end},setpts=(PTS-STARTPTS)/${ease_in_speed}[b];\
[s2]trim=${ease_in_end}:${scripts_at},setpts=(PTS-STARTPTS)/${plan_speed}[c];\
[s3]trim=${scripts_at}:${ease_out_start},setpts=(PTS-STARTPTS)/${scripts_speed}[d];\
[s4]trim=${ease_out_start}:${mid_end},setpts=(PTS-STARTPTS)/${ease_out_speed}[e];\
[s5]trim=${mid_end},setpts=PTS-STARTPTS[f];\
[a][b][c][d][e][f]concat=n=6:v=1:a=0[v];\
[v]fps=${FPS}[vf]"

INPUTS=(-ts_from_file 2 -i "$TEXT" -ts_from_file 2 -i "$CURSOR")

PALETTE=demo/.out/palette.png
trap 'rm -f "$PALETTE"' EXIT

ffmpeg -y -loglevel error "${INPUTS[@]}" -filter_complex "\
${RAMP};[vf]palettegen=max_colors=256:stats_mode=diff" "$PALETTE"

ffmpeg -y -loglevel error "${INPUTS[@]}" -i "$PALETTE" -filter_complex "\
${RAMP};[vf][2:v]paletteuse=dither=none:diff_mode=rectangle" "$OUT"

echo "Wrote $OUT ($(du -h "$OUT" | cut -f1), ${frames} frames over a ${dur}s take; scripts opened at ${scripts_at}s; plan ${plan_speed}x after a ${ease_in_speed}x ease, scripts ${scripts_speed}x before a ${ease_out_speed}x ease)"
