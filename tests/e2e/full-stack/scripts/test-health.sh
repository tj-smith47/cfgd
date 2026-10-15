# shellcheck shell=bash
# Full-stack E2E tests: Health
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== Health Tests ==="

# =================================================================
# FS-HEALTH-01: All components deployed and healthy
# =================================================================
begin_test "FS-HEALTH-01: All components healthy"

# Wait for rollouts to fully complete (setup may have triggered a rollout restart)
kubectl rollout status deployment/"$E2E_OPERATOR_DEPLOY" -n "$E2E_INSTALL_NS" --timeout=120s 2>/dev/null || true

# Check deployment readiness (all replicas updated and available)
GATEWAY_METHOD=$(exec_in_pod curl -sf "${E2E_GATEWAY_URL}/api/v1/enroll/info" 2>/dev/null | jq -r '.method // empty' 2>/dev/null || echo "")
OPERATOR_POD=$(kubectl get deployment "$E2E_OPERATOR_DEPLOY" -n "$E2E_INSTALL_NS" \
    -o jsonpath='{.status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
CFGD_AVAIL=$(exec_in_pod cfgd --version 2>&1 || echo "")

echo "  cfgd binary:  ${CFGD_AVAIL:-not found}"
echo "  Gateway enrollment method: ${GATEWAY_METHOD:-no answer}"
echo "  Operator available: $OPERATOR_POD"

# The PR install keeps the chart's token enrollment.
if [ "$GATEWAY_METHOD" = "token" ] && [ "$OPERATOR_POD" = "True" ] && [ -n "$CFGD_AVAIL" ]; then
    pass_test "FS-HEALTH-01"
else
    fail_test "FS-HEALTH-01" "Not all components are healthy"
fi
