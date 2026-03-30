# Operator E2E tests: DriftAlert
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== DriftAlert Tests ==="

# =================================================================
# OP-DA-01: DriftAlert — marks MachineConfig as drifted
# =================================================================
begin_test "OP-DA-01: DriftAlert creates drift on MachineConfig"

# Create a MachineConfig for drift tests (previous test suites clean up theirs)
kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-drift-mc-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: e2e-drift-host
  profile: dev-workstation
  packages:
    - name: vim
    - name: git
    - name: curl
EOF

# Wait for MC to be reconciled before creating drift alert
sleep 3

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: e2e-drift-1
  namespace: ${E2E_NAMESPACE}
spec:
  deviceId: e2e-host-1
  machineConfigRef:
    name: e2e-drift-mc-${E2E_RUN_ID}
  severity: Medium
  driftDetails:
    - field: packages.ripgrep
      expected: installed
      actual: missing
EOF

# Wait for DriftAlert controller to mark MC as drifted (via DriftDetected condition)
echo "  Waiting for drift propagation..."
DRIFT_COND=$(wait_for_k8s_field machineconfig e2e-drift-mc-${E2E_RUN_ID} "$E2E_NAMESPACE" \
    '{.status.conditions[?(@.type=="DriftDetected")].status}' "True" 60) || true

echo "  MC DriftDetected condition: ${DRIFT_COND:-not set}"

READY_STATUS=$(kubectl get machineconfig e2e-drift-mc-${E2E_RUN_ID} -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")
echo "  MC Ready condition: $READY_STATUS"

if [ "$DRIFT_COND" = "True" ]; then
    pass_test "OP-DA-01"
else
    fail_test "OP-DA-01" "DriftAlert did not mark MachineConfig as drifted"
fi

# =================================================================
# OP-DA-02: DriftAlert cleanup — delete alert, MC drift clears
# =================================================================
begin_test "OP-DA-02: DriftAlert cleanup"

# Delete the drift alert
kubectl delete driftalert e2e-drift-1 -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# Update MC spec to bump generation and trigger re-reconcile (clear drift flag)
kubectl patch machineconfig e2e-drift-mc-${E2E_RUN_ID} -n "$E2E_NAMESPACE" --type=merge \
    -p '{"spec":{"packages":[{"name":"vim"},{"name":"git"},{"name":"curl"},{"name":"wget"}]}}' 2>/dev/null

# Wait for MC to clear drift status (DriftDetected=False or condition removed)
echo "  Waiting for drift to clear..."
DRIFT_CLEARED=false
for i in $(seq 1 30); do
    DRIFT_COND=$(kubectl get machineconfig e2e-drift-mc-${E2E_RUN_ID} -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].status}' 2>/dev/null || echo "")
    if [ "$DRIFT_COND" = "False" ] || [ -z "$DRIFT_COND" ]; then
        echo "  MC DriftDetected after cleanup: ${DRIFT_COND:-removed} (after ${i}s)"
        DRIFT_CLEARED=true
        break
    fi
    sleep 1
done

if $DRIFT_CLEARED; then
    pass_test "OP-DA-02"
else
    fail_test "OP-DA-02" "Drift was not cleared after DriftAlert removal and spec change"
fi
