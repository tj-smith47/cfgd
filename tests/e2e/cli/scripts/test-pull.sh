#!/usr/bin/env bash
# E2E tests for: cfgd pull
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd pull tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "PU01: pull"
run $C pull
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "PU01"
else fail_test "PU01" "exit $RC"; fi

print_summary "Pull"
