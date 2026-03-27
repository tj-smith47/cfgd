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
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC02"
else fail_test "DEC02" "exit $RC"; fi

begin_test "DEC03: decide reject --all (no pending)"
run $C decide reject --all
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC03"
else fail_test "DEC03" "exit $RC"; fi

begin_test "DEC04: decide accept --source"
run $C decide accept --source nonexistent
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC04"
else fail_test "DEC04" "exit $RC"; fi

begin_test "DEC05: decide accept specific resource"
run $C decide accept packages.brew.formulae
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC05"
else fail_test "DEC05" "exit $RC"; fi

# SECTION 39: decide reject subcommand

begin_test "DEC06: decide reject --source"
run $C decide reject --source nonexistent
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC06"
else fail_test "DEC06" "exit $RC"; fi

begin_test "DEC07: decide reject specific resource"
run $C decide reject packages.brew.formulae
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC07"
else fail_test "DEC07" "exit $RC"; fi

print_summary "Decide"
