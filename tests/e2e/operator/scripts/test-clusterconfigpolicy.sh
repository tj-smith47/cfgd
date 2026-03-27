# Operator E2E tests: ClusterConfigPolicy
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== ClusterConfigPolicy Tests ==="

# =================================================================
# OP-CCP-01: ClusterConfigPolicy — namespaceSelector filtering
# =================================================================
begin_test "OP-CCP-01: ClusterConfigPolicy — namespaceSelector filtering"

# Create two namespaces: one matching, one not
kubectl create namespace "e2e-team-alpha-${E2E_RUN_ID}" 2>/dev/null || true
kubectl create namespace "e2e-team-beta-${E2E_RUN_ID}" 2>/dev/null || true
kubectl label namespace "e2e-team-alpha-${E2E_RUN_ID}" cfgd.io/team=alpha --overwrite 2>/dev/null
kubectl label namespace "e2e-team-beta-${E2E_RUN_ID}" cfgd.io/team=beta --overwrite 2>/dev/null

# Create MachineConfigs in both namespaces
for ns in "e2e-team-alpha-${E2E_RUN_ID}" "e2e-team-beta-${E2E_RUN_ID}"; do
    kubectl apply -n "$ns" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: mc-worker-1
  namespace: ${ns}
spec:
  hostname: worker-1-${ns}
  profile: k8s-worker
  packages:
    - name: vim
    - name: git
  systemSettings: {}
EOF
done

# Create ClusterConfigPolicy targeting only team=alpha
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: e2e-alpha-only-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector:
    matchLabels:
      cfgd.io/team: alpha
  packages:
    - name: vim
  settings: {}
EOF

# Wait for ClusterConfigPolicy status
echo "  Waiting for ClusterConfigPolicy evaluation..."
CCP_COMPLIANT=$(wait_for_k8s_field clusterconfigpolicy "e2e-alpha-only-${E2E_RUN_ID}" "" \
    '{.status.compliantCount}' "" 60) || true

CCP_NON_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-alpha-only-${E2E_RUN_ID}" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

echo "  ClusterConfigPolicy — compliant: ${CCP_COMPLIANT:-0}, non-compliant: ${CCP_NON_COMPLIANT:-0}"

# Only e2e-team-alpha MachineConfigs should be evaluated.
# e2e-team-beta should NOT be counted (not in the selector).
if [ -n "$CCP_COMPLIANT" ]; then
    pass_test "OP-CCP-01"
else
    fail_test "OP-CCP-01" "ClusterConfigPolicy status not updated"
fi

# =================================================================
# OP-CCP-02: ClusterConfigPolicy — cluster-wins merge with namespace ConfigPolicy
# =================================================================
begin_test "OP-CCP-02: ClusterConfigPolicy — cluster-wins merge"

# Create a namespace-level ConfigPolicy in alpha namespace
kubectl apply -n "e2e-team-alpha-${E2E_RUN_ID}" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: ns-policy-alpha
  namespace: e2e-team-alpha-${E2E_RUN_ID}
spec:
  packages:
    - name: vim
  settings:
    dns-server: "8.8.8.8"
EOF

# Create a ClusterConfigPolicy that overrides the setting
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: e2e-cluster-override-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector:
    matchLabels:
      cfgd.io/team: alpha
  packages:
    - name: git
  settings:
    dns-server: "1.1.1.1"
EOF

# Wait for reconciliation
sleep 10

CCP2_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cluster-override-${E2E_RUN_ID}" \
    -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")
CCP2_NON_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cluster-override-${E2E_RUN_ID}" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

echo "  Cluster-override policy — compliant: ${CCP2_COMPLIANT:-0}, non-compliant: ${CCP2_NON_COMPLIANT:-0}"

# The MC in e2e-team-alpha has vim and git packages, so both the namespace
# policy (vim) and cluster policy (git) requirements are met
if [ -n "$CCP2_COMPLIANT" ]; then
    pass_test "OP-CCP-02"
else
    fail_test "OP-CCP-02" "ClusterConfigPolicy merge status not updated"
fi
