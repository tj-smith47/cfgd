#!/usr/bin/env bash
# run-all.sh for operator tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# The real binary runs as the invoking user; keep it out of that user's home.
source "$SCRIPT_DIR/../../common/scratch-home.sh"
source "$SCRIPT_DIR/setup-operator-env.sh"

source "$SCRIPT_DIR/test-crds.sh"
source "$SCRIPT_DIR/test-machineconfig.sh"
source "$SCRIPT_DIR/test-configpolicy.sh"
source "$SCRIPT_DIR/test-driftalert.sh"
source "$SCRIPT_DIR/test-module.sh"
source "$SCRIPT_DIR/test-clusterconfigpolicy.sh"
source "$SCRIPT_DIR/test-backuppolicy.sh"
source "$SCRIPT_DIR/test-webhooks.sh"
source "$SCRIPT_DIR/test-oci.sh"
source "$SCRIPT_DIR/test-lifecycle.sh"

SUMMARY_RC=0
print_summary "Operator Tests" || SUMMARY_RC=1
assert_real_config_dir_unchanged || SUMMARY_RC=1
exit "$SUMMARY_RC"
