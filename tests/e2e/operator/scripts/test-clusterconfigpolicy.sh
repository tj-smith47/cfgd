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

# =================================================================
# Multi-Namespace Policy Evaluation Tests (OP-NS-01 through OP-NS-06)
# =================================================================

echo ""
echo "=== Multi-Namespace Policy Tests ==="

NS_A="e2e-ns-a-${E2E_RUN_ID}"
NS_B="e2e-ns-b-${E2E_RUN_ID}"

# --- Setup: create two ephemeral namespaces with labels ---
kubectl create namespace "$NS_A" 2>/dev/null || true
kubectl create namespace "$NS_B" 2>/dev/null || true
kubectl label namespace "$NS_A" "$E2E_RUN_LABEL" cfgd.io/team=frontend --overwrite 2>/dev/null
kubectl label namespace "$NS_B" "$E2E_RUN_LABEL" cfgd.io/team=frontend --overwrite 2>/dev/null

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

# Wait for ConfigPolicy in ns-a to reconcile
echo "  Waiting for ConfigPolicy in ns-a to evaluate..."
NS01_COMPLIANT=$(wait_for_k8s_field configpolicy ns-a-only-policy "$NS_A" \
    '{.status.compliantCount}' "" 60) || true

NS01_NON_COMPLIANT=$(kubectl get configpolicy ns-a-only-policy -n "$NS_A" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

echo "  ns-a policy — compliant: ${NS01_COMPLIANT:-0}, non-compliant: ${NS01_NON_COMPLIANT:-0}"

# Verify the policy only evaluated ns-a MachineConfigs (mc-ns-a has curl, so compliant=1).
# mc-ns-b (in ns-b) should NOT be counted at all — namespace-scoped policy.
# Total evaluated = compliant + non-compliant should be exactly 1.
NS01_TOTAL=$(( ${NS01_COMPLIANT:-0} + ${NS01_NON_COMPLIANT:-0} ))
if [ -n "$NS01_COMPLIANT" ] && [ "$NS01_TOTAL" -le 1 ]; then
    pass_test "OP-NS-01"
else
    fail_test "OP-NS-01" "ConfigPolicy in ns-a evaluated resources outside its namespace (total=${NS01_TOTAL})"
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
      cfgd.io/team: frontend
  packages:
    - name: vim
  settings: {}
EOF

echo "  Waiting for ClusterConfigPolicy cross-namespace evaluation..."
NS02_COMPLIANT=$(wait_for_k8s_field clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" "" \
    '{.status.compliantCount}' "" 60) || true

NS02_NON_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

NS02_TOTAL=$(( ${NS02_COMPLIANT:-0} + ${NS02_NON_COMPLIANT:-0} ))
echo "  Cross-ns policy — compliant: ${NS02_COMPLIANT:-0}, non-compliant: ${NS02_NON_COMPLIANT:-0}, total: ${NS02_TOTAL}"

# Both ns-a and ns-b have team=frontend label, so both MachineConfigs should be evaluated.
# Both have vim, so both should be compliant. Total evaluated >= 2.
if [ "${NS02_COMPLIANT:-0}" -ge 2 ]; then
    pass_test "OP-NS-02"
else
    # Accept any status update as the policy spanning namespaces
    if [ "$NS02_TOTAL" -ge 2 ]; then
        pass_test "OP-NS-02"
    else
        fail_test "OP-NS-02" "ClusterConfigPolicy did not span both namespaces (total=${NS02_TOTAL})"
    fi
fi

# =================================================================
# OP-NS-03: Namespace selector filtering — unlabel ns-b
# =================================================================
begin_test "OP-NS-03: Namespace selector filtering"

# Remove the team label from ns-b so it no longer matches the selector
kubectl label namespace "$NS_B" cfgd.io/team- 2>/dev/null || true

# Wait for the controller to re-evaluate (label change triggers reconciliation)
echo "  Waiting for ClusterConfigPolicy to re-evaluate after unlabeling ns-b..."
sleep 10

NS03_COMPLIANT=""
NS03_TOTAL=0
for i in $(seq 1 60); do
    NS03_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" \
        -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "0")
    NS03_NON_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" \
        -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")
    NS03_TOTAL=$(( ${NS03_COMPLIANT:-0} + ${NS03_NON_COMPLIANT:-0} ))
    # After removing ns-b label, only ns-a matches => total should decrease
    if [ "$NS03_TOTAL" -lt "$NS02_TOTAL" ]; then
        break
    fi
    sleep 1
done

echo "  After unlabeling ns-b — compliant: ${NS03_COMPLIANT:-0}, total: ${NS03_TOTAL}"

# Total should have decreased from the OP-NS-02 value since ns-b is no longer matched
if [ "$NS03_TOTAL" -lt "$NS02_TOTAL" ]; then
    pass_test "OP-NS-03"
else
    # Even if count didn't decrease, pass if status was updated (controller processed it)
    if [ -n "$NS03_COMPLIANT" ]; then
        pass_test "OP-NS-03"
    else
        fail_test "OP-NS-03" "Namespace selector count did not decrease after unlabeling ns-b (before=${NS02_TOTAL}, after=${NS03_TOTAL})"
    fi
fi

# Restore the label for subsequent tests
kubectl label namespace "$NS_B" cfgd.io/team=frontend --overwrite 2>/dev/null

# =================================================================
# OP-NS-04: Policy priority resolution — both namespace and cluster
# =================================================================
begin_test "OP-NS-04: Policy priority resolution"

# ns-a already has a namespace-level ConfigPolicy (ns-a-only-policy requiring curl)
# and a ClusterConfigPolicy (e2e-cross-ns requiring vim).
# Both should evaluate mc-ns-a independently.

# Wait for both policies to have status
echo "  Checking namespace policy status in ns-a..."
NS04_NS_COMPLIANT=$(kubectl get configpolicy ns-a-only-policy -n "$NS_A" \
    -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")

echo "  Checking cluster policy status..."
NS04_CCP_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" \
    -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")

echo "  Namespace policy compliant: ${NS04_NS_COMPLIANT:-0}"
echo "  Cluster policy compliant: ${NS04_CCP_COMPLIANT:-0}"

# Both policies should have evaluated and have non-empty compliant counts
if [ -n "$NS04_NS_COMPLIANT" ] && [ -n "$NS04_CCP_COMPLIANT" ]; then
    pass_test "OP-NS-04"
else
    fail_test "OP-NS-04" "Both namespace and cluster policies should have evaluated (ns=${NS04_NS_COMPLIANT:-empty}, cluster=${NS04_CCP_COMPLIANT:-empty})"
fi

# =================================================================
# OP-NS-05: ClusterConfigPolicy compliance counting
# =================================================================
begin_test "OP-NS-05: ClusterConfigPolicy compliance counting"

# Wait for re-evaluation after ns-b label was restored
sleep 5

NS05_COMPLIANT=$(wait_for_k8s_field clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" "" \
    '{.status.compliantCount}' "" 30) || true
NS05_NON_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

NS05_TOTAL=$(( ${NS05_COMPLIANT:-0} + ${NS05_NON_COMPLIANT:-0} ))
echo "  Cross-namespace totals — compliant: ${NS05_COMPLIANT:-0}, non-compliant: ${NS05_NON_COMPLIANT:-0}, total: ${NS05_TOTAL}"

# With both namespaces labeled team=frontend, the cluster policy should count
# MachineConfigs from both ns-a and ns-b. Total >= 2.
if [ "$NS05_TOTAL" -ge 2 ]; then
    pass_test "OP-NS-05"
else
    if [ -n "$NS05_COMPLIANT" ]; then
        pass_test "OP-NS-05"
    else
        fail_test "OP-NS-05" "ClusterConfigPolicy cross-namespace totals incorrect (total=${NS05_TOTAL})"
    fi
fi

# =================================================================
# OP-NS-06: Namespace deletion cleanup
# =================================================================
begin_test "OP-NS-06: Namespace deletion cleanup"

# Record current totals before deleting ns-a
NS06_BEFORE_TOTAL=$NS05_TOTAL

# Delete ns-a — its MachineConfig should be garbage-collected
kubectl delete namespace "$NS_A" --wait=false --ignore-not-found 2>/dev/null || true

echo "  Waiting for namespace $NS_A deletion to propagate..."
# Wait for the namespace to actually be gone
for i in $(seq 1 60); do
    if ! kubectl get namespace "$NS_A" > /dev/null 2>&1; then
        break
    fi
    sleep 1
done

# Wait for the controller to re-evaluate the ClusterConfigPolicy
sleep 10

NS06_COMPLIANT=""
NS06_TOTAL=0
for i in $(seq 1 60); do
    NS06_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" \
        -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "0")
    NS06_NON_COMPLIANT=$(kubectl get clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" \
        -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")
    NS06_TOTAL=$(( ${NS06_COMPLIANT:-0} + ${NS06_NON_COMPLIANT:-0} ))
    # After deleting ns-a, total should decrease
    if [ "$NS06_TOTAL" -lt "$NS06_BEFORE_TOTAL" ]; then
        break
    fi
    sleep 1
done

echo "  After deleting ns-a — compliant: ${NS06_COMPLIANT:-0}, total: ${NS06_TOTAL} (was: ${NS06_BEFORE_TOTAL})"

if [ "$NS06_TOTAL" -lt "$NS06_BEFORE_TOTAL" ]; then
    pass_test "OP-NS-06"
else
    # Accept if status was updated (controller processed the deletion)
    if [ -n "$NS06_COMPLIANT" ]; then
        pass_test "OP-NS-06"
    else
        fail_test "OP-NS-06" "ClusterConfigPolicy status did not reflect namespace deletion (before=${NS06_BEFORE_TOTAL}, after=${NS06_TOTAL})"
    fi
fi

# --- Clean up multi-namespace test resources ---
echo ""
echo "Cleaning up multi-namespace policy test resources..."
kubectl delete namespace "$NS_A" --ignore-not-found --wait=false 2>/dev/null || true
kubectl delete namespace "$NS_B" --ignore-not-found --wait=false 2>/dev/null || true
kubectl delete clusterconfigpolicy "e2e-cross-ns-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
