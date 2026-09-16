#!/usr/bin/env bash
# run-all.sh for full-stack tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# The real binary runs as the invoking user; keep it out of that user's home.
source "$SCRIPT_DIR/../../common/scratch-home.sh"
source "$SCRIPT_DIR/setup-fullstack-env.sh"

# Order matters: health first, fleet before drift, CSI after fleet
source "$SCRIPT_DIR/test-health.sh"
source "$SCRIPT_DIR/test-fleet.sh"
source "$SCRIPT_DIR/test-drift-lifecycle.sh"
source "$SCRIPT_DIR/test-csi.sh"
source "$SCRIPT_DIR/test-oci-e2e.sh"
source "$SCRIPT_DIR/test-kubectl-plugin.sh"
source "$SCRIPT_DIR/test-debug.sh"
source "$SCRIPT_DIR/test-helm.sh"

SUMMARY_RC=0
print_summary "Full-Stack Tests" || SUMMARY_RC=1
assert_real_config_dir_unchanged || SUMMARY_RC=1
exit "$SUMMARY_RC"
