#!/usr/bin/env bash
# run-all.sh for gateway tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# The real binary runs as the invoking user; keep it out of that user's home.
# shellcheck source=tests/e2e/common/scratch-home.sh
source "$SCRIPT_DIR/../../common/scratch-home.sh"
# shellcheck source=tests/e2e/common/helpers.sh
source "$SCRIPT_DIR/../../common/helpers.sh"

GW_SCRATCH=$(mktemp -d)
export GW_SCRATCH

# Cleanup trap: kill port-forward and any device daemon a case left running,
# delete ephemeral namespace, remove scratch. Installed before the setup starts
# the port-forwards, so a setup that fails part way still stops them.
# DP_DAEMON_PID and DP_CONF are set by test-device-projection.sh, which sources
# after this trap is installed; the config path is swept as well as the recorded
# pid, so a case that aborted before recording one still leaves no daemon
# holding its state store.
trap 'stop_port_forward "${PF_PID:-}"; stop_port_forward "${PF_HEALTH_PID:-}"; kill "${DP_DAEMON_PID:-}" 2>/dev/null || true; [ -n "${DP_CONF:-}" ] && pkill -f "$DP_CONF" 2>/dev/null; rm -rf "$GW_SCRATCH"; cleanup_e2e' EXIT

# shellcheck source=tests/e2e/gateway/scripts/setup-gateway-env.sh
source "$SCRIPT_DIR/setup-gateway-env.sh"

# Disable set -e for the test body — individual test failures are tracked by
# fail_test/pass_test, and print_summary returns non-zero if any test failed.
set +e

# shellcheck source=tests/e2e/gateway/scripts/test-health.sh
source "$SCRIPT_DIR/test-health.sh"
# shellcheck source=tests/e2e/gateway/scripts/test-enrollment.sh
source "$SCRIPT_DIR/test-enrollment.sh"
# shellcheck source=tests/e2e/gateway/scripts/test-checkin.sh
source "$SCRIPT_DIR/test-checkin.sh"
# shellcheck source=tests/e2e/gateway/scripts/test-device-projection.sh
source "$SCRIPT_DIR/test-device-projection.sh"
# shellcheck source=tests/e2e/gateway/scripts/test-api.sh
source "$SCRIPT_DIR/test-api.sh"
# shellcheck source=tests/e2e/gateway/scripts/test-admin.sh
source "$SCRIPT_DIR/test-admin.sh"
# shellcheck source=tests/e2e/gateway/scripts/test-streaming.sh
source "$SCRIPT_DIR/test-streaming.sh"
# shellcheck source=tests/e2e/gateway/scripts/test-dashboard.sh
source "$SCRIPT_DIR/test-dashboard.sh"

SUMMARY_RC=0
print_summary "Gateway Tests" || SUMMARY_RC=1
assert_real_config_dir_unchanged || SUMMARY_RC=1
exit "$SUMMARY_RC"
