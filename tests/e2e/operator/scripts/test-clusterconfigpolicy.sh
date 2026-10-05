# shellcheck shell=bash
# Operator E2E tests: ClusterConfigPolicy
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== ClusterConfigPolicy Tests ==="

# =================================================================
# OP-CCP-01: ClusterConfigPolicy: namespaceSelector filtering
# =================================================================
begin_test "OP-CCP-01: ClusterConfigPolicy: namespaceSelector filtering"

# Create two namespaces: one matching, one not
ensure_namespace "e2e-team-alpha-${E2E_RUN_ID}"
ensure_namespace "e2e-team-beta-${E2E_RUN_ID}"
ensure_label namespace "e2e-team-alpha-${E2E_RUN_ID}" "$E2E_RUN_LABEL" cfgd.io/team=alpha --overwrite
ensure_label namespace "e2e-team-beta-${E2E_RUN_ID}" "$E2E_RUN_LABEL" cfgd.io/team=beta --overwrite

# Create MachineConfigs in both namespaces
for ns in "e2e-team-alpha-${E2E_RUN_ID}" "e2e-team-beta-${E2E_RUN_ID}"; do
    kubectl apply -n "$ns" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: mc-worker-1
  namespace: ${ns}
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  hostname: worker-1-${ns}
  profile: k8s-worker
  packages:
    - name: vim
    - name: git
  systemSettings: {}
EOF
done

# Create ClusterConfigPolicy targeting only team=alpha. Each selector here also
# names this run's label, so a concurrent run's namespaces stay out of the
# counts.
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: e2e-alpha-only-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector:
    matchLabels:
      ${E2E_RUN_LABEL_YAML}
      cfgd.io/team: alpha
  packages:
    - name: vim
  settings: {}
EOF

# Wait for ClusterConfigPolicy status
echo "  Waiting for ClusterConfigPolicy evaluation..."
# Only alpha's mc-worker-1 is counted, and it lists vim; beta's is outside
# the selector.
CCP01_COUNTS=$(wait_for_k8s_field clusterconfigpolicy "e2e-alpha-only-${E2E_RUN_ID}" "" \
    '{.status.compliantCount}/{.status.nonCompliantCount}' "1/0" 60) || true

echo "  ClusterConfigPolicy compliant/non-compliant: ${CCP01_COUNTS:-not set}"

if [ "$CCP01_COUNTS" = "1/0" ]; then
    pass_test "OP-CCP-01"
else
    fail_test "OP-CCP-01" "Expected compliant/non-compliant 1/0 for alpha's machine alone, got ${CCP01_COUNTS:-not set}"
fi

# =================================================================
# OP-CCP-02: ClusterConfigPolicy: cluster-wins merge with namespace ConfigPolicy
# =================================================================
begin_test "OP-CCP-02: ClusterConfigPolicy: cluster-wins merge"

# Create a namespace-level ConfigPolicy in alpha namespace
kubectl apply -n "e2e-team-alpha-${E2E_RUN_ID}" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: ns-policy-alpha
  namespace: e2e-team-alpha-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  packages:
    - name: vim
  settings:
    dns-server: "8.8.8.8"
EOF

# Alpha's machine sets the cluster's dns-server value, so it is compliant only
# when the cluster policy's 1.1.1.1 wins over the namespace policy's 8.8.8.8.
CCP02_PATCH_RC=0
kubectl patch machineconfig mc-worker-1 -n "e2e-team-alpha-${E2E_RUN_ID}" --type=merge \
    -p '{"spec":{"systemSettings":{"dns-server":"1.1.1.1"}}}' > /dev/null 2>&1 || CCP02_PATCH_RC=$?
echo "  dns-server patch rc: $CCP02_PATCH_RC"

# Create a ClusterConfigPolicy that overrides the setting
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: e2e-cluster-override-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector:
    matchLabels:
      ${E2E_RUN_LABEL_YAML}
      cfgd.io/team: alpha
  packages:
    - name: git
  settings:
    dns-server: "1.1.1.1"
EOF

# The merged requirements are vim (namespace), git (cluster) and dns-server
# 1.1.1.1 (cluster over namespace), all of which alpha's machine meets.
CCP02_COUNTS=$(wait_for_k8s_field clusterconfigpolicy "e2e-cluster-override-${E2E_RUN_ID}" "" \
    '{.status.compliantCount}/{.status.nonCompliantCount}' "1/0" 70) || true

echo "  Cluster-override policy compliant/non-compliant: ${CCP02_COUNTS:-not set}"

if [ "$CCP02_PATCH_RC" -eq 0 ] && [ "$CCP02_COUNTS" = "1/0" ]; then
    pass_test "OP-CCP-02"
else
    fail_test "OP-CCP-02" "Expected compliant/non-compliant 1/0 under the cluster's dns-server (patch rc=${CCP02_PATCH_RC}), got ${CCP02_COUNTS:-not set}"
fi

# =================================================================
# Multi-Namespace Policy Evaluation Tests (OP-NS-01 through OP-NS-06)
# =================================================================

echo ""
echo "=== Multi-Namespace Policy Tests ==="

NS_A="e2e-ns-a-${E2E_RUN_ID}"
NS_B="e2e-ns-b-${E2E_RUN_ID}"

# cross_ns_counts <compliant/non-compliant> <timeout_s>: wait for the
# e2e-cross-ns ClusterConfigPolicy to report those counts; prints the last read.
cross_ns_counts() {
    wait_for_k8s_field clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" "" \
        '{.status.compliantCount}/{.status.nonCompliantCount}' "$1" "$2"
}

# --- Setup: create two ephemeral namespaces with labels ---
ensure_namespace "$NS_A"
ensure_namespace "$NS_B"
ensure_label namespace "$NS_A" "$E2E_RUN_LABEL" cfgd.io/team=frontend --overwrite
ensure_label namespace "$NS_B" "$E2E_RUN_LABEL" cfgd.io/team=frontend --overwrite

# Create MachineConfigs in both namespaces
kubectl apply -n "$NS_A" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: mc-ns-a
  namespace: ${NS_A}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: host-ns-a
  profile: worker
  packages:
    - name: vim
    - name: git
    - name: curl
  systemSettings:
    shell: /bin/bash
EOF

kubectl apply -n "$NS_B" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: mc-ns-b
  namespace: ${NS_B}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: host-ns-b
  profile: worker
  packages:
    - name: vim
    - name: git
  systemSettings:
    shell: /bin/bash
EOF

# =================================================================
# OP-NS-01: ConfigPolicy in ns-a does not affect ns-b
# =================================================================
begin_test "OP-NS-01: ConfigPolicy in ns-a does not affect ns-b"

kubectl apply -n "$NS_A" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: ns-a-only-policy
  namespace: ${NS_A}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: curl
  settings: {}
EOF

# mc-ns-a lists curl. mc-ns-b does not, so counting it would show as a
# non-compliant machine.
echo "  Waiting for ConfigPolicy in ns-a to evaluate..."
NS01_COUNTS=$(wait_for_k8s_field configpolicy ns-a-only-policy "$NS_A" \
    '{.status.compliantCount}/{.status.nonCompliantCount}' "1/0" 60) || true

echo "  ns-a policy compliant/non-compliant: ${NS01_COUNTS:-not set}"

if [ "$NS01_COUNTS" = "1/0" ]; then
    pass_test "OP-NS-01"
else
    fail_test "OP-NS-01" "Expected compliant/non-compliant 1/0 for mc-ns-a alone, got ${NS01_COUNTS:-not set}"
fi

# =================================================================
# OP-NS-02: ClusterConfigPolicy spans namespaces
# =================================================================
begin_test "OP-NS-02: ClusterConfigPolicy spans namespaces"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: e2e-cross-ns-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector:
    matchLabels:
      ${E2E_RUN_LABEL_YAML}
      cfgd.io/team: frontend
  packages:
    - name: vim
  settings: {}
EOF

# Both namespaces carry team=frontend. mc-ns-a meets vim plus ns-a's curl, and
# mc-ns-b meets vim.
echo "  Waiting for ClusterConfigPolicy cross-namespace evaluation..."
NS02_COUNTS=$(cross_ns_counts "2/0" 60) || true
echo "  Cross-ns policy compliant/non-compliant: ${NS02_COUNTS:-not set}"

if [ "$NS02_COUNTS" = "2/0" ]; then
    pass_test "OP-NS-02"
else
    fail_test "OP-NS-02" "Expected compliant/non-compliant 2/0 across ns-a and ns-b, got ${NS02_COUNTS:-not set}"
fi

# =================================================================
# OP-NS-03: Namespace selector filtering: unlabel ns-b
# =================================================================
begin_test "OP-NS-03: Namespace selector filtering"

# Remove the team label from ns-b so it no longer matches the selector
ensure_label namespace "$NS_B" cfgd.io/team-

# Only ns-a matches once the controller re-evaluates.
echo "  Waiting for ClusterConfigPolicy to re-evaluate after unlabeling ns-b..."
NS03_COUNTS=$(cross_ns_counts "1/0" 70) || true
echo "  After unlabeling ns-b, compliant/non-compliant: ${NS03_COUNTS:-not set}"

if [ "$NS03_COUNTS" = "1/0" ]; then
    pass_test "OP-NS-03"
else
    fail_test "OP-NS-03" "Expected compliant/non-compliant 1/0 with ns-b unlabelled, got ${NS03_COUNTS:-not set}"
fi

# Restore the label for subsequent tests
ensure_label namespace "$NS_B" cfgd.io/team=frontend --overwrite

# =================================================================
# OP-NS-04: Policy priority resolution: both namespace and cluster
# =================================================================
begin_test "OP-NS-04: Policy priority resolution"

# ns-a already has a namespace-level ConfigPolicy (ns-a-only-policy requiring curl)
# and a ClusterConfigPolicy (e2e-cross-ns requiring vim).
# Both should evaluate mc-ns-a independently.

# The cluster policy counts ns-b again once the controller sees the restored
# label; the namespace policy still counts mc-ns-a alone.
echo "  Waiting for the cluster policy to count ns-b again..."
NS04_CCP_COUNTS=$(cross_ns_counts "2/0" 70) || true
NS04_NS_COUNTS=$(kubectl get configpolicy ns-a-only-policy -n "$NS_A" \
    -o jsonpath='{.status.compliantCount}/{.status.nonCompliantCount}' 2>/dev/null || echo "")

echo "  Namespace policy compliant/non-compliant: ${NS04_NS_COUNTS:-not set}"
echo "  Cluster policy compliant/non-compliant: ${NS04_CCP_COUNTS:-not set}"

if [ "$NS04_NS_COUNTS" = "1/0" ] && [ "$NS04_CCP_COUNTS" = "2/0" ]; then
    pass_test "OP-NS-04"
else
    fail_test "OP-NS-04" "Expected namespace policy 1/0 and cluster policy 2/0, got ns=${NS04_NS_COUNTS:-not set}, cluster=${NS04_CCP_COUNTS:-not set}"
fi

# =================================================================
# OP-NS-05: ClusterConfigPolicy compliance counting
# =================================================================
begin_test "OP-NS-05: ClusterConfigPolicy compliance counting"

# With both namespaces labelled team=frontend, the cluster policy counts both
# machines, each compliant.
NS05_COUNTS=$(cross_ns_counts "2/0" 30) || true
echo "  Cross-namespace compliant/non-compliant: ${NS05_COUNTS:-not set}"

if [ "$NS05_COUNTS" = "2/0" ]; then
    pass_test "OP-NS-05"
else
    fail_test "OP-NS-05" "Expected compliant/non-compliant 2/0 across ns-a and ns-b, got ${NS05_COUNTS:-not set}"
fi

# =================================================================
# OP-NS-06: Namespace deletion cleanup
# =================================================================
begin_test "OP-NS-06: Namespace deletion cleanup"

# Delete ns-a; its MachineConfig goes with it
kubectl delete namespace "$NS_A" --wait=false --ignore-not-found 2>/dev/null || true

echo "  Waiting for namespace $NS_A deletion to propagate..."
wait_for_deleted 60 namespace "$NS_A" || true

# The controller re-evaluates once mc-ns-a goes with its namespace, leaving
# mc-ns-b alone.
NS06_COUNTS=$(cross_ns_counts "1/0" 70) || true
echo "  After deleting ns-a, compliant/non-compliant: ${NS06_COUNTS:-not set}"

if [ "$NS06_COUNTS" = "1/0" ]; then
    pass_test "OP-NS-06"
else
    fail_test "OP-NS-06" "Expected compliant/non-compliant 1/0 with mc-ns-b alone, got ${NS06_COUNTS:-not set}"
fi

# --- Clean up multi-namespace test resources ---
echo ""
echo "Cleaning up multi-namespace policy test resources..."
kubectl delete namespace "$NS_A" --ignore-not-found --wait=false 2>/dev/null || true
kubectl delete namespace "$NS_B" --ignore-not-found --wait=false 2>/dev/null || true
kubectl delete clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
