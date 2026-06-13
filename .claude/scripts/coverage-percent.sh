#!/usr/bin/env bash
# Print the workspace coverage percentage from cobertura.xml's top-level
# line-rate. Default output matches the README badge exactly:
#   <percent>%        e.g. 92.3%
# With --raw, output is the unrounded percent with no suffix, for a precise
# floor comparison that a rounded badge value would mask (92.49 -> 92.5):
#   <percent>         e.g. 92.273323
# Single source of the line-rate extraction shared by the badge publisher
# (default form) and the coverage:gate floor check (--raw form).
set -euo pipefail

RAW=0
if [ "${1:-}" = "--raw" ]; then
  RAW=1
  shift
fi

XML="${1:?Usage: coverage-percent.sh [--raw] <cobertura.xml>}"

if [ ! -f "$XML" ]; then
  echo "::error::Coverage XML not found: $XML" >&2
  exit 1
fi

LINE_RATE=$(grep -oP -m1 'line-rate="\K[^"]+' "$XML")
if [ "$RAW" -eq 1 ]; then
  awk "BEGIN {printf \"%.6f\n\", $LINE_RATE * 100}"
else
  awk "BEGIN {printf \"%.1f%%\n\", $LINE_RATE * 100}"
fi
