#!/usr/bin/env bash
# Print the workspace coverage percentage as it appears in the README badge,
# extracted from cobertura.xml's top-level line-rate. One line of output:
#   <percent>%
# Same formula the badge publisher uses; downstream tools can grep this.
set -euo pipefail

XML="${1:?Usage: coverage-percent.sh <cobertura.xml>}"

if [ ! -f "$XML" ]; then
  echo "::error::Coverage XML not found: $XML" >&2
  exit 1
fi

LINE_RATE=$(grep -oP -m1 'line-rate="\K[^"]+' "$XML")
awk "BEGIN {printf \"%.1f%%\n\", $LINE_RATE * 100}"
