#!/usr/bin/env bash
# E2E tests for: cfgd global flags & help
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd global flags & help tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "G01: --help"
run $C --help
if assert_ok && assert_contains "$OUTPUT" "apply" && assert_contains "$OUTPUT" "profile"; then
    pass_test "G01"
else fail_test "G01"; fi

begin_test "G02: --version"
run --version
if assert_ok && assert_contains "$OUTPUT" "cfgd"; then
    pass_test "G02"
else fail_test "G02"; fi

begin_test "G03: --verbose flag accepted"
run $C status --verbose
# Just verify it doesn't crash — verbose may produce extra output or not
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G03"
else fail_test "G03" "exit $RC"; fi

begin_test "G04: -v short flag"
run $C status -v
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G04"
else fail_test "G04" "exit $RC"; fi

begin_test "G05: --quiet flag accepted"
run $C status --quiet
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G05"
else fail_test "G05" "exit $RC"; fi

begin_test "G06: -q short flag"
run $C status -q
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G06"
else fail_test "G06" "exit $RC"; fi

begin_test "G07: --no-color flag"
run --config "$CONF" --no-color status
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G07"
else fail_test "G07"; fi

begin_test "G08: --profile override"
run $C --profile base status
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G08"
else fail_test "G08"; fi

begin_test "G09: --config with bad path fails"
run --config /nonexistent/cfgd.yaml status
if assert_fail; then
    pass_test "G09"
else fail_test "G09"; fi

begin_test "G10: unknown subcommand fails"
run $C nonexistent-command
if assert_fail; then
    pass_test "G10"
else fail_test "G10"; fi

print_summary "Global"
