#!/usr/bin/env bash
# Run all CLI E2E tests.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== cfgd Exhaustive CLI Tests ==="

# Create shared scratch once (domain files will create subdirs)
export CLI_SCRATCH=$(mktemp -d)
trap 'rm -rf "$CLI_SCRATCH"' EXIT

SUITES=(
    test-global.sh
    test-init.sh
    test-apply.sh
    test-status.sh
    test-diff.sh
    test-log.sh
    test-verify.sh
    test-doctor.sh
    test-profile.sh
    test-module.sh
    test-source.sh
    test-explain.sh
    test-config.sh
    test-completions.sh
    test-secret.sh
    test-decide.sh
    test-daemon.sh
    test-sync.sh
    test-pull.sh
    test-upgrade.sh
    test-workflow.sh
    test-checkin.sh
    test-enroll.sh
    test-plan.sh
    test-rollback.sh
    test-output-formats.sh
    test-compliance.sh
    test-generate.sh
    test-mcp-server.sh
    test-behavioral.sh
)

FAILED_SUITES=()

for suite in "${SUITES[@]}"; do
    echo ""
    echo "──────────────────────────────────────"
    echo "Running: $suite"
    echo "──────────────────────────────────────"
    if bash "$SCRIPT_DIR/$suite"; then
        : # suite passed
    else
        FAILED_SUITES+=("$suite")
    fi
done

echo ""
echo "═══════════════════════════════════════"
echo "  CLI E2E Summary"
echo "═══════════════════════════════════════"
if [ ${#FAILED_SUITES[@]} -eq 0 ]; then
    echo "  All suites passed!"
    exit 0
else
    echo "  FAILED suites:"
    for s in "${FAILED_SUITES[@]}"; do
        echo "    - $s"
    done
    exit 1
fi
