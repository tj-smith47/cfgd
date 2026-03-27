# Full-stack E2E tests: Health
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Health Tests ==="

# =================================================================
# FS-HEALTH-01: All components deployed and healthy
# =================================================================
begin_test "FS-HEALTH-01: All components healthy"

# Wait for rollouts to fully complete (setup may have triggered a rollout restart)
kubectl rollout status deployment/cfgd-server -n cfgd-system --timeout=120s 2>/dev/null || true
kubectl rollout status deployment/cfgd-operator -n cfgd-system --timeout=120s 2>/dev/null || true

# Check deployment readiness (all replicas updated and available)
GATEWAY_POD=$(kubectl get deployment cfgd-server -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
OPERATOR_POD=$(kubectl get deployment cfgd-operator -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
CFGD_AVAIL=$(exec_in_pod cfgd --version 2>&1 || echo "")

echo "  cfgd binary:  ${CFGD_AVAIL:-not found}"
echo "  Gateway available: $GATEWAY_POD"
echo "  Operator available: $OPERATOR_POD"

if [ "$GATEWAY_POD" = "True" ] && [ "$OPERATOR_POD" = "True" ] && [ -n "$CFGD_AVAIL" ]; then
    pass_test "FS-HEALTH-01"
else
    fail_test "FS-HEALTH-01" "Not all components are healthy"
fi
