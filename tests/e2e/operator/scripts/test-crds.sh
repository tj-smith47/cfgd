# shellcheck shell=bash
# Operator E2E tests: CRDs
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== CRD Tests ==="

# =================================================================
# OP-CRD-01: CRDs are installed and established (all 6)
# =================================================================
begin_test "OP-CRD-01: CRDs installed (all 6)"
CRD01_MISSING=""
for crd in machineconfigs configpolicies driftalerts modules clusterconfigpolicies backuppolicies; do
    CRD01_ESTABLISHED=$(kubectl get crd "${crd}.cfgd.io" \
        -o jsonpath='{.status.conditions[?(@.type=="Established")].status}' 2>/dev/null || echo "")
    echo "  ${crd}.cfgd.io Established: ${CRD01_ESTABLISHED:-not found}"
    [ "$CRD01_ESTABLISHED" = "True" ] || CRD01_MISSING="$CRD01_MISSING ${crd}.cfgd.io"
done

if [ -z "$CRD01_MISSING" ]; then
    pass_test "OP-CRD-01"
else
    fail_test "OP-CRD-01" "Not established:$CRD01_MISSING"
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
