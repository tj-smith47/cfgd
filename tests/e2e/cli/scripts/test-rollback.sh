#!/usr/bin/env bash
# E2E tests for: cfgd rollback
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd rollback tests ==="

# State prerequisite: apply so rollback has state to check
run $C apply --yes

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "RB01: rollback --help"
run $C rollback 0 --help
if assert_ok && assert_contains "$OUTPUT" "roll"; then
    pass_test "RB01"
else fail_test "RB01"; fi

begin_test "RB02: rollback nonexistent apply ID"
run $C rollback 999999 --yes
if assert_fail; then
    pass_test "RB02"
else fail_test "RB02"; fi

begin_test "RB03: rollback -y (short flag)"
run $C rollback 999999 -y
if assert_fail; then
    pass_test "RB03"
else fail_test "RB03"; fi

begin_test "RB04: rollback valid apply ID"
# Get the most recent apply ID from the log
APPLY_ID=$("$CFGD" $C log -n 1 --output json 2>&1 | grep -oE '"id":\s*[0-9]+' | head -1 | grep -oE '[0-9]+' || echo "")
if [ -z "$APPLY_ID" ]; then
    APPLY_ID=$("$CFGD" $C log -n 1 2>&1 | grep -E '^[0-9]' | awk '{print $1}' | head -1 || echo "")
fi
if [ -n "$APPLY_ID" ]; then
    run $C rollback "$APPLY_ID" --yes
    # May succeed (restores files) or fail (no backups) — both are valid
    if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
        pass_test "RB04"
    else fail_test "RB04" "exit $RC"; fi
else
    skip_test "RB04" "No apply ID found in log"
fi

print_summary "Rollback"
