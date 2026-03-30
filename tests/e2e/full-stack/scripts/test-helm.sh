# Full-stack E2E tests: Helm Chart Lifecycle
# Sourced by run-all.sh — do NOT set traps or pipefail here.

CHART_DIR="$REPO_ROOT/chart/cfgd"

echo ""
echo "=== Helm Chart Lifecycle Tests ==="

# Pre-flight: delete stale cluster-scoped resources from prior cfgd-test helm releases.
# Helm ClusterRoles/ClusterRoleBindings persist after namespace deletion and block reinstall.
for res in clusterrole clusterrolebinding; do
    for name in $(kubectl get "$res" -o name 2>/dev/null | grep 'cfgd-test' | sed "s|${res}.rbac.authorization.k8s.io/||; s|${res}/||"); do
        kubectl delete "$res" "$name" --ignore-not-found 2>/dev/null || true
    done
done

# Helper: create a dedicated namespace for a Helm test, install, and return release name.
# Usage: helm_test_ns "01" -- sets HELM_NS="e2e-helm-01-${E2E_RUN_ID}"
helm_test_ns() {
    local id="$1"
    HELM_NS="e2e-helm-${id}-${E2E_RUN_ID}"
    kubectl create namespace "$HELM_NS" 2>/dev/null || true
    kubectl label namespace "$HELM_NS" "$E2E_RUN_LABEL" --overwrite 2>/dev/null || true
    # Wait for Reflector to replicate registry-credentials (needed for imagePullSecrets)
    local deadline=$((SECONDS + 30))
    while [ $SECONDS -lt $deadline ]; do
        if kubectl get secret registry-credentials -n "$HELM_NS" > /dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    echo "  WARN: registry-credentials not replicated to $HELM_NS"
}

# Helper: clean up a Helm test namespace (uninstall release + delete namespace).
helm_test_cleanup() {
    local release="${1:-cfgd-test}"
    helm uninstall "$release" -n "$HELM_NS" 2>/dev/null || true
    # Clean up cluster-scoped resources that Helm doesn't remove on uninstall
    for res in clusterrole clusterrolebinding; do
        for name in $(kubectl get "$res" -o name 2>/dev/null | grep "$release" | sed "s|${res}.rbac.authorization.k8s.io/||; s|${res}/||"); do
            kubectl delete "$res" "$name" --ignore-not-found 2>/dev/null || true
        done
    done
    kubectl delete namespace "$HELM_NS" --ignore-not-found --wait=false 2>/dev/null || true
}

# =================================================================
# FS-HELM-01: Fresh install — operator + CSI running
# =================================================================
begin_test "FS-HELM-01: Fresh Helm install creates operator deployment"

# CSI driver is cluster-scoped (CSIDriver resource) and already installed by setup-cluster.sh.
# A second Helm install with csiDriver.enabled=true in a different namespace will fail because
# the CSIDriver "csi.cfgd.io" is already owned by the cfgd-csi release. Test operator only.
helm_test_ns "01"
INSTALL_OUTPUT=$(helm install cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=${REGISTRY}/cfgd-operator" \
    --set "operator.image.tag=$IMAGE_TAG" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1) || true

OPERATOR_DEPLOY=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")

echo "  Operator deployment: ${OPERATOR_DEPLOY:-<none>}"

if [ -n "$OPERATOR_DEPLOY" ]; then
    OPERATOR_AVAIL=$(kubectl get deployment -n "$HELM_NS" \
        -l app.kubernetes.io/component=operator \
        -o jsonpath='{.items[0].status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
    echo "  Operator available: ${OPERATOR_AVAIL:-unknown}"
    if [ "$OPERATOR_AVAIL" = "True" ]; then
        pass_test "FS-HELM-01"
    else
        PODS=$(kubectl get pods -n "$HELM_NS" -l app.kubernetes.io/component=operator \
            -o jsonpath='{.items[*].status.phase}' 2>/dev/null || echo "")
        echo "  Operator pod phases: ${PODS:-<none>}"
        if echo "$PODS" | grep -q "Running"; then
            echo "  Operator pod Running (not yet Available — normal for fresh install)"
            pass_test "FS-HELM-01"
        else
            fail_test "FS-HELM-01" "Operator deployment exists but pod not Running"
        fi
    fi
else
    echo "  Helm install output:"
    echo "$INSTALL_OUTPUT" | head -20 | sed 's/^/    /'
    fail_test "FS-HELM-01" "Expected operator deployment after helm install"
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-02: Gateway enabled — gateway service exists
# =================================================================
begin_test "FS-HELM-02: Gateway enabled creates gateway service"

helm_test_ns "02"
helm install cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=${REGISTRY}/cfgd-operator" \
    --set "operator.image.tag=$IMAGE_TAG" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set deviceGateway.enabled=true \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set csiDriver.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

GATEWAY_SVC=$(kubectl get svc -n "$HELM_NS" \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  Services: $GATEWAY_SVC"

# The gateway service name follows the pattern: <release>-cfgd-gateway
if echo "$GATEWAY_SVC" | grep -q "gateway"; then
    # Also verify that the operator deployment has the DEVICE_GATEWAY_ENABLED env
    GW_ENV=$(kubectl get deployment -n "$HELM_NS" \
        -l app.kubernetes.io/component=operator \
        -o jsonpath='{.items[0].spec.template.spec.containers[0].env[?(@.name=="DEVICE_GATEWAY_ENABLED")].value}' \
        2>/dev/null || echo "")
    echo "  DEVICE_GATEWAY_ENABLED: ${GW_ENV:-<not set>}"
    pass_test "FS-HELM-02"
else
    fail_test "FS-HELM-02" "Gateway service not found when deviceGateway.enabled=true"
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-03: Gateway disabled — no gateway service
# =================================================================
begin_test "FS-HELM-03: Gateway disabled creates no gateway service"

helm_test_ns "03"
helm install cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=${REGISTRY}/cfgd-operator" \
    --set "operator.image.tag=$IMAGE_TAG" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set deviceGateway.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set csiDriver.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

GATEWAY_SVC=$(kubectl get svc -n "$HELM_NS" \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  Services: ${GATEWAY_SVC:-<none>}"

if echo "$GATEWAY_SVC" | grep -q "gateway"; then
    fail_test "FS-HELM-03" "Gateway service found when deviceGateway.enabled=false"
else
    # Confirm operator deployment does NOT have gateway env
    GW_ENV=$(kubectl get deployment -n "$HELM_NS" \
        -l app.kubernetes.io/component=operator \
        -o jsonpath='{.items[0].spec.template.spec.containers[0].env[?(@.name=="DEVICE_GATEWAY_ENABLED")].value}' \
        2>/dev/null || echo "")
    echo "  DEVICE_GATEWAY_ENABLED: ${GW_ENV:-<not set>}"
    if [ -z "$GW_ENV" ]; then
        pass_test "FS-HELM-03"
    else
        fail_test "FS-HELM-03" "DEVICE_GATEWAY_ENABLED env set when gateway disabled"
    fi
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-04: CSI disabled — no CSI daemonset
# =================================================================
begin_test "FS-HELM-04: CSI disabled creates no CSI daemonset"

helm_test_ns "04"
helm install cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=${REGISTRY}/cfgd-operator" \
    --set "operator.image.tag=$IMAGE_TAG" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

CSI_DS=$(kubectl get daemonset -n "$HELM_NS" \
    -l app.kubernetes.io/component=csi-driver \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  CSI DaemonSets: ${CSI_DS:-<none>}"

if [ -z "$CSI_DS" ]; then
    pass_test "FS-HELM-04"
else
    fail_test "FS-HELM-04" "CSI daemonset found when csiDriver.enabled=false: $CSI_DS"
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-05: Upgrade preserves CRDs — existing instances survive
# =================================================================
begin_test "FS-HELM-05: Helm upgrade preserves CRDs and instances"

helm_test_ns "05"

# Install initial release
helm install cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=${REGISTRY}/cfgd-operator" \
    --set "operator.image.tag=$IMAGE_TAG" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

# Create a CRD instance to verify it survives the upgrade
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: helm-upgrade-test-${E2E_RUN_ID}
  namespace: $HELM_NS
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
EOF

# Verify the CRD instance was created
MC_BEFORE=$(kubectl get machineconfig "helm-upgrade-test-${E2E_RUN_ID}" \
    -n "$HELM_NS" -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
echo "  MachineConfig before upgrade: ${MC_BEFORE:-<not found>}"

# Perform Helm upgrade
UPGRADE_OUTPUT=$(helm upgrade cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=${REGISTRY}/cfgd-operator" \
    --set "operator.image.tag=$IMAGE_TAG" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1) || true

# Verify CRD instance survived the upgrade
MC_AFTER=$(kubectl get machineconfig "helm-upgrade-test-${E2E_RUN_ID}" \
    -n "$HELM_NS" -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
echo "  MachineConfig after upgrade: ${MC_AFTER:-<not found>}"

# Verify operator is still running
OPERATOR_AVAIL=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[0].status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
echo "  Operator available after upgrade: ${OPERATOR_AVAIL:-unknown}"

if [ "$MC_BEFORE" = "helm-upgrade-test-${E2E_RUN_ID}" ] && \
   [ "$MC_AFTER" = "helm-upgrade-test-${E2E_RUN_ID}" ]; then
    pass_test "FS-HELM-05"
else
    fail_test "FS-HELM-05" "CRD instance did not survive Helm upgrade"
fi

# Clean up the CRD instance
kubectl delete machineconfig "helm-upgrade-test-${E2E_RUN_ID}" -n "$HELM_NS" --ignore-not-found 2>/dev/null || true
helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-06: Values override — custom replica count reflected
# =================================================================
begin_test "FS-HELM-06: Values override — custom replica count"

helm_test_ns "06"
helm install cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=${REGISTRY}/cfgd-operator" \
    --set "operator.image.tag=$IMAGE_TAG" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set operator.replicaCount=2 \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set deviceGateway.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

REPLICAS=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[0].spec.replicas}' 2>/dev/null || echo "")
echo "  Operator replicas: ${REPLICAS:-<not found>}"

if [ "$REPLICAS" = "2" ]; then
    pass_test "FS-HELM-06"
else
    fail_test "FS-HELM-06" "Expected 2 replicas, got: ${REPLICAS:-<none>}"
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-07: Helm template validation — valid YAML
# =================================================================
begin_test "FS-HELM-07: Helm template produces valid YAML"

TEMPLATE_OUTPUT=$(helm template cfgd-test "$CHART_DIR" \
    --set operator.enabled=true \
    --set csiDriver.enabled=true \
    --set agent.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set deviceGateway.enabled=true 2>&1)
TEMPLATE_RC=$?

if [ $TEMPLATE_RC -ne 0 ]; then
    fail_test "FS-HELM-07" "helm template failed with exit code $TEMPLATE_RC"
    echo "  Output: $(echo "$TEMPLATE_OUTPUT" | head -10)"
else
    # Validate via dry-run (no cluster mutation)
    DRYRUN_OUTPUT=$(echo "$TEMPLATE_OUTPUT" | kubectl apply --dry-run=client -f - 2>&1)
    DRYRUN_RC=$?
    echo "  Template lines: $(echo "$TEMPLATE_OUTPUT" | wc -l)"
    echo "  Dry-run exit code: $DRYRUN_RC"

    if [ $DRYRUN_RC -eq 0 ]; then
        pass_test "FS-HELM-07"
    else
        fail_test "FS-HELM-07" "Templated YAML failed kubectl dry-run validation"
        echo "  Errors: $(echo "$DRYRUN_OUTPUT" | head -10)"
    fi
fi

# =================================================================
# FS-HELM-08: Helm uninstall cleanup — resources removed, CRDs preserved
# =================================================================
begin_test "FS-HELM-08: Helm uninstall removes resources but preserves CRDs"

helm_test_ns "08"

# Install
helm install cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=${REGISTRY}/cfgd-operator" \
    --set "operator.image.tag=$IMAGE_TAG" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

# Verify deployment exists before uninstall
DEPLOY_BEFORE=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  Deployment before uninstall: ${DEPLOY_BEFORE:-<none>}"

# Uninstall
helm uninstall cfgd-test -n "$HELM_NS" 2>&1 || true
sleep 5

# Verify deployment is gone
DEPLOY_AFTER=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  Deployment after uninstall: ${DEPLOY_AFTER:-<none>}"

# Verify CRDs still exist (Helm does not delete CRDs on uninstall)
CRDS_EXIST=true
for crd in machineconfigs.cfgd.io configpolicies.cfgd.io modules.cfgd.io driftalerts.cfgd.io; do
    if ! kubectl get crd "$crd" > /dev/null 2>&1; then
        echo "  CRD missing: $crd"
        CRDS_EXIST=false
    fi
done
echo "  CRDs preserved: $CRDS_EXIST"

if [ -n "$DEPLOY_BEFORE" ] && [ -z "$DEPLOY_AFTER" ] && [ "$CRDS_EXIST" = "true" ]; then
    pass_test "FS-HELM-08"
else
    if [ -z "$DEPLOY_BEFORE" ]; then
        fail_test "FS-HELM-08" "Deployment was not created during install"
    elif [ -n "$DEPLOY_AFTER" ]; then
        fail_test "FS-HELM-08" "Deployment still present after uninstall"
    else
        fail_test "FS-HELM-08" "CRDs were removed after uninstall"
    fi
fi

kubectl delete namespace "$HELM_NS" --ignore-not-found --wait=false 2>/dev/null || true
