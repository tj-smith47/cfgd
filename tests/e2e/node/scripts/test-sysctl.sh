# Node E2E tests: Sysctl
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Sysctl Tests ==="

# =================================================================
# SYSCTL-01: Drift detection after manual change
# =================================================================
begin_test "SYSCTL-01: Drift detection"
# Change a sysctl value manually
ORIG=$(exec_in_pod cat /proc/sys/net/ipv4/ip_forward)
exec_in_pod sysctl -w net.ipv4.ip_forward=0 > /dev/null 2>&1 || true

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --dry-run --no-color 2>&1) || true
echo "  After changing net.ipv4.ip_forward to 0:"
echo "$OUTPUT" | head -15 | sed 's/^/    /'

if assert_contains "$OUTPUT" "net.ipv4.ip_forward"; then
    pass_test "SYSCTL-01"
else
    fail_test "SYSCTL-01" "Drift not detected for net.ipv4.ip_forward"
fi

# Restore
exec_in_pod sysctl -w "net.ipv4.ip_forward=$ORIG" > /dev/null 2>&1 || true

# =================================================================
# SYSCTL-02: Sysctl set-verify-drift cycle
# =================================================================
begin_test "SYSCTL-02: Sysctl set-verify-drift cycle"
# Save original
ORIG=$(exec_in_pod cat /proc/sys/net/ipv4/ip_forward)
echo "  Original net.ipv4.ip_forward: $ORIG"

# Apply desired state
exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true
APPLIED=$(exec_in_pod cat /proc/sys/net/ipv4/ip_forward)
echo "  After apply: $APPLIED"

# Introduce drift
exec_in_pod sysctl -w net.ipv4.ip_forward=0 > /dev/null 2>&1 || true
DRIFTED=$(exec_in_pod cat /proc/sys/net/ipv4/ip_forward)
echo "  After manual drift: $DRIFTED"

# Detect drift
PLAN=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --dry-run --no-color 2>&1) || true

if assert_equals "$DRIFTED" "0" && assert_contains "$PLAN" "net.ipv4.ip_forward"; then
    # Re-apply to fix drift
    exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true
    FIXED=$(exec_in_pod cat /proc/sys/net/ipv4/ip_forward)
    echo "  After re-apply: $FIXED"
    if assert_equals "$FIXED" "1"; then
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
