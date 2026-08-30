#!/usr/bin/env bash
# .claude/scripts/audit-tests/audit-test.sh
# Validates that .claude/scripts/audit.sh's banned-pattern rules catch every
# bad_* fixture and ignore every good_* fixture. Run via `task audit:test`
# or invoked from `task ci`.
#
# Fixtures are .txt files outside the cargo source tree. The driver tells
# audit.sh to scope its scan to the fixture directory via CFGD_AUDIT_PATH.
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
    # `|| true`: a bad_* fixture legitimately drives audit.sh to a nonzero
    # exit (an ERROR-level gate), and this script runs under `set -e` — a
    # bare statement whose command fails would abort the whole suite on the
    # first caught violation instead of reaching fixture_is_clean below.
    CFGD_AUDIT_PATH="$fixture" \
        bash .claude/scripts/audit.sh > "$TMP/out" 2>&1 || true
}

# Only ERRORS flip the script's exit code (see audit.sh's summary), so a
# fixture that must trip a WARN-level gate — the dead-error-variant and
# repeated-string-literal gates below are both `log_warn` — passes the exit
# code check regardless of what it reports. The summary line is the one place
# both severities are visible, so "clean" means that exact line, not exit 0.
fixture_is_clean() {
    grep -qF '=== Audit Complete: 0 errors, 0 warnings ===' "$TMP/out"
}

for fix in "$FIXTURE_DIR"/bad_*.txt; do
    name=$(basename "$fix" .txt)
    run_audit_against "$fix"
    if fixture_is_clean; then
        echo "FAIL: $name was NOT caught by audit (expected violation)"
        cat "$TMP/out"
        FAIL=1
    else
        echo "ok:   $name correctly caught"
    fi
done

for fix in "$FIXTURE_DIR"/good_*.txt; do
    name=$(basename "$fix" .txt)
    run_audit_against "$fix"
    if fixture_is_clean; then
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
