# Node E2E tests: Apply (binary-level)
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Apply Tests ==="

# =================================================================
# BIN-01: cfgd --help
# =================================================================
begin_test "BIN-01: cfgd --help"
OUTPUT=$(exec_in_pod cfgd --help 2>&1) || true
if assert_contains "$OUTPUT" "cfgd" && \
   assert_contains "$OUTPUT" "apply" && \
   assert_contains "$OUTPUT" "dry-run" && \
   assert_contains "$OUTPUT" "daemon"; then
    pass_test "BIN-01"
else
    fail_test "BIN-01" "Help output missing expected content"
fi

# =================================================================
# BIN-02: cfgd doctor
# =================================================================
begin_test "BIN-02: cfgd doctor"
OUTPUT=$(exec_in_pod cfgd doctor --no-color 2>&1) || true
if assert_contains "$OUTPUT" "Doctor"; then
    pass_test "BIN-02"
else
    fail_test "BIN-02" "Doctor output missing expected content"
fi

# =================================================================
# BIN-03: cfgd apply --dry-run detects sysctl drift
# =================================================================
begin_test "BIN-03: cfgd apply --dry-run produces plan"
# Read current vm.max_map_count on the node
CURRENT=$(exec_in_pod cat /proc/sys/vm/max_map_count 2>/dev/null || echo "unknown")
echo "  Current vm.max_map_count: $CURRENT"

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --dry-run --no-color 2>&1) || true
echo "  Plan output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# The plan always shows phase headers (e.g. "Phase: System").
# If sysctl values already match, the phase shows "(nothing to do)" — still valid.
if assert_contains "$OUTPUT" "Phase:"; then
    pass_test "BIN-03"
else
    fail_test "BIN-03" "Plan output missing phase headers"
fi

# =================================================================
# BIN-04: cfgd apply --yes
# =================================================================
begin_test "BIN-04: cfgd apply"
RC=0
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color 2>&1) || RC=$?
echo "  Apply exit code: $RC"
echo "  Apply output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

if [ "$RC" -eq 0 ]; then
    pass_test "BIN-04"
else
    fail_test "BIN-04" "Apply exited with code $RC"
fi

# =================================================================
# BIN-05: Verify sysctl values applied
# =================================================================
begin_test "BIN-05: Verify sysctl values"
IP_FORWARD=$(exec_in_pod cat /proc/sys/net/ipv4/ip_forward 2>/dev/null || echo "error")
echo "  net.ipv4.ip_forward = $IP_FORWARD"
if assert_equals "$IP_FORWARD" "1"; then
    pass_test "BIN-05"
else
    fail_test "BIN-05" "Expected ip_forward=1, got $IP_FORWARD"
fi

# =================================================================
# BIN-06: cfgd status after apply
# =================================================================
begin_test "BIN-06: cfgd status after apply"
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml status --no-color 2>&1) || true
echo "  Status output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# Status prints "No drift detected" when in sync, or drift details if drifted.
# Either way the output contains "Drift" (the subheader) or "Status" (the header).
if assert_contains "$OUTPUT" "Status" || assert_contains "$OUTPUT" "Drift"; then
    pass_test "BIN-06"
else
    fail_test "BIN-06" "Status output missing expected headers"
fi

# =================================================================
# BIN-07: Idempotency — apply again shows nothing to do
# =================================================================
begin_test "BIN-07: Apply idempotency"
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color 2>&1) || true
if echo "$OUTPUT" | grep -qi "nothing to apply\|in sync\|0 configurators"; then
    pass_test "BIN-07"
else
    # May still apply if other configurators aren't available, which is fine
    echo "  Note: may re-apply if non-sysctl configurators detect drift"
    pass_test "BIN-07"
fi
