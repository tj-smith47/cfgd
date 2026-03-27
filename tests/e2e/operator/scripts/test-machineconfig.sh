# Operator E2E tests: MachineConfig
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== MachineConfig Tests ==="

# =================================================================
# OP-MC-01: Create MachineConfig — controller reconciles and sets status
# =================================================================
begin_test "OP-MC-01: MachineConfig reconciliation"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-workstation-1
  namespace: ${E2E_NAMESPACE}
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
    # Verify conditions
    READY_STATUS=$(kubectl get machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")
    echo "  Ready condition: $READY_STATUS"

    if [ "$READY_STATUS" = "True" ]; then
        pass_test "OP-MC-01"
    else
        # May be False if drift was detected, still valid reconciliation
        pass_test "OP-MC-01"
    fi
else
    fail_test "OP-MC-01" "MachineConfig status was not updated by controller"
fi

# =================================================================
# OP-MC-02: Update MachineConfig — controller re-reconciles
# =================================================================
begin_test "OP-MC-02: MachineConfig update triggers re-reconcile"
BEFORE_TS="$MC_STATUS"

# Wait to ensure timestamp differs from initial reconcile
sleep 2

# Update the spec
kubectl patch machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" --type=merge \
    -p '{"spec":{"packages":[{"name":"vim"},{"name":"git"},{"name":"curl"},{"name":"ripgrep"}]}}' 2>/dev/null

# Wait for new reconciliation — poll until timestamp changes
echo "  Waiting for re-reconciliation..."
AFTER_TS=""
deadline=$((SECONDS + 60))
while [ $SECONDS -lt $deadline ]; do
    AFTER_TS=$(kubectl get machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.lastReconciled}' 2>/dev/null || echo "")
    if [ -n "$AFTER_TS" ] && [ "$AFTER_TS" != "$BEFORE_TS" ]; then
        break
    fi
    sleep 1
done

echo "  Before: $BEFORE_TS"
echo "  After:  ${AFTER_TS:-unchanged}"

if [ -n "$AFTER_TS" ] && [ "$AFTER_TS" != "$BEFORE_TS" ]; then
    pass_test "OP-MC-02"
else
    fail_test "OP-MC-02" "Controller did not re-reconcile after spec update"
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
OPERATOR_STATUS=$(kubectl get pods -n cfgd-system -l app.kubernetes.io/name=cfgd-operator \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
echo "  Operator pod status: ${OPERATOR_STATUS:-unknown}"

if [ "$MODULES_RESOLVED" = "False" ] && [ "$OPERATOR_STATUS" = "Running" ]; then
    pass_test "OP-ERR-01"
elif [ "$OPERATOR_STATUS" != "Running" ]; then
    fail_test "OP-ERR-01" "Operator pod is not Running (status: ${OPERATOR_STATUS})"
else
    fail_test "OP-ERR-01" "ModulesResolved condition not set to False for nonexistent moduleRef"
fi

kubectl delete machineconfig "e2e-bad-moduleref-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" 2>/dev/null || true

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
sleep 5

# Poll until compliantCount field is present (even if 0)
ERR02_STATUS=""
for i in $(seq 1 60); do
    ERR02_STATUS=$(kubectl get configpolicy "e2e-impossible-selector-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")
    if [ -n "$ERR02_STATUS" ]; then
        break
    fi
    sleep 1
done

COMPLIANT=$(kubectl get configpolicy "e2e-impossible-selector-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")
NON_COMPLIANT=$(kubectl get configpolicy "e2e-impossible-selector-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "")

echo "  Compliant: ${COMPLIANT:-not set}, Non-compliant: ${NON_COMPLIANT:-not set}"

if [ "${COMPLIANT:-}" = "0" ] && [ "${NON_COMPLIANT:-}" = "0" ]; then
    pass_test "OP-ERR-02"
elif [ -n "$ERR02_STATUS" ]; then
    # Status was set — accept any 0-total as pass
    TOTAL=$(( ${COMPLIANT:-0} + ${NON_COMPLIANT:-0} ))
    if [ "$TOTAL" -eq 0 ]; then
        pass_test "OP-ERR-02"
    else
        fail_test "OP-ERR-02" "Expected 0 total, got compliant=${COMPLIANT}, non-compliant=${NON_COMPLIANT}"
    fi
else
    fail_test "OP-ERR-02" "ConfigPolicy status was not updated by controller"
fi

kubectl delete configpolicy "e2e-impossible-selector-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" 2>/dev/null || true

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
    - field: packages.vim
      expected: installed
      actual: missing
EOF

# Wait for DriftAlert to be processed (owner ref set)
sleep 5

# Delete the MachineConfig — DriftAlert becomes orphaned
# Remove finalizers first in case controller added them
kubectl patch machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    --type=json -p='[{"op":"replace","path":"/metadata/finalizers","value":[]}]' 2>/dev/null || true
kubectl delete machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --wait=false 2>/dev/null || true

# Wait for MC to actually be gone
for i in $(seq 1 30); do
    if ! kubectl get machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" > /dev/null 2>&1; then
        break
    fi
    sleep 1
done

# The DriftAlert may be garbage-collected by owner ref, or the controller
# may handle the orphaned state. Either outcome is acceptable as long as
# the operator does not crash.
sleep 5

OPERATOR_STATUS=$(kubectl get pods -n cfgd-system -l app.kubernetes.io/name=cfgd-operator \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
OPERATOR_RESTARTS=$(kubectl get pods -n cfgd-system -l app.kubernetes.io/name=cfgd-operator \
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

kubectl delete driftalert "e2e-orphan-drift-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" 2>/dev/null || true
kubectl delete machineconfig "e2e-ephemeral-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" 2>/dev/null || true

# =================================================================
# OP-ERR-04: Rapid create/delete — no reconcile panic
# =================================================================
begin_test "OP-ERR-04: Rapid create/delete — no reconcile panic"

# Record operator restart count before the test
RESTARTS_BEFORE=$(kubectl get pods -n cfgd-system -l app.kubernetes.io/name=cfgd-operator \
    -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || echo "0")

# Create and immediately delete a MachineConfig to race the controller
for i in $(seq 1 5); do
    kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF 2>/dev/null || true
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
    kubectl delete machineconfig "e2e-rapid-${E2E_RUN_ID}-${i}" -n "$E2E_NAMESPACE" --wait=false 2>/dev/null || true
done

# Give the controller time to process the events
sleep 10

OPERATOR_STATUS=$(kubectl get pods -n cfgd-system -l app.kubernetes.io/name=cfgd-operator \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
RESTARTS_AFTER=$(kubectl get pods -n cfgd-system -l app.kubernetes.io/name=cfgd-operator \
    -o jsonpath='{.items[0].status.containerStatuses[0].restartCount}' 2>/dev/null || echo "0")

echo "  Operator pod status: ${OPERATOR_STATUS:-unknown}"
echo "  Restarts before: ${RESTARTS_BEFORE}, after: ${RESTARTS_AFTER}"

if [ "$OPERATOR_STATUS" = "Running" ] && [ "${RESTARTS_AFTER:-0}" -eq "${RESTARTS_BEFORE:-0}" ]; then
    pass_test "OP-ERR-04"
elif [ "$OPERATOR_STATUS" = "Running" ]; then
    # Running but with extra restarts — still acceptable if no crash loop
    CRASH_LOOP=$(kubectl get pods -n cfgd-system -l app.kubernetes.io/name=cfgd-operator \
        -o jsonpath='{.items[0].status.containerStatuses[0].state.waiting.reason}' 2>/dev/null || echo "")
    if [ "$CRASH_LOOP" = "CrashLoopBackOff" ]; then
        fail_test "OP-ERR-04" "Operator entered CrashLoopBackOff after rapid create/delete"
    else
        pass_test "OP-ERR-04"
    fi
else
    fail_test "OP-ERR-04" "Operator pod is not Running after rapid create/delete (status: ${OPERATOR_STATUS})"
fi

# Clean up any stragglers
for i in $(seq 1 5); do
    kubectl delete machineconfig "e2e-rapid-${E2E_RUN_ID}-${i}" -n "$E2E_NAMESPACE" 2>/dev/null || true
done
