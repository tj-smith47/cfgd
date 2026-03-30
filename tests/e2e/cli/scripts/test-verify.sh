#!/usr/bin/env bash
# E2E tests for: cfgd verify
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd verify tests ==="

# State prerequisite: apply so verify has state to check
run $C apply --yes

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "V01: verify"
run $C verify
if assert_ok; then
    pass_test "V01"
else fail_test "V01"; fi

begin_test "V02: verify --module"
run $C verify --module nvim
if assert_ok; then
    pass_test "V02"
else fail_test "V02"; fi

print_summary "Verify"
