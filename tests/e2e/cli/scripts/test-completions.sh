#!/usr/bin/env bash
# E2E tests for: cfgd completions
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd completions tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "CMP01: completions bash"
run $C completions bash
if assert_ok && assert_contains "$OUTPUT" "complete"; then
    pass_test "CMP01"
else fail_test "CMP01"; fi

begin_test "CMP02: completions zsh"
run $C completions zsh
if assert_ok; then
    pass_test "CMP02"
else fail_test "CMP02"; fi

begin_test "CMP03: completions fish"
run $C completions fish
if assert_ok; then
    pass_test "CMP03"
else fail_test "CMP03"; fi

begin_test "CMP04: completions powershell"
run $C completions powershell
if assert_ok; then
    pass_test "CMP04"
else fail_test "CMP04"; fi

begin_test "CMP05: completions elvish"
run $C completions elvish
if assert_ok; then
    pass_test "CMP05"
else fail_test "CMP05"; fi

print_summary "Completions"
