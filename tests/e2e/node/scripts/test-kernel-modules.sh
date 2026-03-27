# Node E2E tests: Kernel Modules
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Kernel Module Tests ==="

# =================================================================
# KMOD-01: Kernel module loading
# =================================================================
begin_test "KMOD-01: Kernel module loading"
# Unload ip_vs first to ensure we test cfgd's ability to load it
exec_in_pod rmmod ip_vs 2>/dev/null || true
BEFORE=$(exec_in_pod bash -c 'grep -c "^ip_vs " /proc/modules 2>/dev/null || echo 0' | tr -d '[:space:]')
echo "  ip_vs loaded before apply: $BEFORE"

exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true
AFTER=$(exec_in_pod bash -c 'grep -c "^ip_vs " /proc/modules 2>/dev/null || echo 0' | tr -d '[:space:]')
echo "  ip_vs loaded after apply: $AFTER"

if [ "$AFTER" -gt 0 ]; then
    pass_test "KMOD-01"
else
    fail_test "KMOD-01" "cfgd apply did not load ip_vs"
fi

# =================================================================
# KMOD-02: Kernel module persistence file
# =================================================================
begin_test "KMOD-02: Kernel module persistence"
# KMOD-01's apply should have written the persistence file
if exec_in_pod test -f /etc/modules-load.d/cfgd.conf; then
    CONTENT=$(exec_in_pod cat /etc/modules-load.d/cfgd.conf)
    echo "  /etc/modules-load.d/cfgd.conf:"
    echo "$CONTENT" | head -5 | sed 's/^/    /'
    if assert_contains "$CONTENT" "ip_vs"; then
        pass_test "KMOD-02"
    else
        fail_test "KMOD-02" "Persistence file missing ip_vs entry"
    fi
else
    fail_test "KMOD-02" "/etc/modules-load.d/cfgd.conf not created"
fi
