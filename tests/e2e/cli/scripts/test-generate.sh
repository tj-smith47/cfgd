#!/usr/bin/env bash
# E2E tests for: cfgd generate
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd generate tests ==="

# ── GEN01: generate --help ─────────────────────────────────────────
begin_test "GEN01: generate --help"
run $C generate --help
if assert_ok && assert_contains "$OUTPUT" "generate"; then
    pass_test "GEN01"
else
    fail_test "GEN01"
fi

# ── GEN02: generate --scan-only ────────────────────────────────────
begin_test "GEN02: generate --scan-only"
run $C generate --scan-only
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    # Scan-only should not call any AI API; verify scan-related output
    if assert_contains "$OUTPUT" "Scan"; then
        pass_test "GEN02"
    else
        fail_test "GEN02" "expected scan output"
    fi
else
    fail_test "GEN02" "exit $RC"
fi

# ── GEN03: generate module --help ──────────────────────────────────
begin_test "GEN03: generate module --help"
run $C generate module --help
if assert_ok && assert_contains "$OUTPUT" "module"; then
    pass_test "GEN03"
else
    fail_test "GEN03" "exit $RC"
fi

# ── GEN04: generate without ANTHROPIC_API_KEY ──────────────────────
begin_test "GEN04: generate without ANTHROPIC_API_KEY"
# Save and unset the key, then run generate (requires API key)
SAVED_KEY="${ANTHROPIC_API_KEY:-}"
unset ANTHROPIC_API_KEY 2>/dev/null || true
run $C generate --yes
# Restore key
if [ -n "$SAVED_KEY" ]; then
    export ANTHROPIC_API_KEY="$SAVED_KEY"
fi
if assert_fail && assert_contains "$OUTPUT" "API key"; then
    pass_test "GEN04"
else
    fail_test "GEN04" "expected non-zero exit and API key error"
fi

# ── GEN05: generate with API key (gated) ──────────────────────────
begin_test "GEN05: generate with API key (full flow, gated)"
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    skip_test "GEN05" "ANTHROPIC_API_KEY not set"
else
    run $C generate --yes
    if assert_ok; then
        pass_test "GEN05"
    else
        fail_test "GEN05" "exit $RC"
    fi
fi

# ── GEN06: generate --model override (gated) ──────────────────────
begin_test "GEN06: generate --model override (gated)"
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    skip_test "GEN06" "ANTHROPIC_API_KEY not set"
else
    run $C generate --model claude-sonnet-4-20250514 --yes
    if assert_ok; then
        pass_test "GEN06"
    else
        fail_test "GEN06" "exit $RC"
    fi
fi

print_summary "Generate"
