#!/usr/bin/env bash
# E2E tests for: cfgd sync
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd sync tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "SY01: sync"
run $C sync
if assert_ok; then
    pass_test "SY01"
else fail_test "SY01"; fi

print_summary "Sync"
