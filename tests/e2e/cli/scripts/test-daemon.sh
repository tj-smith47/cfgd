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
# Daemon not running, should report that
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DM02"
else fail_test "DM02" "exit $RC"; fi

begin_test "DM03: daemon install"
run $C daemon install
# May succeed or fail depending on systemd/launchd availability
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DM03"
else fail_test "DM03" "exit $RC"; fi

begin_test "DM04: daemon uninstall"
run $C daemon uninstall
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DM04"
else fail_test "DM04" "exit $RC"; fi

print_summary "Daemon"
