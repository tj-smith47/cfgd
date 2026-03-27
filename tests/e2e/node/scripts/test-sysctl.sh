# Node E2E tests: Sysctl
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Sysctl Tests ==="

# =================================================================
# SYSCTL-01: Drift detection after manual change
# =================================================================
begin_test "SYSCTL-01: Drift detection"
# Change a sysctl value manually
ORIG=$(exec_in_pod cat /proc/sys/vm/max_map_count)
exec_in_pod sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --dry-run --no-color 2>&1) || true
echo "  After changing vm.max_map_count to 65530:"
echo "$OUTPUT" | head -15 | sed 's/^/    /'

if assert_contains "$OUTPUT" "vm.max_map_count"; then
    pass_test "SYSCTL-01"
else
    fail_test "SYSCTL-01" "Drift not detected for vm.max_map_count"
fi

# Restore
exec_in_pod sysctl -w "vm.max_map_count=$ORIG" > /dev/null 2>&1 || true

# =================================================================
# SYSCTL-02: Sysctl set-verify-drift cycle
# =================================================================
begin_test "SYSCTL-02: Sysctl set-verify-drift cycle"
# Save original
ORIG=$(exec_in_pod cat /proc/sys/vm/max_map_count)
echo "  Original vm.max_map_count: $ORIG"

# Apply desired state
exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true
APPLIED=$(exec_in_pod cat /proc/sys/vm/max_map_count)
echo "  After apply: $APPLIED"

# Introduce drift
exec_in_pod sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true
DRIFTED=$(exec_in_pod cat /proc/sys/vm/max_map_count)
echo "  After manual drift: $DRIFTED"

# Detect drift
PLAN=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --dry-run --no-color 2>&1) || true

if assert_equals "$DRIFTED" "65530" && assert_contains "$PLAN" "vm.max_map_count"; then
    # Re-apply to fix drift
    exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true
    FIXED=$(exec_in_pod cat /proc/sys/vm/max_map_count)
    echo "  After re-apply: $FIXED"
    if assert_equals "$FIXED" "262144"; then
        pass_test "SYSCTL-02"
    else
        fail_test "SYSCTL-02" "Re-apply did not fix drift (got $FIXED)"
    fi
else
    fail_test "SYSCTL-02" "Drift not properly detected"
fi

# =================================================================
# SYSCTL-03: Sysctl persistence file created
# =================================================================
begin_test "SYSCTL-03: Sysctl persistence file"
if exec_in_pod test -f /etc/sysctl.d/99-cfgd.conf; then
    CONTENT=$(exec_in_pod cat /etc/sysctl.d/99-cfgd.conf)
    echo "  /etc/sysctl.d/99-cfgd.conf:"
    echo "$CONTENT" | head -5 | sed 's/^/    /'
    if assert_contains "$CONTENT" "vm.max_map_count"; then
        pass_test "SYSCTL-03"
    else
        fail_test "SYSCTL-03" "Persistence file missing vm.max_map_count entry"
    fi
else
    fail_test "SYSCTL-03" "/etc/sysctl.d/99-cfgd.conf not found"
fi
