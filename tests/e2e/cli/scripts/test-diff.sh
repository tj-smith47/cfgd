#!/usr/bin/env bash
# E2E tests for: cfgd diff
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd diff tests ==="

# State prerequisite: apply so diff has state to check
run $C apply --yes

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "D01: diff"
run $C diff
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "D01"
else fail_test "D01" "exit $RC"; fi

begin_test "D02: diff --module"
run $C diff --module nvim
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "D02"
else fail_test "D02" "exit $RC"; fi

print_summary "Diff"
