#!/usr/bin/env bash
# E2E tests for: cfgd status
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd status tests ==="

# State prerequisite: apply so status has state to check
run $C apply --yes

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "S01: status"
run $C status
if assert_ok; then
    pass_test "S01"
else fail_test "S01"; fi

begin_test "S02: status --verbose"
run $C status --verbose
if assert_ok; then
    pass_test "S02"
else fail_test "S02"; fi

begin_test "S03: status --quiet"
run $C status --quiet
if assert_ok; then
    pass_test "S03"
else fail_test "S03"; fi

begin_test "S04: status --module"
run $C status --module nvim
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "S04"
else fail_test "S04" "exit $RC"; fi

print_summary "Status"
