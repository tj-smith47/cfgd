#!/usr/bin/env bash
# E2E tests for: cfgd output format flags
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd output format tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "OF01: status --output json"
run $C status --output json
if assert_ok && assert_contains "$OUTPUT" "{"; then
    pass_test "OF01"
else fail_test "OF01"; fi

begin_test "OF02: status --output yaml"
run $C status --output yaml
if assert_ok; then
    pass_test "OF02"
else fail_test "OF02"; fi

begin_test "OF03: status --output wide"
run $C status --output wide
if assert_ok; then
    pass_test "OF03"
else fail_test "OF03"; fi

begin_test "OF04: status --output table"
run $C status --output table
if assert_ok; then
    pass_test "OF04"
else fail_test "OF04"; fi

begin_test "OF05: status --output name"
run $C status --output name
if assert_ok; then
    pass_test "OF05"
else fail_test "OF05"; fi

begin_test "OF06: profile list --output json"
run $C profile list --output json
if assert_ok && assert_contains "$OUTPUT" "{"; then
    pass_test "OF06"
else fail_test "OF06"; fi

begin_test "OF07: profile list --output yaml"
run $C profile list --output yaml
if assert_ok; then
    pass_test "OF07"
else fail_test "OF07"; fi

begin_test "OF08: module list --output json"
run $C module list --output json
# May output [] (empty array) or [{...}] (populated) — both are valid JSON
if assert_ok && (assert_contains "$OUTPUT" "[" || assert_contains "$OUTPUT" "{"); then
    pass_test "OF08"
else fail_test "OF08"; fi

begin_test "OF09: module list --output yaml"
run $C module list --output yaml
if assert_ok; then
    pass_test "OF09"
else fail_test "OF09"; fi

begin_test "OF10: source list --output json"
run $C source list --output json
# May output [] (empty array) or [{...}] (populated) — both are valid JSON
if assert_ok; then
    pass_test "OF10"
else fail_test "OF10"; fi

begin_test "OF11: log --output json"
run $C log --output json
if assert_ok; then
    pass_test "OF11"
else fail_test "OF11"; fi

begin_test "OF12: --output jsonpath=EXPR"
run $C status --output 'jsonpath={.drift}'
if assert_ok; then
    pass_test "OF12"
else fail_test "OF12"; fi

begin_test "OF13: -o short flag"
run $C status -o json
if assert_ok && assert_contains "$OUTPUT" "{"; then
    pass_test "OF13"
else fail_test "OF13"; fi

print_summary "OutputFormats"
