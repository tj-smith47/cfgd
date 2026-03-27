#!/usr/bin/env bash
# run-all.sh for operator tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-operator-env.sh"

source "$SCRIPT_DIR/test-crds.sh"
source "$SCRIPT_DIR/test-machineconfig.sh"
source "$SCRIPT_DIR/test-configpolicy.sh"
source "$SCRIPT_DIR/test-driftalert.sh"
source "$SCRIPT_DIR/test-module.sh"
source "$SCRIPT_DIR/test-clusterconfigpolicy.sh"
source "$SCRIPT_DIR/test-webhooks.sh"
source "$SCRIPT_DIR/test-oci.sh"
source "$SCRIPT_DIR/test-lifecycle.sh"

print_summary "Operator Tests"
