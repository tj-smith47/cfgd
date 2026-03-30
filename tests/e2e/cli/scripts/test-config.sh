#!/usr/bin/env bash
# E2E tests for: cfgd config
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd config tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "CF01: config --help"
run $C config --help
if assert_ok && assert_contains "$OUTPUT" "show"; then
    pass_test "CF01"
else fail_test "CF01"; fi

begin_test "CF02: config show"
run $C config show
if assert_ok && assert_contains "$OUTPUT" "dev"; then
    pass_test "CF02"
else fail_test "CF02"; fi

# config edit requires $EDITOR interaction, tested via EDITOR=true
begin_test "CF03: config edit (EDITOR=true)"
EDITOR=true run $C config edit
if assert_ok; then
    pass_test "CF03"
else fail_test "CF03"; fi

# SECTION 27: config get/set/unset

begin_test "CF04: config get"
run $C config get profile
if assert_ok; then
    pass_test "CF04"
else fail_test "CF04"; fi

begin_test "CF05: config set"
run $C config set theme minimal
if assert_ok; then
    pass_test "CF05"
else fail_test "CF05"; fi

begin_test "CF06: config get (verify set)"
run $C config get theme
if assert_ok && assert_contains "$OUTPUT" "minimal"; then
    pass_test "CF06"
else fail_test "CF06"; fi

begin_test "CF07: config unset"
run $C config unset theme
if assert_ok; then
    pass_test "CF07"
else fail_test "CF07"; fi

begin_test "CF08: config get nonexistent key"
run $C config get nonexistent.key.path
if assert_fail; then
    pass_test "CF08"
else fail_test "CF08"; fi

print_summary "Config"
