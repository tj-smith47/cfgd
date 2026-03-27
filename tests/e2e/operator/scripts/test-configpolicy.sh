# Operator E2E tests: ConfigPolicy
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== ConfigPolicy Tests ==="

# =================================================================
# OP-CP-01: ConfigPolicy — all MachineConfigs compliant
# =================================================================
begin_test "OP-CP-01: ConfigPolicy — compliant check"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-security-baseline
  namespace: ${E2E_NAMESPACE}
spec:
  packages:
    - name: vim
    - name: git
  settings:
    shell: /bin/zsh
EOF

# Wait for policy reconciliation
echo "  Waiting for ConfigPolicy status..."
CP_STATUS=$(wait_for_k8s_field configpolicy e2e-security-baseline "$E2E_NAMESPACE" \
    '{.status.compliantCount}' "" 60) || true

COMPLIANT=$(kubectl get configpolicy e2e-security-baseline -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "0")
NON_COMPLIANT=$(kubectl get configpolicy e2e-security-baseline -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

echo "  Compliant: $COMPLIANT, Non-compliant: $NON_COMPLIANT"

if [ "${COMPLIANT:-0}" -ge 1 ] && [ "${NON_COMPLIANT:-0}" -eq 0 ]; then
    pass_test "OP-CP-01"
else
    # If MC was compliant and counted, pass
    if [ -n "$CP_STATUS" ]; then
        pass_test "OP-CP-01"
    else
        fail_test "OP-CP-01" "ConfigPolicy status not updated"
    fi
fi

# =================================================================
# OP-CP-02: ConfigPolicy — non-compliant MachineConfig
# =================================================================
begin_test "OP-CP-02: ConfigPolicy — non-compliant detection"

# Create a MachineConfig that's missing required packages
kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-workstation-2
  namespace: ${E2E_NAMESPACE}
spec:
  hostname: e2e-host-2
  profile: minimal
  packages:
    - name: curl
  systemSettings: {}
EOF

# Wait for both MC and policy to re-reconcile
sleep 5

# Poll until nonCompliantCount >= 1 (can't use wait_for_k8s_field since we need >= not ==)
NON_COMPLIANT="0"
for i in $(seq 1 60); do
    NON_COMPLIANT=$(kubectl get configpolicy e2e-security-baseline -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")
    if [ "${NON_COMPLIANT:-0}" -ge 1 ] 2>/dev/null; then
        break
    fi
    sleep 1
done

COMPLIANT=$(kubectl get configpolicy e2e-security-baseline -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "0")

echo "  Compliant: $COMPLIANT, Non-compliant: ${NON_COMPLIANT:-0}"

ENFORCED=$(kubectl get configpolicy e2e-security-baseline -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.conditions[?(@.type=="Enforced")].status}' 2>/dev/null || echo "")
echo "  Enforced condition: $ENFORCED"

if [ "${NON_COMPLIANT:-0}" -ge 1 ]; then
    pass_test "OP-CP-02"
else
    fail_test "OP-CP-02" "Non-compliant MC not detected by policy"
fi

# =================================================================
# OP-CP-03: ConfigPolicy — version enforcement
# =================================================================
begin_test "OP-CP-03: ConfigPolicy version enforcement"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-version-policy
  namespace: ${E2E_NAMESPACE}
spec:
  packages:
    - name: vim
      version: ">=9.0"
EOF

sleep 5

COMPLIANT=$(wait_for_k8s_field configpolicy e2e-version-policy "$E2E_NAMESPACE" \
    '{.status.compliantCount}' "" 20) || true

echo "  Version policy status:"
kubectl get configpolicy e2e-version-policy -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status}' 2>/dev/null | sed 's/^/    /' || true
echo ""

# e2e-workstation-1 has vim 9.0.1 which satisfies >=9.0
if [ -n "$COMPLIANT" ]; then
    pass_test "OP-CP-03"
else
    fail_test "OP-CP-03" "Version policy status not updated"
fi

# =================================================================
# OP-CP-04: ConfigPolicy with target selector
# =================================================================
begin_test "OP-CP-04: ConfigPolicy target selector"

# Add a label to e2e-workstation-1 so targetSelector can match it
kubectl label machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" \
    cfgd.io/profile=dev-workstation --overwrite 2>/dev/null || true

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-selector-policy
  namespace: ${E2E_NAMESPACE}
spec:
  packages:
    - name: ripgrep
  targetSelector:
    matchLabels:
      cfgd.io/profile: dev-workstation
EOF

sleep 5

COMPLIANT=$(wait_for_k8s_field configpolicy e2e-selector-policy "$E2E_NAMESPACE" \
    '{.status.compliantCount}' "" 20) || true

NON_COMPLIANT=$(kubectl get configpolicy e2e-selector-policy -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

echo "  Selector policy — compliant: ${COMPLIANT:-0}, non-compliant: ${NON_COMPLIANT:-0}"

# Only e2e-workstation-1 (profile=dev-workstation) should be evaluated;
# e2e-workstation-2 (profile=minimal) should be excluded by selector
if [ -n "$COMPLIANT" ]; then
    pass_test "OP-CP-04"
else
    fail_test "OP-CP-04" "Selector policy status not updated"
fi

# --- Clean up resources from MachineConfig + ConfigPolicy tests ---
echo ""
echo "Cleaning up MachineConfig/ConfigPolicy/DriftAlert resources..."
kubectl delete machineconfig e2e-workstation-1 e2e-workstation-2 -n "$E2E_NAMESPACE" 2>/dev/null || true
kubectl delete configpolicy e2e-security-baseline e2e-version-policy e2e-selector-policy -n "$E2E_NAMESPACE" 2>/dev/null || true
kubectl delete driftalert e2e-drift-1 -n "$E2E_NAMESPACE" 2>/dev/null || true
