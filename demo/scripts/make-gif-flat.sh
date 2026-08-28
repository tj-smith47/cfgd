#!/usr/bin/env bash
# Encode a raw take into a GIF at 1:1, whole take, no ramp.
#
# For a per-surface tape (~30-40s by construction) there is no install wait
# to compress, so unlike make-gif.sh this never trims, speeds up, or
# concatenates — it plays every frame at the speed it was recorded. The
# encode settings themselves (fps, the frame-pair composite, palettegen,
# paletteuse, the two-pass palette-to-file split, the ffmpeg gate) are copied
# verbatim from make-gif.sh; see that script's comments for why each one is
# what it is.
set -euo pipefail

# ffmpeg is the only external binary this script runs; without this check a
# missing install fails deep inside the filter_complex pipeline with a cryptic
# "command not found" instead of a clear ask.
if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "ffmpeg is required to build the GIF and was not found on PATH — install it." >&2
    exit 1
fi

cd "$(dirname "$0")/../.."

NAME="${1:?usage: make-gif-flat.sh <name>}"
TAPE="demo/${NAME}.tape"
FRAMES="demo/.out/${NAME}"
OUT="demo/cfgd-${NAME}.gif"

if [ ! -d "$FRAMES" ]; then
    echo "$FRAMES does not exist — record the take first." >&2
    exit 1
fi

# VHS records two lossless PNG sequences per take, one holding the terminal
# text and one holding just the cursor, and composites them at encode time.
# Reading those directly is the whole point of this pipeline: an intermediate
# h264 mp4 is 4:2:0, and chroma subsampling smears exactly the thin coloured
# glyphs (check marks, drift arrows, accent headings) the demos exist to show,
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
dur=$(awk -v n="$frames" -v f="$SRC_FPS" 'BEGIN { printf "%.2f", n / f }')

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
# stats_mode=diff weights the palette toward the pixels that actually move, so
# long static holds don't spend colours the moving lines need (measured on the
# k8s take: it lands the success green 33 RGB units from its source, `full`
# 43 — a global 256-colour palette over antialiased text cannot do better, and
# a per-frame palette costs ~770 bytes a frame). diff_mode=rectangle lets each
# frame store only its changed bounding box. dither=none, because the
# background is one flat colour and dithering it spread noise over the whole
# canvas while spending palette entries on five near-black variants.
#
# Two ffmpeg passes, not one split+palettegen+paletteuse graph: paletteuse's
# second input can't start consuming until palettegen has seen every frame, so
# a single-graph split has to buffer the whole clip in memory while it waits —
# see make-gif.sh's comment on the OOM this caused there. Writing the palette
# to a file first lets pass two read it as a single static frame, so the video
# side streams through without ever queuing more than a few frames.
FILTER="[0][1]overlay[merged];\
[merged]pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2:${BG}[padded];\
[padded]fps=${FPS}[vf]"

INPUTS=(-framerate "$SRC_FPS" -i "$TEXT" -framerate "$SRC_FPS" -i "$CURSOR")

PALETTE="demo/.out/${NAME}-palette.png"
trap 'rm -f "$PALETTE"' EXIT

ffmpeg -y -loglevel error "${INPUTS[@]}" -filter_complex "\
${FILTER};[vf]palettegen=max_colors=256:stats_mode=diff" "$PALETTE"

ffmpeg -y -loglevel error "${INPUTS[@]}" -i "$PALETTE" -filter_complex "\
${FILTER};[vf][2:v]paletteuse=dither=none:diff_mode=rectangle" "$OUT"

echo "Wrote $OUT ($(du -h "$OUT" | cut -f1), ${frames} frames at ${SRC_FPS}fps = ${dur}s take at 1:1)"
