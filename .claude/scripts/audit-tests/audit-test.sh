#!/usr/bin/env bash
# .claude/scripts/audit-tests/audit-test.sh
# Validates that .claude/scripts/audit.sh's banned-pattern rules catch every
# bad_* fixture and ignore every good_* fixture. Run via `task audit:test`
# or invoked from `task ci`.
#
# Fixtures are .txt files outside the cargo source tree. The driver tells
# audit.sh to include the fixture directory via CFGD_OUTPUT_V2_AUDIT_EXTRA_PATH.
# rg's --type-add 'rust:*.txt' makes the audit's existing rust-typed regexes
# match .txt content unchanged.
set -euo pipefail
cd "$(dirname "$0")/../../.."

FIXTURE_DIR=".claude/scripts/audit-tests"
TMP=$(mktemp -d)

cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

FAIL=0

run_audit_against() {
    local fixture="$1"
    CFGD_OUTPUT_V2_AUDIT=1 \
    CFGD_OUTPUT_V2_AUDIT_EXTRA_PATH="$fixture" \
        bash .claude/scripts/audit.sh > "$TMP/out" 2>&1
    return $?
}

for fix in "$FIXTURE_DIR"/bad_*.txt; do
    name=$(basename "$fix" .txt)
    if run_audit_against "$fix"; then
        echo "FAIL: $name was NOT caught by audit (expected violation)"
        cat "$TMP/out"
        FAIL=1
    else
        echo "ok:   $name correctly caught"
    fi
done

for fix in "$FIXTURE_DIR"/good_*.txt; do
    name=$(basename "$fix" .txt)
    if run_audit_against "$fix"; then
        echo "ok:   $name correctly accepted"
    else
        echo "FAIL: $name was flagged by audit (expected to pass)"
        cat "$TMP/out"
        FAIL=1
    fi
done

if [ "$FAIL" -ne 0 ]; then
    echo
    echo "audit-test FAILED — audit.sh rules drifted from expected behavior."
    exit 1
fi
echo
echo "All audit-test fixtures passed."
