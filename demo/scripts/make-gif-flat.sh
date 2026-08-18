#!/usr/bin/env bash
# Encode a raw take into a GIF at 1:1, whole take, no ramp.
#
# For a per-surface tape (~30-40s by construction) there is no install wait
# to compress, so unlike make-gif.sh this never trims, speeds up, or
# concatenates — it plays every frame at the speed it was recorded. The
# encode settings themselves (fps, palettegen, paletteuse, the two-pass
# palette-to-file split, the ffmpeg/ffprobe gate) are copied verbatim from
# make-gif.sh; see that script's comments for why each one is what it is.
set -euo pipefail

# Both are used below (ffprobe for duration, ffmpeg for the palette encode);
# without this check a missing install fails deep inside the filter_complex
# pipeline with a cryptic "command not found" instead of a clear ask.
for bin in ffmpeg ffprobe; do
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "$bin is required to build the GIF and was not found on PATH — install ffmpeg (which provides both)." >&2
        exit 1
    fi
done

cd "$(dirname "$0")/../.."

NAME="${1:?usage: make-gif-flat.sh <name>}"
RAW="demo/.out/${NAME}.mp4"
OUT="demo/cfgd-${NAME}.gif"

if [ ! -f "$RAW" ]; then
    echo "$RAW does not exist — record the take first." >&2
    exit 1
fi

dur=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$RAW")

# fps 50 divides 100 exactly, so every GIF frame delay is a whole 2-centisecond
# delay and playback does not drift against the recorded timing.
#
# stats_mode=diff weights the palette toward the pixels that actually move, so
# long static holds don't spend colours the moving lines need, and
# diff_mode=rectangle lets each frame store only its changed bounding box.
#
# Two ffmpeg passes, not one split+palettegen+paletteuse graph: paletteuse's
# second input can't start consuming until palettegen has seen every frame, so
# a single-graph split has to buffer the whole clip in memory while it waits —
# see make-gif.sh's comment on the OOM this caused there. Writing the palette
# to a file first lets pass two read it as a single static frame, so the video
# side streams through without ever queuing more than a few frames.
FILTER="[0:v]fps=50[vf]"

PALETTE="demo/.out/${NAME}-palette.png"
trap 'rm -f "$PALETTE"' EXIT

ffmpeg -y -loglevel error -i "$RAW" -filter_complex "\
${FILTER};[vf]palettegen=max_colors=256:stats_mode=diff" "$PALETTE"

ffmpeg -y -loglevel error -i "$RAW" -i "$PALETTE" -filter_complex "\
${FILTER};[vf][1:v]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle" "$OUT"

echo "Wrote $OUT ($(du -h "$OUT" | cut -f1), ${dur}s take at 1:1)"
