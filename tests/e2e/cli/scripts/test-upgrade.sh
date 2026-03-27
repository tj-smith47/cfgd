#!/usr/bin/env bash
# E2E tests for: cfgd upgrade
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd upgrade tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "UP01: upgrade --help"
run $C upgrade --help
if assert_ok; then
    pass_test "UP01"
else fail_test "UP01"; fi

begin_test "UP02: upgrade --check"
run $C upgrade --check
# May fail without network, but should not crash
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "UP02"
else fail_test "UP02" "exit $RC"; fi

print_summary "Upgrade"
