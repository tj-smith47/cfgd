# shellcheck shell=bash
# Operator E2E tests: ConfigPolicy
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== ConfigPolicy Tests ==="

# =================================================================
# OP-CP-01: ConfigPolicy: all MachineConfigs compliant
# =================================================================
begin_test "OP-CP-01: ConfigPolicy: compliant check"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-security-baseline
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  packages:
    - name: vim
    - name: git
  settings:
    shell: /bin/zsh
EOF

# e2e-workstation-1 is the namespace's one MachineConfig, and it lists vim and
# git and sets shell to /bin/zsh.
echo "  Waiting for ConfigPolicy status..."
CP01_COUNTS=$(wait_for_k8s_field configpolicy e2e-security-baseline "$E2E_NAMESPACE" \
    '{.status.compliantCount}/{.status.nonCompliantCount}' "1/0" 60) || true

echo "  Compliant/non-compliant: ${CP01_COUNTS:-not set}"

if [ "$CP01_COUNTS" = "1/0" ]; then
    pass_test "OP-CP-01"
else
    fail_test "OP-CP-01" "Expected compliant/non-compliant 1/0 for e2e-workstation-1, got ${CP01_COUNTS:-not set}"
fi

# =================================================================
# OP-CP-02: ConfigPolicy: non-compliant MachineConfig
# =================================================================
begin_test "OP-CP-02: ConfigPolicy: non-compliant detection"

# Create a MachineConfig that's missing required packages
kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-workstation-2
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  hostname: e2e-host-2
  profile: minimal
  packages:
    - name: curl
  systemSettings: {}
EOF

# e2e-workstation-2 lists neither vim nor git, so the policy counts it against
# e2e-workstation-1's pass.
CP02_COUNTS=$(wait_for_k8s_field configpolicy e2e-security-baseline "$E2E_NAMESPACE" \
    '{.status.compliantCount}/{.status.nonCompliantCount}' "1/1" 65) || true

echo "  Compliant/non-compliant: ${CP02_COUNTS:-not set}"

ENFORCED=$(kubectl get configpolicy e2e-security-baseline -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.conditions[?(@.type=="Enforced")].status}' 2>/dev/null || echo "")
echo "  Enforced condition: $ENFORCED"

if [ "$CP02_COUNTS" = "1/1" ]; then
    pass_test "OP-CP-02"
else
    fail_test "OP-CP-02" "Expected compliant/non-compliant 1/1 once e2e-workstation-2 exists, got ${CP02_COUNTS:-not set}"
fi

# =================================================================
# OP-CP-03: ConfigPolicy: version enforcement
# =================================================================
begin_test "OP-CP-03: ConfigPolicy version enforcement"

# A version pin is met only by a version the machine reported, so
# e2e-workstation-1 reports vim 9.0.1 the way a device check-in would.
CP03_SEED_RC=0
kubectl patch machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" --subresource=status --type=merge \
    -p '{"status":{"packageVersions":{"apt/vim":"9.0.1"}}}' > /dev/null 2>&1 || CP03_SEED_RC=$?
echo "  packageVersions seed rc: $CP03_SEED_RC"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-version-policy
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  packages:
    - name: vim
      version: ">=9.0"
EOF

# e2e-workstation-1's reported vim 9.0.1 meets >=9.0; e2e-workstation-2 lists
# no vim.
CP03_COUNTS=$(wait_for_k8s_field configpolicy e2e-version-policy "$E2E_NAMESPACE" \
    '{.status.compliantCount}/{.status.nonCompliantCount}' "1/1" 65) || true

echo "  Version policy status:"
kubectl get configpolicy e2e-version-policy -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status}' 2>/dev/null | sed 's/^/    /' || true
echo ""

if [ "$CP03_SEED_RC" -eq 0 ] && [ "$CP03_COUNTS" = "1/1" ]; then
    pass_test "OP-CP-03"
else
    fail_test "OP-CP-03" "Expected compliant/non-compliant 1/1 with vim 9.0.1 seeded (seed rc=${CP03_SEED_RC}), got ${CP03_COUNTS:-not set}"
fi

# =================================================================
# OP-CP-04: ConfigPolicy with target selector
# =================================================================
begin_test "OP-CP-04: ConfigPolicy target selector"

# Add a label to e2e-workstation-1 so targetSelector can match it
ensure_label machineconfig e2e-workstation-1 -n "$E2E_NAMESPACE" \
    cfgd.io/profile=dev-workstation --overwrite

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-selector-policy
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  packages:
    - name: ripgrep
  targetSelector:
    matchLabels:
      cfgd.io/profile: dev-workstation
EOF

# The selector takes e2e-workstation-1 alone, which gained ripgrep in
# OP-MC-02; counting e2e-workstation-2 (no ripgrep) would show as a
# non-compliant machine.
CP04_COUNTS=$(wait_for_k8s_field configpolicy e2e-selector-policy "$E2E_NAMESPACE" \
    '{.status.compliantCount}/{.status.nonCompliantCount}' "1/0" 65) || true

echo "  Selector policy compliant/non-compliant: ${CP04_COUNTS:-not set}"

if [ "$CP04_COUNTS" = "1/0" ]; then
    pass_test "OP-CP-04"
else
    fail_test "OP-CP-04" "Expected compliant/non-compliant 1/0 for the selected e2e-workstation-1 alone, got ${CP04_COUNTS:-not set}"
fi

# --- Clean up resources from MachineConfig + ConfigPolicy tests ---
echo ""
echo "Cleaning up MachineConfig/ConfigPolicy/DriftAlert resources..."
kubectl delete machineconfig e2e-workstation-1 e2e-workstation-2 -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
kubectl delete configpolicy e2e-security-baseline e2e-version-policy e2e-selector-policy -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
kubectl delete driftalert e2e-drift-1 -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
