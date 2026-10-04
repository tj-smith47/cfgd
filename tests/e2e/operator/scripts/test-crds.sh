# shellcheck shell=bash
# Operator E2E tests: CRDs
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== CRD Tests ==="

# =================================================================
# OP-CRD-01: CRDs are installed and established (all 6)
# =================================================================
begin_test "OP-CRD-01: CRDs installed (all 6)"
MC_CRD=$(kubectl get crd machineconfigs.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
CP_CRD=$(kubectl get crd configpolicies.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
DA_CRD=$(kubectl get crd driftalerts.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
MOD_CRD=$(kubectl get crd modules.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
CCP_CRD=$(kubectl get crd clusterconfigpolicies.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
BP_CRD=$(kubectl get crd backuppolicies.cfgd.io -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")

echo "  MachineConfig CRD:       ${MC_CRD:-not found}"
echo "  ConfigPolicy CRD:        ${CP_CRD:-not found}"
echo "  DriftAlert CRD:          ${DA_CRD:-not found}"
echo "  Module CRD:              ${MOD_CRD:-not found}"
echo "  ClusterConfigPolicy CRD: ${CCP_CRD:-not found}"
echo "  BackupPolicy CRD:        ${BP_CRD:-not found}"

if [ -n "$MC_CRD" ] && [ -n "$CP_CRD" ] && [ -n "$DA_CRD" ] && \
   [ -n "$MOD_CRD" ] && [ -n "$CCP_CRD" ] && [ -n "$BP_CRD" ]; then
    pass_test "OP-CRD-01"
else
    fail_test "OP-CRD-01" "One or more CRDs not installed"
fi

# =================================================================
# OP-CRD-02: Operator pod is running
# =================================================================
begin_test "OP-CRD-02: Operator pod running"
if wait_for_pod "$E2E_INSTALL_NS" "$E2E_OPERATOR_PODS" 60; then
    pass_test "OP-CRD-02"
else
    fail_test "OP-CRD-02" "Operator pod not running"
    kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o wide 2>/dev/null || true
fi

# =================================================================
# OP-PR-01: The operator under test is this run's image
# =================================================================
begin_test "OP-PR-01: Operator under test is this run's image"
PR01_RUNNING="$(running_image deployment "$E2E_OPERATOR_DEPLOY" operator "$E2E_INSTALL_NS")"
PR01_WANT="$(e2e_image cfgd-operator)"
echo "  deployment/$E2E_OPERATOR_DEPLOY in $E2E_INSTALL_NS runs: $PR01_RUNNING"
if [ "$PR01_RUNNING" = "$PR01_WANT" ]; then
    pass_test "OP-PR-01"
else
    fail_test "OP-PR-01" "deployment/$E2E_OPERATOR_DEPLOY in $E2E_INSTALL_NS runs $PR01_RUNNING, want $PR01_WANT"
fi
