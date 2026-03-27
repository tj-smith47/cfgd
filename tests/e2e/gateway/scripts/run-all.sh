#!/usr/bin/env bash
# run-all.sh for gateway tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-gateway-env.sh"

# Cleanup trap: kill port-forward and delete ephemeral namespace
trap 'kill "$PF_PID" 2>/dev/null || true; cleanup_e2e' EXIT

# Disable set -e for the test body — individual test failures are tracked by
# fail_test/pass_test, and print_summary returns non-zero if any test failed.
set +e

source "$SCRIPT_DIR/test-health.sh"
source "$SCRIPT_DIR/test-enrollment.sh"
source "$SCRIPT_DIR/test-checkin.sh"
source "$SCRIPT_DIR/test-api.sh"
source "$SCRIPT_DIR/test-admin.sh"
source "$SCRIPT_DIR/test-streaming.sh"
source "$SCRIPT_DIR/test-dashboard.sh"

print_summary "Gateway Tests"
