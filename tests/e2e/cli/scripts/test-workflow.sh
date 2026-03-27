#!/usr/bin/env bash
# E2E tests for: cfgd workflow
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd workflow tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "WF01: workflow --help"
run $C workflow --help
if assert_ok; then
    pass_test "WF01"
else fail_test "WF01"; fi

begin_test "WF02: workflow generate"
run $C workflow generate
if assert_ok; then
    pass_test "WF02"
else fail_test "WF02"; fi

begin_test "WF03: workflow generate --force"
run $C workflow generate --force
if assert_ok; then
    pass_test "WF03"
else fail_test "WF03"; fi

print_summary "Workflow"
