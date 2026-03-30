#!/usr/bin/env bash
# E2E tests for: cfgd decide
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd decide tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "DEC01: decide --help"
run $C decide --help
if assert_ok; then
    pass_test "DEC01"
else fail_test "DEC01"; fi

begin_test "DEC02: decide accept --all (no pending)"
run $C decide accept --all
if assert_ok; then
    pass_test "DEC02"
else fail_test "DEC02"; fi

begin_test "DEC03: decide reject --all (no pending)"
run $C decide reject --all
if assert_ok; then
    pass_test "DEC03"
else fail_test "DEC03"; fi

begin_test "DEC04: decide accept --source (no matching pending)"
run $C decide accept --source nonexistent
if assert_ok; then
    pass_test "DEC04"
else fail_test "DEC04"; fi

begin_test "DEC05: decide accept specific resource (no pending)"
run $C decide accept packages.brew.formulae
if assert_ok; then
    pass_test "DEC05"
else fail_test "DEC05"; fi

# SECTION 39: decide reject subcommand

begin_test "DEC06: decide reject --source (no matching pending)"
run $C decide reject --source nonexistent
if assert_ok; then
    pass_test "DEC06"
else fail_test "DEC06"; fi

begin_test "DEC07: decide reject specific resource (no pending)"
run $C decide reject packages.brew.formulae
if assert_ok; then
    pass_test "DEC07"
else fail_test "DEC07"; fi

print_summary "Decide"
