#!/usr/bin/env bash
# run-all.sh for gateway tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-gateway-env.sh"

export GW_SCRATCH=$(mktemp -d)

# Cleanup trap: kill port-forward and any device daemon a case left running,
# delete ephemeral namespace, remove scratch. DP_DAEMON_PID and DP_CONF are set
# by test-device-projection.sh, which sources after this trap is installed; the
# config path is swept as well as the recorded pid, so a case that aborted before
# recording one still leaves no daemon holding its state store.
trap 'kill "$PF_PID" 2>/dev/null || true; kill "${PF_HEALTH_PID:-}" 2>/dev/null || true; kill "${DP_DAEMON_PID:-}" 2>/dev/null || true; [ -n "${DP_CONF:-}" ] && pkill -f "$DP_CONF" 2>/dev/null; rm -rf "$GW_SCRATCH"; cleanup_e2e' EXIT

# Disable set -e for the test body — individual test failures are tracked by
# fail_test/pass_test, and print_summary returns non-zero if any test failed.
set +e

source "$SCRIPT_DIR/test-health.sh"
source "$SCRIPT_DIR/test-enrollment.sh"
source "$SCRIPT_DIR/test-checkin.sh"
source "$SCRIPT_DIR/test-device-projection.sh"
source "$SCRIPT_DIR/test-api.sh"
source "$SCRIPT_DIR/test-admin.sh"
source "$SCRIPT_DIR/test-streaming.sh"
source "$SCRIPT_DIR/test-dashboard.sh"

print_summary "Gateway Tests"
