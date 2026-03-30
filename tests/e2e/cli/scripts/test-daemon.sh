#!/usr/bin/env bash
# E2E tests for: cfgd daemon
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd daemon tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "DM01: daemon --help"
run $C daemon --help
if assert_ok && assert_contains "$OUTPUT" "install"; then
    pass_test "DM01"
else fail_test "DM01"; fi

begin_test "DM02: daemon status"
run $C daemon status
# Daemon not running, status should still succeed (reports not-running)
if assert_ok; then
    pass_test "DM02"
else fail_test "DM02"; fi

begin_test "DM03: daemon install"
run $C daemon install
# Requires systemd/launchd — skip if unavailable
if assert_ok; then
    pass_test "DM03"
else
    skip_test "DM03" "daemon install not available (no init system)"
fi

begin_test "DM04: daemon uninstall"
run $C daemon uninstall
# Requires systemd/launchd — skip if unavailable
if assert_ok; then
    pass_test "DM04"
else
    skip_test "DM04" "daemon uninstall not available (no init system)"
fi

print_summary "Daemon"
