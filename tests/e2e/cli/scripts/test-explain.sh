#!/usr/bin/env bash
# E2E tests for: cfgd explain
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd explain tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "E01: explain (no args — lists types)"
run $C explain
if assert_ok; then
    pass_test "E01"
else fail_test "E01"; fi

begin_test "E02: explain profile"
run $C explain profile
if assert_ok && assert_contains "$OUTPUT" "spec"; then
    pass_test "E02"
else fail_test "E02"; fi

begin_test "E03: explain module"
run $C explain module
if assert_ok; then
    pass_test "E03"
else fail_test "E03"; fi

begin_test "E04: explain cfgdconfig"
run $C explain cfgdconfig
if assert_ok; then
    pass_test "E04"
else fail_test "E04"; fi

begin_test "E05: explain configsource"
run $C explain configsource
if assert_ok; then
    pass_test "E05"
else fail_test "E05"; fi

begin_test "E06: explain machineconfig"
run $C explain machineconfig
if assert_ok; then
    pass_test "E06"
else fail_test "E06"; fi

begin_test "E07: explain configpolicy"
run $C explain configpolicy
if assert_ok; then
    pass_test "E07"
else fail_test "E07"; fi

begin_test "E08: explain driftalert"
run $C explain driftalert
if assert_ok; then
    pass_test "E08"
else fail_test "E08"; fi

begin_test "E09: explain teamconfig"
run $C explain teamconfig
if assert_ok; then
    pass_test "E09"
else fail_test "E09"; fi

begin_test "E10: explain --recursive profile"
run $C explain --recursive profile
if assert_ok; then
    pass_test "E10"
else fail_test "E10"; fi

begin_test "E11: explain profile.spec.packages"
run $C explain profile.spec.packages
if assert_ok; then
    pass_test "E11"
else fail_test "E11"; fi

begin_test "E12: explain unknown type"
run $C explain nonexistent
if assert_fail || assert_contains "$OUTPUT" "unknown"; then
    pass_test "E12"
else fail_test "E12"; fi

# SECTION 41: explain additional types

begin_test "E13: explain clusterconfigpolicy (not in schema — fails gracefully)"
run $C explain clusterconfigpolicy
if assert_fail && assert_contains "$OUTPUT" "Unknown resource type"; then
    pass_test "E13"
else fail_test "E13"; fi

print_summary "Explain"
