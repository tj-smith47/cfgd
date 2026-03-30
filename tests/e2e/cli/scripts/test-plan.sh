#!/usr/bin/env bash
# E2E tests for: cfgd plan
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd plan tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "PL01: plan --help"
run $C plan --help
if assert_ok && assert_contains "$OUTPUT" "phase"; then
    pass_test "PL01"
else fail_test "PL01"; fi

begin_test "PL02: plan (default)"
run $C plan
if assert_ok; then
    pass_test "PL02"
else fail_test "PL02"; fi

begin_test "PL03: plan --phase files"
run $C plan --phase files
if assert_ok; then
    pass_test "PL03"
else fail_test "PL03"; fi

begin_test "PL04: plan --phase packages"
run $C plan --phase packages
if assert_ok; then
    pass_test "PL04"
else fail_test "PL04"; fi

begin_test "PL05: plan --phase system"
run $C plan --phase system
if assert_ok; then
    pass_test "PL05"
else fail_test "PL05"; fi

begin_test "PL06: plan --phase env"
run $C plan --phase env
if assert_ok; then
    pass_test "PL06"
else fail_test "PL06"; fi

begin_test "PL07: plan --phase secrets"
run $C plan --phase secrets
if assert_ok; then
    pass_test "PL07"
else fail_test "PL07"; fi

begin_test "PL08: plan --skip files"
run $C plan --skip files
if assert_ok; then
    pass_test "PL08"
else fail_test "PL08"; fi

begin_test "PL09: plan --only files"
run $C plan --only files
if assert_ok; then
    pass_test "PL09"
else fail_test "PL09"; fi

begin_test "PL10: plan --module (nonexistent)"
run $C plan --module nonexistent
# Nonexistent module filter should produce empty plan and succeed
if assert_ok; then
    pass_test "PL10"
else fail_test "PL10"; fi

begin_test "PL11: plan --skip-scripts"
run $C plan --skip-scripts
if assert_ok; then
    pass_test "PL11"
else fail_test "PL11"; fi

begin_test "PL12: plan --context reconcile"
run $C plan --context reconcile
if assert_ok; then
    pass_test "PL12"
else fail_test "PL12"; fi

begin_test "PL13: plan --context apply (explicit default)"
run $C plan --context apply
if assert_ok; then
    pass_test "PL13"
else fail_test "PL13"; fi

begin_test "PL14: plan --skip multiple"
run $C plan --skip files --skip packages
if assert_ok; then
    pass_test "PL14"
else fail_test "PL14"; fi

begin_test "PL15: plan --only multiple"
run $C plan --only files --only env
if assert_ok; then
    pass_test "PL15"
else fail_test "PL15"; fi

print_summary "Plan"
