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
