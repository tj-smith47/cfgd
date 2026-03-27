# Operator E2E tests: CRDs
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== CRD Tests ==="

# =================================================================
# OP-CRD-01: CRDs are installed and established (all 5)
# =================================================================
begin_test "OP-CRD-01: CRDs installed (all 5)"
MC_CRD=$(kubectl get crd machineconfigs.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
CP_CRD=$(kubectl get crd configpolicies.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
DA_CRD=$(kubectl get crd driftalerts.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
MOD_CRD=$(kubectl get crd modules.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
CCP_CRD=$(kubectl get crd clusterconfigpolicies.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")

echo "  MachineConfig CRD:       ${MC_CRD:-not found}"
echo "  ConfigPolicy CRD:        ${CP_CRD:-not found}"
echo "  DriftAlert CRD:          ${DA_CRD:-not found}"
echo "  Module CRD:              ${MOD_CRD:-not found}"
echo "  ClusterConfigPolicy CRD: ${CCP_CRD:-not found}"

if [ -n "$MC_CRD" ] && [ -n "$CP_CRD" ] && [ -n "$DA_CRD" ] && \
   [ -n "$MOD_CRD" ] && [ -n "$CCP_CRD" ]; then
    pass_test "OP-CRD-01"
else
    fail_test "OP-CRD-01" "One or more CRDs not installed"
fi

# =================================================================
# OP-CRD-02: Operator pod is running
# =================================================================
begin_test "OP-CRD-02: Operator pod running"
if wait_for_pod cfgd-system "app=cfgd-operator" 60; then
    pass_test "OP-CRD-02"
else
    fail_test "OP-CRD-02" "Operator pod not running"
    kubectl get pods -n cfgd-system -l app=cfgd-operator -o wide 2>/dev/null || true
fi
