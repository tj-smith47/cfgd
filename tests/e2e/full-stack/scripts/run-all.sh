#!/usr/bin/env bash
# run-all.sh for full-stack tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-fullstack-env.sh"

# Order matters: health first, fleet before drift, CSI after fleet
source "$SCRIPT_DIR/test-health.sh"
source "$SCRIPT_DIR/test-fleet.sh"
source "$SCRIPT_DIR/test-drift-lifecycle.sh"
source "$SCRIPT_DIR/test-csi.sh"
source "$SCRIPT_DIR/test-kubectl-plugin.sh"
source "$SCRIPT_DIR/test-debug.sh"
source "$SCRIPT_DIR/test-helm.sh"

print_summary "Full-Stack Tests"
