#!/usr/bin/env bash
# E2E tests for: cfgd log
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd log tests ==="

# State prerequisite: apply so log has state to check
run $C apply --yes

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "L01: log"
run $C log
if assert_ok; then
    pass_test "L01"
else fail_test "L01"; fi

begin_test "L02: log --limit 5"
run $C log --limit 5
if assert_ok; then
    pass_test "L02"
else fail_test "L02"; fi

begin_test "L03: log -n 1"
run $C log -n 1
if assert_ok; then
    pass_test "L03"
else fail_test "L03"; fi

begin_test "L04: log --show-output <apply_id>"
# --show-output takes an apply ID — get one from the log table (first column is the numeric ID)
LOG_ID=$("$CFGD" $C log -n 1 --output json 2>&1 | grep -oE '"id":\s*[0-9]+' | head -1 | grep -oE '[0-9]+' || echo "")
if [ -z "$LOG_ID" ]; then
    # Fallback: parse table output — ID is the first number on the data line
    LOG_ID=$("$CFGD" $C log -n 1 2>&1 | grep -E '^[0-9]' | awk '{print $1}' | head -1 || echo "")
fi
if [ -n "$LOG_ID" ]; then
    run $C log --show-output "$LOG_ID"
    if assert_ok; then
        pass_test "L04"
    else fail_test "L04"; fi
else
    skip_test "L04" "No apply ID found in log"
fi

begin_test "L05: log --show-output invalid ID"
run $C log --show-output 999999
# Invalid ID should fail (entry not found)
if assert_fail; then
    pass_test "L05"
else fail_test "L05" "expected non-zero exit for invalid ID"; fi

print_summary "Log"
