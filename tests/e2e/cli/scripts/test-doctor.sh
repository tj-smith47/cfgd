#!/usr/bin/env bash
# E2E tests for: cfgd doctor
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd doctor tests ==="

# State prerequisite: apply so doctor has state to check
run $C apply --yes

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "DR01: doctor"
run $C doctor
if assert_ok; then
    pass_test "DR01"
else fail_test "DR01"; fi

print_summary "Doctor"
