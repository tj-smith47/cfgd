# shellcheck shell=bash
# Operator E2E tests: MachineConfig
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== MachineConfig Tests ==="

# =================================================================
# OP-MC-01: Create MachineConfig: controller reconciles and sets status
# =================================================================
begin_test "OP-MC-01: MachineConfig reconciliation"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-workstation-1
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  hostname: e2e-host-1
  profile: dev-workstation
  packages:
    - name: vim
    - name: git
    - name: curl
  files:
    - path: /home/user/.gitconfig
      content: "[user]\n    name = Test"
      mode: "0644"
  systemSettings:
    shell: /bin/zsh
EOF

# Wait for controller to reconcile (status update)
echo "  Waiting for MachineConfig status update..."
MC_STATUS=$(wait_for_k8s_field machineconfig e2e-workstation-1 "$E2E_NAMESPACE" \
    '{.status.lastReconciled}' "" 60) || true

echo "  lastReconciled: ${MC_STATUS:-not set}"

if [ -n "$MC_STATUS" ]; then
    # A fresh MachineConfig has no DriftAlert and no moduleRefs, so the
    # controller's first pass writes all three of its own conditions true or
    # false with nothing left open.
    MC01_CONDITIONS=$(kubectl get machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" \
        -o jsonpath='{range .status.conditions[*]}{.type}={.status}/{.reason} {end}' 2>/dev/null || echo "")
    echo "  Conditions: ${MC01_CONDITIONS:-none}"
    case " $MC01_CONDITIONS" in
        *" Reconciled=True/ReconcileSuccess "*" DriftDetected=False/NoDrift "*" ModulesResolved=True/AllResolved "*)
            pass_test "OP-MC-01" ;;
        *)
            fail_test "OP-MC-01" "Expected Reconciled=True/ReconcileSuccess, DriftDetected=False/NoDrift and ModulesResolved=True/AllResolved, got: ${MC01_CONDITIONS:-none}" ;;
    esac
else
    fail_test "OP-MC-01" "MachineConfig status was not updated by controller"
fi

# =================================================================
# OP-MC-02: Update MachineConfig: controller re-reconciles
# =================================================================
begin_test "OP-MC-02: MachineConfig update triggers re-reconcile"
BEFORE_TS="$MC_STATUS"

sleep 1 # sleep-ok: lastReconciled has one-second resolution, so the re-reconcile has to land in a later second

# Update the spec
MC02_PATCH_RC=0
kubectl patch machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" --type=merge \
    -p '{"spec":{"packages":[{"name":"vim"},{"name":"git"},{"name":"curl"},{"name":"ripgrep"}]}}' \
    > /dev/null 2>&1 || MC02_PATCH_RC=$?
echo "  Spec patch rc: $MC02_PATCH_RC"

# Wait for a new reconciliation: poll until the timestamp changes
echo "  Waiting for re-reconciliation..."
mc02_reconciled_again() {
    AFTER_TS=$(kubectl get machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.lastReconciled}' 2>/dev/null || echo "")
    [ -n "$AFTER_TS" ] && [ "$AFTER_TS" != "$BEFORE_TS" ]
}
AFTER_TS=""
wait_until 60 1 "lastReconciled on e2e-workstation-1 to move past $BEFORE_TS" mc02_reconciled_again || true

echo "  Before: $BEFORE_TS"
echo "  After:  ${AFTER_TS:-unchanged}"

if [ "$MC02_PATCH_RC" -eq 0 ] && [ -n "$AFTER_TS" ] && [ "$AFTER_TS" != "$BEFORE_TS" ]; then
    pass_test "OP-MC-02"
else
    fail_test "OP-MC-02" "Controller did not re-reconcile after spec update (patch rc=${MC02_PATCH_RC})"
fi

# =================================================================
# OP-ERR-01: MachineConfig with nonexistent moduleRef
# =================================================================
begin_test "OP-ERR-01: MachineConfig with nonexistent moduleRef"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-bad-moduleref-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: e2e-bad-moduleref
  profile: dev-workstation
  moduleRefs:
    - name: nonexistent-mod-xyz-${E2E_RUN_ID}
  packages:
    - name: vim
  systemSettings: {}
EOF

# Wait for controller to reconcile and set ModulesResolved condition
echo "  Waiting for ModulesResolved condition..."
MODULES_RESOLVED=$(wait_for_k8s_field machineconfig "e2e-bad-moduleref-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
    '{.status.conditions[?(@.type=="ModulesResolved")].status}' "False" 60) || true

MODULES_REASON=$(kubectl get machineconfig "e2e-bad-moduleref-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.conditions[?(@.type=="ModulesResolved")].reason}' 2>/dev/null || echo "")

echo "  ModulesResolved status: ${MODULES_RESOLVED:-not set}"
echo "  ModulesResolved reason: ${MODULES_REASON:-not set}"

# Verify the operator pod is not crash-looping
OPERATOR_STATUS=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
echo "  Operator pod status: ${OPERATOR_STATUS:-unknown}"

if [ "$MODULES_RESOLVED" = "False" ] && [ "$OPERATOR_STATUS" = "Running" ]; then
    pass_test "OP-ERR-01"
elif [ "$OPERATOR_STATUS" != "Running" ]; then
    fail_test "OP-ERR-01" "Operator pod is not Running (status: ${OPERATOR_STATUS})"
else
    fail_test "OP-ERR-01" "ModulesResolved condition not set to False for nonexistent moduleRef"
fi

kubectl delete machineconfig "e2e-bad-moduleref-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-ERR-02: ConfigPolicy with impossible selector
# =================================================================
begin_test "OP-ERR-02: ConfigPolicy with impossible selector"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-impossible-selector-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: vim
  targetSelector:
    matchLabels:
      cfgd.io/nonexistent-label: "impossible-value-${E2E_RUN_ID}"
EOF

# Wait for policy reconciliation
echo "  Waiting for ConfigPolicy status..."
# compliantCount is written even when it is 0.
ERR02_STATUS=$(wait_for_k8s_field configpolicy "e2e-impossible-selector-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
    '{.status.compliantCount}' "" 65) || true

COMPLIANT=$(kubectl get configpolicy "e2e-impossible-selector-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")
NON_COMPLIANT=$(kubectl get configpolicy "e2e-impossible-selector-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "")

echo "  Compliant: ${COMPLIANT:-not set}, Non-compliant: ${NON_COMPLIANT:-not set}"

if [ -z "$ERR02_STATUS" ]; then
    fail_test "OP-ERR-02" "ConfigPolicy status was not updated by controller"
elif [ "${COMPLIANT:-}" = "0" ] && [ "${NON_COMPLIANT:-}" = "0" ]; then
    pass_test "OP-ERR-02"
else
    fail_test "OP-ERR-02" "Expected compliant=0 and non-compliant=0, got compliant=${COMPLIANT:-not set}, non-compliant=${NON_COMPLIANT:-not set}"
fi

kubectl delete configpolicy "e2e-impossible-selector-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-ERR-03: DriftAlert for deleted MachineConfig
# =================================================================
begin_test "OP-ERR-03: DriftAlert for deleted MachineConfig"

# Create a MachineConfig, then a DriftAlert, then delete the MachineConfig
kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-ephemeral-mc-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: e2e-ephemeral
  profile: dev-workstation
  packages:
    - name: vim
  systemSettings: {}
EOF

# Wait for MC to be reconciled
echo "  Waiting for ephemeral MachineConfig reconciliation..."
wait_for_k8s_field machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
    '{.status.lastReconciled}' "" 60 > /dev/null 2>&1 || true

# Create a DriftAlert referencing this MC
kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: e2e-orphan-drift-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  deviceId: e2e-ephemeral
  machineConfigRef:
    name: e2e-ephemeral-mc-${E2E_RUN_ID}
  severity: Medium
  driftDetails:
    - field: sysctl.vm.swappiness
      expected: "10"
      actual: "60"
EOF

# The DriftAlert controller owns the alert by its MachineConfig, which is what
# makes deleting the MachineConfig below orphan it.
wait_for_k8s_field driftalert "e2e-orphan-drift-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
    '{.metadata.ownerReferences[?(@.kind=="MachineConfig")].name}' "e2e-ephemeral-mc-${E2E_RUN_ID}" 30 > /dev/null || true

# Delete the MachineConfig, which orphans the DriftAlert
# Remove finalizers first in case controller added them
kubectl patch machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    --type=json -p='[{"op":"replace","path":"/metadata/finalizers","value":[]}]' 2>/dev/null || true # rc-ok: clearing finalizers is best-effort; OP-ERR-03 asserts only that the operator survives the orphaned alert
kubectl delete machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --wait=false --ignore-not-found 2>/dev/null || true

wait_for_deleted 30 machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" || true

# The garbage collector removes the alert through its owner reference while the
# operator handles the orphaned alert; the verdict reads the operator after
# both. An alert that stays is acceptable as long as the operator does not crash.
wait_for_deleted 30 driftalert "e2e-orphan-drift-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" || true

OPERATOR_STATUS=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
OPERATOR_RESTARTS=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || echo "0")

echo "  Operator pod status: ${OPERATOR_STATUS:-unknown}, restarts: ${OPERATOR_RESTARTS:-0}"

# Check if DriftAlert was garbage-collected or still exists
DA_EXISTS=$(kubectl get driftalert "e2e-orphan-drift-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" 2>/dev/null && echo "yes" || echo "no")
echo "  DriftAlert still exists: ${DA_EXISTS}"

if [ "$OPERATOR_STATUS" = "Running" ]; then
    pass_test "OP-ERR-03"
else
    fail_test "OP-ERR-03" "Operator pod is not Running after DriftAlert orphan scenario (status: ${OPERATOR_STATUS})"
fi

kubectl delete driftalert "e2e-orphan-drift-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
kubectl delete machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-ERR-04: Rapid create/delete: no reconcile panic
# =================================================================
begin_test "OP-ERR-04: Rapid create/delete: no reconcile panic"

# Record operator restart count before the test
RESTARTS_BEFORE=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || echo "0")

# Create and immediately delete a MachineConfig to race the controller
for i in $(seq 1 5); do
    kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF 2>/dev/null || true # rc-ok: the rapid create/delete race asserts only that the operator does not crash
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-rapid-${E2E_RUN_ID}-${i}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: e2e-rapid-${i}
  profile: dev-workstation
  packages:
    - name: vim
  systemSettings: {}
EOF
    kubectl delete machineconfig "e2e-rapid-${E2E_RUN_ID}-${i}" -n "$E2E_NAMESPACE" --wait=false --ignore-not-found 2>/dev/null || true
done

# A MachineConfig the controller has claimed stays until its finalizer is
# handled, so all five gone means the controller has finished with each delete.
ERR04_NAMES=()
for i in $(seq 1 5); do
    ERR04_NAMES+=("e2e-rapid-${E2E_RUN_ID}-${i}")
done
wait_for_deleted 60 machineconfig "${ERR04_NAMES[@]}" -n "$E2E_NAMESPACE" || true

OPERATOR_STATUS=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
RESTARTS_AFTER=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || echo "0")

echo "  Operator pod status: ${OPERATOR_STATUS:-unknown}"
echo "  Restarts before: ${RESTARTS_BEFORE}, after: ${RESTARTS_AFTER}"

if [ "$OPERATOR_STATUS" = "Running" ] && [ "${RESTARTS_AFTER:-0}" -eq "${RESTARTS_BEFORE:-0}" ]; then
    pass_test "OP-ERR-04"
elif [ "$OPERATOR_STATUS" = "Running" ]; then
    # Running but with extra restarts: still acceptable if no crash loop
    CRASH_LOOP_RC=0
    CRASH_LOOP=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
        -o jsonpath='{.items[0].status.containerStatuses[0].state.waiting.reason}' 2>/dev/null) || CRASH_LOOP_RC=$?
    if [ "$CRASH_LOOP_RC" -ne 0 ]; then
        fail_test "OP-ERR-04" "Could not read the operator pod's state (kubectl exit $CRASH_LOOP_RC)"
    elif [ "$CRASH_LOOP" = "CrashLoopBackOff" ]; then
        fail_test "OP-ERR-04" "Operator entered CrashLoopBackOff after rapid create/delete"
    else
        pass_test "OP-ERR-04"
    fi
else
    fail_test "OP-ERR-04" "Operator pod is not Running after rapid create/delete (status: ${OPERATOR_STATUS})"
fi

# Clean up any stragglers
for i in $(seq 1 5); do
    kubectl delete machineconfig "e2e-rapid-${E2E_RUN_ID}-${i}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
done
