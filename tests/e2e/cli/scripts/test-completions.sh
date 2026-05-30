#!/usr/bin/env bash
# E2E tests for: cfgd completion
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd completion tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "CMP01: completion bash"
run $C completion bash
if assert_ok && assert_contains "$OUTPUT" "complete"; then
    pass_test "CMP01"
else fail_test "CMP01"; fi

begin_test "CMP02: completion zsh"
run $C completion zsh
if assert_ok; then
    pass_test "CMP02"
else fail_test "CMP02"; fi

begin_test "CMP03: completion fish"
run $C completion fish
if assert_ok; then
    pass_test "CMP03"
else fail_test "CMP03"; fi

begin_test "CMP04: completion powershell"
run $C completion powershell
if assert_ok; then
    pass_test "CMP04"
else fail_test "CMP04"; fi

begin_test "CMP05: completion elvish"
run $C completion elvish
if assert_ok; then
    pass_test "CMP05"
else fail_test "CMP05"; fi

# Back-compat: the plural `completions` alias must still resolve.
begin_test "CMP06: completions alias (back-compat)"
run $C completions bash
if assert_ok && assert_contains "$OUTPUT" "complete"; then
    pass_test "CMP06"
else fail_test "CMP06"; fi

print_summary "Completion"
