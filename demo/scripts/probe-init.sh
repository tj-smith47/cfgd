#!/usr/bin/env bash
# Replay the tape's init inside the demo image with plain (non-TTY) output, so a
# failed action can be read as text instead of hunted for in video frames.
set -uo pipefail

OUT=${1:-/root/.cache/cfgd-debug/demo/probe-init.log}
mkdir -p "$(dirname "$OUT")"

docker run --rm cfgd-demo:latest bash -lc \
    'cfgd init --from https://github.com/tj-smith47/cfgd-config.git --apply-module nvim --yes' \
    >"$OUT" 2>&1
echo "INIT_EXIT=$?" >>"$OUT"
